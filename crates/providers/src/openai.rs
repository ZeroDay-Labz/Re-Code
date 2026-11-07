//! An adapter for any server speaking the OpenAI Chat Completions API: OpenAI itself, Google Gemini
//! through its OpenAI-compatible endpoint, OpenRouter (including its free models), and local servers
//! such as LM Studio. Tool calling (schemas, streamed tool-call parsing, and the message round-trip) is supported.
//! OpenRouter gets two extras: an `X-Title` header naming this app, and its key's own free-allowance
//! figures appended when it rate-limits (`openrouter`).

use crate::http::{check, decode_json, root_cause};
use crate::sse::SseDecoder;
use crate::{ChatRequest, FinishReason, Health, ModelInfo, Provider, ProviderError, StreamEvent};
use async_trait::async_trait;
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use recode_core::capability::ModelCapabilities;
use recode_core::message::{Role, ToolCall};
use recode_core::usage::ReportedUsage;
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const METADATA_TIMEOUT: Duration = Duration::from_secs(20);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Chat has no overall limit because a long answer legitimately takes a while; the per-read watchdog
/// lives in the engine. This is the default generation cap, which a provider may lower.
const DEFAULT_MAX_TOKENS: u32 = 4096;

/// A connection to an OpenAI-compatible server.
#[derive(Debug, Clone)]
pub struct OpenAiProvider {
    base_url: String,
    api_key: Option<String>,
    client: reqwest::Client,
}

impl OpenAiProvider {
    /// Creates a client. An empty key is treated as no key, for local servers that need none.
    pub fn new(base_url: &str, api_key: &str) -> Result<Self, ProviderError> {
        let base_url = base_url.trim_end_matches('/').to_owned();
        let mut builder = reqwest::Client::builder().connect_timeout(CONNECT_TIMEOUT);
        if crate::openrouter::is_openrouter(&base_url) {
            // OpenRouter shows the calling app in its usage views when told who's calling. Only the
            // app's name goes out; nothing about the user or the machine.
            let mut headers = reqwest::header::HeaderMap::new();
            headers.insert("X-Title", reqwest::header::HeaderValue::from_static("re-code"));
            builder = builder.default_headers(headers);
        }
        let client = builder
            .build()
            .map_err(|error| ProviderError::Unreachable { url: base_url.clone(), reason: error.to_string() })?;
        let api_key = (!api_key.trim().is_empty()).then(|| api_key.trim().to_owned());
        Ok(Self { base_url, api_key, client })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }

    fn authed(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.api_key {
            Some(key) => builder.bearer_auth(key),
            None => builder,
        }
    }
}

#[async_trait]
impl Provider for OpenAiProvider {
    async fn health(&self) -> Result<Health, ProviderError> {
        // There is no version endpoint, so a successful model list stands in for health.
        self.list_models().await.map(|_| Health { version: String::new() })
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        let url = self.url("/models");
        let response = self.authed(self.client.get(&url)).timeout(METADATA_TIMEOUT).send().await;
        let models: ModelsResponse = decode_json(&url, response).await?;
        let mut models: Vec<ModelInfo> = models
            .data
            .into_iter()
            .filter_map(|entry| {
                let name = entry.get("id")?.as_str()?.to_owned();
                Some(ModelInfo {
                    name,
                    size_bytes: None,
                    family: None,
                    parameter_size: None,
                    quantization: None,
                    facts: crate::catalog::facts_from_listing(&entry),
                })
            })
            .collect();
        models.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(models)
    }

    async fn capabilities(&self, _model: &str) -> Result<ModelCapabilities, ProviderError> {
        // The OpenAI models endpoint doesn't describe capabilities, so nothing is claimed.
        Ok(ModelCapabilities::default())
    }

    fn stream(
        &self,
        request: ChatRequest,
        cancel: CancellationToken,
    ) -> BoxStream<'static, Result<StreamEvent, ProviderError>> {
        let client = self.client.clone();
        let url = self.url("/chat/completions");
        let base_url = self.base_url.clone();
        let api_key = self.api_key.clone();
        let body = request_body(&request);
        let stream = async_stream::stream! {
            let mut builder = client.post(&url).json(&body);
            if let Some(key) = &api_key {
                builder = builder.bearer_auth(key);
            }
            let response = tokio::select! {
                biased;
                () = cancel.cancelled() => { yield Ok(StreamEvent::Finished(FinishReason::Cancelled)); return; }
                response = builder.send() => response,
            };
            let response = match check(&url, response).await {
                Ok(response) => response,
                Err(error) => { yield Err(explain_limit(&base_url, api_key.as_deref(), error).await); return; }
            };
            let mut bytes = response.bytes_stream();
            let mut decoder = SseDecoder::default();
            // Thinking some servers leave inside the text as <thought>…</thought> goes to the reasoning stream.
            let mut tags = crate::think_tags::TagFilter::default();
            // The finish is held until the end so the trailing usage-only chunk is reported first.
            let mut finish: Option<FinishReason> = None;
            // Tool calls arrive as fragments across chunks, keyed by index; flushed at the end.
            let mut tools_acc: Vec<ToolAcc> = Vec::new();
            loop {
                let next = tokio::select! {
                    biased;
                    () = cancel.cancelled() => { yield Ok(StreamEvent::Finished(FinishReason::Cancelled)); return; }
                    next = bytes.next() => next,
                };
                let body_ended = next.is_none();
                let events = match next {
                    Some(Ok(chunk)) => decoder.push(&chunk),
                    Some(Err(error)) => { yield Err(ProviderError::Unreachable { url: url.clone(), reason: root_cause(&error) }); return; }
                    None => decoder.finish().into_iter().collect(),
                };
                let mut done = false;
                for event in &events {
                    if event.data == "[DONE]" {
                        done = true;
                        break;
                    }
                    match parse_chunk(&event.data, &mut tools_acc) {
                        Ok((items, reason)) => {
                            for item in items {
                                match item {
                                    StreamEvent::TextDelta(text) => {
                                        for routed in tags.push(&text) { yield Ok(routed); }
                                    }
                                    other => yield Ok(other),
                                }
                            }
                            if reason.is_some() { finish = reason; }
                        }
                        Err(error) => { yield Err(error); return; }
                    }
                }
                if done || body_ended {
                    for call in tools_acc.drain(..).map(ToolAcc::into_call) {
                        yield Ok(StreamEvent::ToolCall(call));
                    }
                    for routed in tags.finish() { yield Ok(routed); }
                    yield Ok(StreamEvent::Finished(finish.unwrap_or(FinishReason::Stop)));
                    return;
                }
            }
        };
        stream.boxed()
    }
}

/// On OpenRouter, a rate limit on a free model usually means the day's free allowance is used up;
/// the key's own figures say so, so they're added to the message when they can be read.
async fn explain_limit(base_url: &str, api_key: Option<&str>, error: ProviderError) -> ProviderError {
    let ProviderError::RateLimited { url, retry_after, message } = error else { return error };
    let status = match api_key {
        Some(key) if crate::openrouter::is_openrouter(base_url) => {
            crate::openrouter::key_status(base_url, key).await.ok()
        }
        _ => None,
    };
    let message = match status {
        Some(status) => format!("{message}. {}", status.summary()),
        None => message,
    };
    ProviderError::RateLimited { url, retry_after, message }
}

fn request_body(request: &ChatRequest) -> Value {
    let messages: Vec<Value> = request
        .messages
        .iter()
        .map(|message| {
            let role = match message.role {
                Role::System => "system",
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Tool => "tool",
            };
            let mut wire = if message.images.is_empty() {
                json!({"role": role, "content": message.content})
            } else {
                // Vision messages use the content-array form with data URLs.
                let mut parts = vec![json!({"type": "text", "text": message.content})];
                for image in &message.images {
                    let url = format!("data:{};base64,{}", image.media_type, image.data_base64);
                    parts.push(json!({"type": "image_url", "image_url": {"url": url}}));
                }
                json!({"role": role, "content": parts})
            };
            // An assistant turn that called tools. OpenAI wants the arguments as a JSON string.
            if !message.tool_calls.is_empty() {
                wire["tool_calls"] = message
                    .tool_calls
                    .iter()
                    .map(|call| {
                        json!({
                            "id": call.id,
                            "type": "function",
                            "function": {"name": call.name, "arguments": call.arguments.to_string()}
                        })
                    })
                    .collect::<Vec<_>>()
                    .into();
            }
            // A tool result is tied back to the call by id.
            if let Some(id) = &message.tool_call_id {
                wire["tool_call_id"] = id.clone().into();
            }
            wire
        })
        .collect();
    let mut body = json!({
        "model": request.model,
        "messages": messages,
        "stream": true,
        "max_tokens": DEFAULT_MAX_TOKENS,
        // Ask for a final usage chunk. Servers that don't support this ignore it.
        "stream_options": {"include_usage": true},
    });
    if !request.tools.is_empty() {
        body["tools"] = request
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "function": {"name": tool.name, "description": tool.description, "parameters": tool.parameters}
                })
            })
            .collect::<Vec<_>>()
            .into();
    }
    body
}

/// Parses one streamed chunk into events, merging any tool-call fragments into `tools`. Empty deltas
/// and keep-alive chunks produce nothing.
fn parse_chunk(
    data: &str,
    tools: &mut Vec<ToolAcc>,
) -> Result<(Vec<StreamEvent>, Option<FinishReason>), ProviderError> {
    let chunk: ChatChunk =
        serde_json::from_str(data).map_err(|error| ProviderError::Decode(format!("{error}: {data}")))?;
    if let Some(error) = chunk.error {
        let lower = error.kind.as_deref().unwrap_or_default().to_ascii_lowercase();
        if crate::http::is_client_only_message(&error.message) {
            return Err(ProviderError::ClientOnly { url: String::new(), message: error.message });
        }
        if crate::http::is_limit_kind(&lower) {
            return Err(ProviderError::RateLimited { url: String::new(), retry_after: None, message: error.message });
        }
        if lower.contains("authentication") {
            return Err(ProviderError::NeedsKey { url: String::new(), message: error.message });
        }
        if crate::http::is_policy_message(&error.message) {
            return Err(ProviderError::PolicyBlocked { url: String::new(), message: error.message });
        }
        return Err(ProviderError::Server(error.message));
    }
    let mut events = Vec::new();
    if let Some(usage) = chunk.usage.as_ref() {
        events.push(StreamEvent::Usage(usage.into()));
    }
    let mut finish = None;
    if let Some(choice) = chunk.choices.into_iter().next() {
        if let Some(reasoning) = choice.delta.reasoning_content.filter(|text| !text.is_empty()) {
            events.push(StreamEvent::ReasoningDelta(reasoning));
        }
        if let Some(content) = choice.delta.content.filter(|text| !text.is_empty()) {
            events.push(StreamEvent::TextDelta(content));
        }
        for fragment in choice.delta.tool_calls {
            merge_tool_fragment(tools, fragment);
        }
        finish = choice.finish_reason.map(|reason| match reason.as_str() {
            "stop" | "tool_calls" => FinishReason::Stop,
            "length" => FinishReason::Length,
            other => FinishReason::Other(other.to_owned()),
        });
    }
    Ok((events, finish))
}

/// Accumulates a streamed tool call, whose id, name and argument fragments arrive across chunks.
#[derive(Default)]
struct ToolAcc {
    id: String,
    name: String,
    arguments: String,
}

impl ToolAcc {
    fn into_call(self) -> ToolCall {
        // The arguments come as a JSON string; if that doesn't parse, pass it on for validation.
        let arguments = serde_json::from_str(&self.arguments).unwrap_or(Value::String(self.arguments.clone()));
        let id = if self.id.is_empty() { format!("call_{}", uuid::Uuid::now_v7().simple()) } else { self.id };
        ToolCall { id, name: self.name, arguments }
    }
}

fn merge_tool_fragment(tools: &mut Vec<ToolAcc>, fragment: DeltaToolCall) {
    let index = fragment.index.unwrap_or_else(|| tools.len().saturating_sub(1));
    while tools.len() <= index {
        tools.push(ToolAcc::default());
    }
    let acc = &mut tools[index];
    if let Some(id) = fragment.id {
        acc.id = id;
    }
    if let Some(function) = fragment.function {
        if let Some(name) = function.name {
            acc.name.push_str(&name);
        }
        if let Some(arguments) = function.arguments {
            acc.arguments.push_str(&arguments);
        }
    }
}

#[derive(Deserialize)]
struct ModelsResponse {
    /// Kept as raw JSON: each provider describes its models differently (see `catalog`).
    #[serde(default)]
    data: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct ChatChunk {
    #[serde(default)]
    choices: Vec<Choice>,
    usage: Option<WireUsage>,
    error: Option<WireError>,
}

#[derive(Deserialize)]
struct Choice {
    delta: Delta,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct Delta {
    content: Option<String>,
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<DeltaToolCall>,
}

#[derive(Deserialize)]
struct DeltaToolCall {
    index: Option<usize>,
    id: Option<String>,
    function: Option<DeltaFunction>,
}

#[derive(Deserialize)]
struct DeltaFunction {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Deserialize)]
struct WireError {
    #[serde(default)]
    message: String,
    /// The provider's error type, such as `FreeUsageLimitError` or `authentication_error`.
    #[serde(default, rename = "type")]
    kind: Option<String>,
}

#[derive(Deserialize)]
struct WireUsage {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
}

impl From<&WireUsage> for ReportedUsage {
    fn from(usage: &WireUsage) -> Self {
        Self { prompt_tokens: usage.prompt_tokens, output_tokens: usage.completion_tokens, ..Self::default() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use recode_core::message::{ImageData, Message};

    #[test]
    fn an_image_becomes_a_content_array_with_a_data_url() {
        let image = ImageData { media_type: "image/png".into(), data_base64: "AAAA".into(), label: String::new() };
        let request = ChatRequest {
            model: "m".into(),
            messages: vec![Message::user_with_images("hi", vec![image])],
            tools: vec![],
            options: crate::ProviderOptions::None,
        };
        let body = request_body(&request);
        assert_eq!(body["messages"][0]["content"][0]["type"], "text");
        assert_eq!(body["messages"][0]["content"][1]["image_url"]["url"], "data:image/png;base64,AAAA");
    }

    fn request() -> ChatRequest {
        ChatRequest {
            model: "m".into(),
            messages: vec![Message::system("be brief"), Message::user("hi")],
            tools: vec![],
            options: crate::ProviderOptions::None,
        }
    }

    #[test]
    fn the_body_sends_roles_and_streaming() {
        let body = request_body(&request());
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["content"], "hi");
        assert_eq!(body["stream"], true);
    }

    fn chunk(data: &str) -> (Vec<StreamEvent>, Option<FinishReason>) {
        parse_chunk(data, &mut Vec::new()).unwrap()
    }

    #[test]
    fn a_text_chunk_and_a_usage_chunk_are_understood() {
        let (text, finish) = chunk("{\"choices\":[{\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}");
        assert_eq!(text, [StreamEvent::TextDelta("hi".into())]);
        assert_eq!(finish, None);
        let (events, finish) = chunk(
            "{\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":5}}",
        );
        assert!(matches!(events[0], StreamEvent::Usage(_)));
        assert_eq!(finish, Some(FinishReason::Stop));
    }

    #[test]
    fn reasoning_is_separate_from_text() {
        let (events, _) = chunk("{\"choices\":[{\"delta\":{\"reasoning_content\":\"think\"},\"finish_reason\":null}]}");
        assert_eq!(events, [StreamEvent::ReasoningDelta("think".into())]);
    }

    #[test]
    fn a_server_error_in_the_stream_is_reported() {
        let error = parse_chunk("{\"error\":{\"message\":\"model is overloaded\"}}", &mut Vec::new()).unwrap_err();
        assert!(matches!(error, ProviderError::Server(message) if message == "model is overloaded"));
    }

    #[test]
    fn tool_call_fragments_accumulate_across_chunks() {
        let mut acc = Vec::new();
        // Name and id arrive first, then the arguments split over two chunks.
        parse_chunk(
            "{\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"read\",\"arguments\":\"{\\\"path\\\":\"}}]}}]}",
            &mut acc,
        )
        .unwrap();
        parse_chunk(
            "{\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"a.rs\\\"}\"}}]}}]}",
            &mut acc,
        )
        .unwrap();
        assert_eq!(acc.len(), 1);
        let call = acc.pop().unwrap().into_call();
        assert_eq!(call.id, "call_1");
        assert_eq!(call.name, "read");
        assert_eq!(call.arguments, json!({"path": "a.rs"}));
    }

    #[test]
    fn the_body_sends_tool_schemas_and_round_trips_a_tool_exchange() {
        let call = ToolCall { id: "c1".into(), name: "read".into(), arguments: json!({"path": "a"}) };
        let request = ChatRequest {
            model: "m".into(),
            messages: vec![Message::assistant("", vec![call.clone()]), Message::tool_result(&call, "contents")],
            tools: vec![crate::ToolDefinition {
                name: "read".into(),
                description: "read a file".into(),
                parameters: json!({"type": "object"}),
            }],
            options: crate::ProviderOptions::None,
        };
        let body = request_body(&request);
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["function"]["name"], "read");
        // OpenAI wants the call arguments as a JSON string.
        assert_eq!(body["messages"][0]["tool_calls"][0]["function"]["arguments"], "{\"path\":\"a\"}");
        assert_eq!(body["messages"][1]["role"], "tool");
        assert_eq!(body["messages"][1]["tool_call_id"], "c1");
    }
}
