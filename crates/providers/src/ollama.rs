//! Ollama's native API.
//!
//! This uses `/api/chat` rather than the OpenAI-compatible endpoint, because measurements on
//! Ollama 0.40.14 showed that endpoint ignores `num_ctx` and reloads the model at the server's
//! default. See `docs/spikes/2026-11-06-ollama.md`.

use crate::ndjson::LineDecoder;
use crate::{ChatRequest, FinishReason, Health, ModelInfo, Provider, ProviderError, ProviderOptions, StreamEvent};
use async_trait::async_trait;
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use recode_core::capability::{Capability, ModelCapabilities};
use recode_core::message::{Message, Role, ToolCall};
use recode_core::usage::ReportedUsage;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// The address Ollama listens on by default.
pub const DEFAULT_URL: &str = "http://localhost:11434";

/// Metadata requests answer quickly. Chat requests have no overall limit, because loading a large
/// model and evaluating a long prompt can legitimately take minutes with no bytes sent.
const METADATA_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// Controls specific to Ollama.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OllamaOptions {
    /// Context size. It should be sent every time and kept stable within a session: over-limit
    /// prompts are silently truncated, and changing the value reloads the model.
    pub num_ctx: Option<u32>,
    /// Maximum tokens to generate.
    pub num_predict: Option<u32>,
    /// How long to keep the model loaded, such as `5m`.
    pub keep_alive: Option<String>,
    /// Thinking setting. Leave unset unless a model's profile says otherwise: `think: false`
    /// was measured putting reasoning into the content on at least one model.
    pub think: Option<bool>,
}

/// A loaded model, as reported by `/api/ps`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LoadedModel {
    pub name: String,
    pub size_bytes: u64,
    pub size_vram_bytes: u64,
    pub context_length: Option<u64>,
}

/// A connection to an Ollama server.
#[derive(Debug, Clone)]
pub struct OllamaProvider {
    base_url: String,
    client: reqwest::Client,
}

impl OllamaProvider {
    /// Creates a client for the given base URL. This does not contact the server.
    pub fn new(base_url: &str) -> Result<Self, ProviderError> {
        let base_url = base_url.trim_end_matches('/').to_owned();
        let client = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .map_err(|error| ProviderError::Unreachable { url: base_url.clone(), reason: error.to_string() })?;
        Ok(Self { base_url, client })
    }

    /// The server's base URL.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Lists the models currently in memory.
    pub async fn loaded_models(&self) -> Result<Vec<LoadedModel>, ProviderError> {
        let response: PsResponse = self.get_json("/api/ps").await?;
        Ok(response
            .models
            .into_iter()
            .map(|model| LoadedModel {
                name: model.name,
                size_bytes: model.size,
                size_vram_bytes: model.size_vram,
                context_length: model.context_length,
            })
            .collect())
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, ProviderError> {
        let url = self.url(path);
        let response = self.client.get(&url).timeout(METADATA_TIMEOUT).send().await;
        decode_json(&url, response).await
    }
}

#[async_trait]
impl Provider for OllamaProvider {
    async fn health(&self) -> Result<Health, ProviderError> {
        let response: VersionResponse = self.get_json("/api/version").await?;
        Ok(Health { version: response.version })
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        let response: TagsResponse = self.get_json("/api/tags").await?;
        Ok(response
            .models
            .into_iter()
            .map(|model| ModelInfo {
                name: model.name,
                size_bytes: model.size,
                family: model.details.family,
                parameter_size: model.details.parameter_size,
                quantization: model.details.quantization_level,
                facts: recode_core::model_facts::ModelFacts::default(),
            })
            .collect())
    }

    async fn capabilities(&self, model: &str) -> Result<ModelCapabilities, ProviderError> {
        let url = self.url("/api/show");
        let response = self.client.post(&url).json(&json!({"model": model})).timeout(METADATA_TIMEOUT).send().await;
        let show: ShowResponse = decode_json(&url, response).await?;
        Ok(capabilities_from(&show))
    }

    fn stream(
        &self,
        request: ChatRequest,
        cancel: CancellationToken,
    ) -> BoxStream<'static, Result<StreamEvent, ProviderError>> {
        let client = self.client.clone();
        let url = self.url("/api/chat");
        let body = request_body(&request);
        let stream = async_stream::stream! {
            let send = client.post(&url).json(&body).send();
            let response = tokio::select! {
                biased;
                () = cancel.cancelled() => {
                    yield Ok(StreamEvent::Finished(FinishReason::Cancelled));
                    return;
                }
                response = send => response,
            };
            let response = match check_status(&url, response).await {
                Ok(response) => response,
                Err(error) => {
                    yield Err(error);
                    return;
                }
            };

            let mut bytes = response.bytes_stream();
            let mut decoder = LineDecoder::default();
            let mut finished = false;
            loop {
                // Dropping the response stream closes the connection, which tells Ollama to stop.
                let next = tokio::select! {
                    biased;
                    () = cancel.cancelled() => {
                        yield Ok(StreamEvent::Finished(FinishReason::Cancelled));
                        return;
                    }
                    next = bytes.next() => next,
                };
                let body_ended = next.is_none();
                let lines = match next {
                    Some(Ok(chunk)) => decoder.push(&chunk),
                    Some(Err(error)) => {
                        yield Err(ProviderError::Unreachable { url: url.clone(), reason: error.to_string() });
                        return;
                    }
                    None => decoder.finish().into_iter().collect(),
                };
                for line in lines {
                    match events_from_line(&line) {
                        Ok((events, done)) => {
                            finished |= done;
                            for event in events {
                                yield Ok(event);
                            }
                        }
                        Err(error) => {
                            yield Err(error);
                            return;
                        }
                    }
                }
                if finished {
                    return;
                }
                if body_ended {
                    yield Err(ProviderError::EndedEarly);
                    return;
                }
            }
        };
        stream.boxed()
    }
}

fn request_body(request: &ChatRequest) -> Value {
    let mut body = json!({
        "model": request.model,
        "stream": true,
        "messages": request.messages.iter().map(wire_message).collect::<Vec<_>>(),
    });
    if !request.tools.is_empty() {
        body["tools"] = request
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "function": {"name": tool.name, "description": tool.description, "parameters": tool.parameters},
                })
            })
            .collect();
    }
    if let ProviderOptions::Ollama(options) = &request.options {
        let mut model_options = serde_json::Map::new();
        if let Some(num_ctx) = options.num_ctx {
            model_options.insert("num_ctx".into(), num_ctx.into());
        }
        if let Some(num_predict) = options.num_predict {
            model_options.insert("num_predict".into(), num_predict.into());
        }
        if !model_options.is_empty() {
            body["options"] = model_options.into();
        }
        if let Some(keep_alive) = &options.keep_alive {
            body["keep_alive"] = keep_alive.clone().into();
        }
        if let Some(think) = options.think {
            body["think"] = think.into();
        }
    }
    body
}

fn wire_message(message: &Message) -> Value {
    let role = match message.role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    };
    // Reasoning is deliberately not re-sent: it costs context and Ollama doesn't need it back.
    let mut wire = json!({"role": role, "content": message.content});
    if !message.images.is_empty() {
        // Ollama takes images as an array of base64 strings on the message.
        wire["images"] = message.images.iter().map(|image| image.data_base64.clone()).collect();
    }
    if !message.tool_calls.is_empty() {
        wire["tool_calls"] = message
            .tool_calls
            .iter()
            .map(|call| json!({"id": call.id, "function": {"name": call.name, "arguments": call.arguments}}))
            .collect();
    }
    if let Some(name) = &message.tool_name {
        wire["tool_name"] = name.clone().into();
    }
    if let Some(id) = &message.tool_call_id {
        wire["tool_call_id"] = id.clone().into();
    }
    wire
}

/// Converts one response line into events, and reports whether it was the final one.
fn events_from_line(line: &[u8]) -> Result<(Vec<StreamEvent>, bool), ProviderError> {
    let chunk: ChatChunk = serde_json::from_slice(line)
        .map_err(|error| ProviderError::Decode(format!("{error}: {}", String::from_utf8_lossy(line))))?;
    if let Some(error) = chunk.error {
        return Err(ProviderError::Server(error));
    }
    let mut events = Vec::new();
    if let Some(message) = chunk.message {
        if let Some(thinking) = message.thinking.filter(|text| !text.is_empty()) {
            events.push(StreamEvent::ReasoningDelta(thinking));
        }
        if !message.content.is_empty() {
            events.push(StreamEvent::TextDelta(message.content));
        }
        events.extend(message.tool_calls.into_iter().map(|call| StreamEvent::ToolCall(call.into())));
    }
    if chunk.done {
        events.push(StreamEvent::Usage(ReportedUsage {
            prompt_tokens: chunk.prompt_eval_count,
            cached_prompt_tokens: chunk.prompt_eval_cached_count,
            output_tokens: chunk.eval_count,
            load_ms: chunk.load_duration.map(nanos_to_millis),
            prompt_eval_ms: chunk.prompt_eval_duration.map(nanos_to_millis),
            output_ms: chunk.eval_duration.map(nanos_to_millis),
        }));
        events.push(StreamEvent::Finished(match chunk.done_reason.as_deref() {
            None | Some("stop") => FinishReason::Stop,
            Some("length") => FinishReason::Length,
            Some(other) => FinishReason::Other(other.to_owned()),
        }));
    }
    Ok((events, chunk.done))
}

const fn nanos_to_millis(nanos: u64) -> u64 {
    nanos / 1_000_000
}

fn capabilities_from(show: &ShowResponse) -> ModelCapabilities {
    let declared =
        |name: &str| show.capabilities.as_ref().map(|list| Capability::declared(list.iter().any(|item| item == name)));
    let info = |suffix: &str| {
        show.model_info.iter().find(|(key, _)| key.ends_with(suffix)).and_then(|(_, value)| value.as_u64())
    };
    let context_limit = info(".context_length").map(Capability::declared);
    ModelCapabilities {
        completion: declared("completion"),
        tools: declared("tools"),
        vision: declared("vision"),
        thinking: declared("thinking"),
        context_limit,
        kv_bytes_per_token: kv_bytes_per_token(&info).map(Capability::declared),
    }
}

/// Key/value cache per token from the GGUF metadata: every layer stores a key and a value per
/// KV head, each `key_length`/`value_length` wide (or the head size, `embedding / heads`), at two bytes
/// per entry for a 16-bit cache. llama3.2:3b: 28 layers × 8 KV heads × (128 + 128) × 2 = 114,688 bytes.
fn kv_bytes_per_token(info: &impl Fn(&str) -> Option<u64>) -> Option<u64> {
    let layers = info(".block_count")?;
    let heads = info(".attention.head_count")?;
    let kv_heads = info(".attention.head_count_kv").unwrap_or(heads);
    let head_size = info(".embedding_length").map(|embedding| embedding / heads.max(1));
    let key = info(".attention.key_length").or(head_size)?;
    let value = info(".attention.value_length").or(head_size)?;
    Some(layers * kv_heads * (key + value) * 2)
}

async fn check_status(
    url: &str,
    response: reqwest::Result<reqwest::Response>,
) -> Result<reqwest::Response, ProviderError> {
    let response =
        response.map_err(|error| ProviderError::Unreachable { url: url.to_owned(), reason: root_cause(&error) })?;
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let text = response.text().await.unwrap_or_default();
    let message = serde_json::from_str::<ErrorBody>(&text).map_or(text, |body| body.error);
    Err(ProviderError::Http { url: url.to_owned(), status: status.as_u16(), message })
}

async fn decode_json<T: serde::de::DeserializeOwned>(
    url: &str,
    response: reqwest::Result<reqwest::Response>,
) -> Result<T, ProviderError> {
    let response = check_status(url, response).await?;
    let body = response
        .bytes()
        .await
        .map_err(|error| ProviderError::Unreachable { url: url.to_owned(), reason: root_cause(&error) })?;
    serde_json::from_slice(&body).map_err(|error| ProviderError::Decode(format!("{url}: {error}")))
}

/// Finds the most specific cause, since the top-level message from reqwest is rarely useful.
fn root_cause(error: &(dyn std::error::Error + 'static)) -> String {
    let mut current = error;
    while let Some(next) = current.source() {
        current = next;
    }
    current.to_string()
}

#[derive(Deserialize)]
struct ErrorBody {
    error: String,
}

#[derive(Deserialize)]
struct VersionResponse {
    version: String,
}

#[derive(Deserialize)]
struct TagsResponse {
    #[serde(default)]
    models: Vec<WireModel>,
}

#[derive(Deserialize)]
struct WireModel {
    name: String,
    size: Option<u64>,
    #[serde(default)]
    details: WireDetails,
}

#[derive(Default, Deserialize)]
struct WireDetails {
    family: Option<String>,
    parameter_size: Option<String>,
    quantization_level: Option<String>,
}

#[derive(Deserialize)]
struct PsResponse {
    #[serde(default)]
    models: Vec<WireLoaded>,
}

#[derive(Deserialize)]
struct WireLoaded {
    name: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    size_vram: u64,
    context_length: Option<u64>,
}

#[derive(Deserialize)]
struct ShowResponse {
    capabilities: Option<Vec<String>>,
    #[serde(default)]
    model_info: serde_json::Map<String, Value>,
}

#[derive(Deserialize)]
struct ChatChunk {
    message: Option<WireMessage>,
    #[serde(default)]
    done: bool,
    done_reason: Option<String>,
    prompt_eval_count: Option<u64>,
    prompt_eval_cached_count: Option<u64>,
    eval_count: Option<u64>,
    load_duration: Option<u64>,
    prompt_eval_duration: Option<u64>,
    eval_duration: Option<u64>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct WireMessage {
    #[serde(default)]
    content: String,
    thinking: Option<String>,
    #[serde(default)]
    tool_calls: Vec<WireToolCall>,
}

#[derive(Deserialize)]
struct WireToolCall {
    id: Option<String>,
    function: WireFunction,
}

#[derive(Deserialize)]
struct WireFunction {
    name: String,
    #[serde(default)]
    arguments: Value,
}

impl From<WireToolCall> for ToolCall {
    fn from(call: WireToolCall) -> Self {
        // Some models encode arguments as a JSON string. If it doesn't parse, it is passed on
        // unchanged so that validation can report the problem back to the model.
        let arguments = match call.function.arguments {
            Value::String(text) => serde_json::from_str(&text).unwrap_or(Value::String(text)),
            other => other,
        };
        Self {
            id: call.id.unwrap_or_else(|| format!("call_{}", uuid::Uuid::now_v7().simple())),
            name: call.function.name,
            arguments,
            extra: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use recode_core::message::{ImageData, ToolDefinition};

    #[test]
    fn body_sends_context_size_and_leaves_think_unset_by_default() {
        let request = ChatRequest {
            model: "m".into(),
            messages: vec![Message::user("hi")],
            tools: vec![],
            options: ProviderOptions::Ollama(OllamaOptions { num_ctx: Some(16384), ..OllamaOptions::default() }),
        };
        let body = request_body(&request);
        assert_eq!(body["options"]["num_ctx"], 16384);
        assert!(body.get("think").is_none());
        assert!(body.get("tools").is_none());
    }

    #[test]
    fn body_encodes_tools_and_a_full_tool_exchange() {
        let call = ToolCall { id: "c1".into(), name: "read".into(), arguments: json!({"path": "a"}), extra: None };
        let mut assistant = Message::assistant("", vec![call.clone()]);
        assistant.reasoning = Some("private reasoning".into());
        let request = ChatRequest {
            model: "m".into(),
            messages: vec![assistant, Message::tool_result(&call, "contents")],
            tools: vec![ToolDefinition {
                name: "read".into(),
                description: "d".into(),
                parameters: json!({"type": "object"}),
            }],
            options: ProviderOptions::None,
        };
        let body = request_body(&request);
        assert_eq!(body["tools"][0]["function"]["name"], "read");
        assert_eq!(body["messages"][0]["tool_calls"][0]["function"]["arguments"]["path"], "a");
        assert_eq!(body["messages"][1]["role"], "tool");
        assert_eq!(body["messages"][1]["tool_name"], "read");
        assert!(!body.to_string().contains("private reasoning"));
    }

    #[test]
    fn a_message_with_an_image_sends_the_images_array() {
        let image = ImageData { media_type: "image/png".into(), data_base64: "AAAA".into(), label: "x".into() };
        let request = ChatRequest {
            model: "m".into(),
            messages: vec![Message::user_with_images("look", vec![image])],
            tools: vec![],
            options: ProviderOptions::None,
        };
        let body = request_body(&request);
        assert_eq!(body["messages"][0]["images"][0], "AAAA");
        assert_eq!(body["messages"][0]["content"], "look");
    }

    #[test]
    fn string_arguments_are_parsed_and_bad_ones_are_kept_for_validation() {
        let call = |arguments: Value| {
            ToolCall::from(WireToolCall { id: None, function: WireFunction { name: "t".into(), arguments } })
        };
        assert_eq!(call(json!("{\"a\":1}")).arguments, json!({"a": 1}));
        assert_eq!(call(json!("{broken")).arguments, json!("{broken"));
        assert!(call(json!({})).id.starts_with("call_"));
    }

    #[test]
    fn missing_capabilities_mean_unknown_not_unsupported() {
        let show: ShowResponse = serde_json::from_value(json!({"model_info": {}})).unwrap();
        assert_eq!(capabilities_from(&show).tools, None);
        assert_eq!(capabilities_from(&show).kv_bytes_per_token, None);
    }

    #[test]
    fn the_kv_cache_per_token_follows_the_layer_and_head_counts() {
        let show: ShowResponse = serde_json::from_value(json!({"model_info": {
            "llama.block_count": 28, "llama.attention.head_count": 24, "llama.attention.head_count_kv": 8,
            "llama.attention.key_length": 128, "llama.attention.value_length": 128, "llama.embedding_length": 3072
        }}))
        .unwrap();
        assert_eq!(capabilities_from(&show).kv_bytes_per_token.unwrap().value, 114_688);
        // Without explicit key/value lengths the head size stands in; without KV heads, every head.
        let plain: ShowResponse = serde_json::from_value(json!({"model_info": {
            "qwen3.block_count": 36, "qwen3.attention.head_count": 32, "qwen3.embedding_length": 4096
        }}))
        .unwrap();
        assert_eq!(capabilities_from(&plain).kv_bytes_per_token.unwrap().value, 36 * 32 * 256 * 2);
    }
}
