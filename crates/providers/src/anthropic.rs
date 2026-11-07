//! An adapter for Anthropic's native Messages API, so Claude's features aren't lost through a
//! compatibility layer. Tool calling (schemas, streamed `tool_use` parsing, and the `tool_result`
//! round-trip) is supported.
//!
//! Anthropic doesn't permit third-party apps to use Claude subscription sign-in, so this uses an
//! API key only. See `docs/PROVIDER_ARCHITECTURE.md`.

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

const API_VERSION: &str = "2023-06-01";
const METADATA_TIMEOUT: Duration = Duration::from_secs(20);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_MAX_TOKENS: u32 = 4096;

/// A connection to an Anthropic-style server.
#[derive(Debug, Clone)]
pub struct AnthropicProvider {
    base_url: String,
    api_key: String,
    client: reqwest::Client,
}

impl AnthropicProvider {
    pub fn new(base_url: &str, api_key: &str) -> Result<Self, ProviderError> {
        let base_url = base_url.trim_end_matches('/').to_owned();
        let client = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .map_err(|error| ProviderError::Unreachable { url: base_url.clone(), reason: error.to_string() })?;
        Ok(Self { base_url, api_key: api_key.trim().to_owned(), client })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }

    fn authed(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        builder.header("x-api-key", &self.api_key).header("anthropic-version", API_VERSION)
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    async fn health(&self) -> Result<Health, ProviderError> {
        self.list_models().await.map(|_| Health { version: API_VERSION.to_owned() })
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        let url = self.url("/v1/models?limit=1000");
        let response = self.authed(self.client.get(&url)).timeout(METADATA_TIMEOUT).send().await;
        let models: ModelsResponse = decode_json(&url, response).await?;
        Ok(models
            .data
            .into_iter()
            .map(|model| ModelInfo {
                name: model.id,
                size_bytes: None,
                family: model.display_name,
                parameter_size: None,
                quantization: None,
                facts: recode_core::model_facts::ModelFacts::default(),
            })
            .collect())
    }

    async fn capabilities(&self, _model: &str) -> Result<ModelCapabilities, ProviderError> {
        Ok(ModelCapabilities::default())
    }

    fn stream(
        &self,
        request: ChatRequest,
        cancel: CancellationToken,
    ) -> BoxStream<'static, Result<StreamEvent, ProviderError>> {
        let client = self.client.clone();
        let url = self.url("/v1/messages");
        let key = self.api_key.clone();
        let body = request_body(&request);
        let stream = async_stream::stream! {
            let builder = client.post(&url).header("x-api-key", &key).header("anthropic-version", API_VERSION).json(&body);
            let response = tokio::select! {
                biased;
                () = cancel.cancelled() => { yield Ok(StreamEvent::Finished(FinishReason::Cancelled)); return; }
                response = builder.send() => response,
            };
            let response = match check(&url, response).await {
                Ok(response) => response,
                Err(error) => { yield Err(error); return; }
            };
            let mut bytes = response.bytes_stream();
            let mut decoder = SseDecoder::default();
            let mut finished = false;
            // tool_use blocks arrive across start/delta/stop events, keyed by content-block index.
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
                for event in &events {
                    match parse_event(event, &mut tools_acc) {
                        Ok(items) => {
                            for item in items {
                                if matches!(item, StreamEvent::Finished(_)) { finished = true; }
                                yield Ok(item);
                            }
                        }
                        Err(error) => { yield Err(error); return; }
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
    // Anthropic takes the system prompt at the top level, not as a message.
    let system: Vec<&str> = request
        .messages
        .iter()
        .filter(|message| message.role == Role::System)
        .map(|message| message.content.as_str())
        .collect();
    let messages: Vec<Value> = request
        .messages
        .iter()
        .filter(|message| matches!(message.role, Role::User | Role::Assistant | Role::Tool))
        .map(|message| {
            // A tool result is a user turn carrying a tool_result block tied back to the call's id.
            if message.role == Role::Tool {
                return json!({
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": message.tool_call_id.clone().unwrap_or_default(),
                        "content": message.content,
                    }]
                });
            }
            let role = if message.role == Role::User { "user" } else { "assistant" };
            // An assistant turn that called tools: optional text, then a tool_use block per call.
            if !message.tool_calls.is_empty() {
                let mut parts = Vec::new();
                if !message.content.is_empty() {
                    parts.push(json!({"type": "text", "text": message.content}));
                }
                for call in &message.tool_calls {
                    parts.push(json!({
                        "type": "tool_use",
                        "id": call.id,
                        "name": call.name,
                        "input": call.arguments,
                    }));
                }
                return json!({"role": role, "content": parts});
            }
            if message.images.is_empty() {
                json!({"role": role, "content": message.content})
            } else {
                // Images come before the text block in Anthropic's content array.
                let mut parts = Vec::new();
                for image in &message.images {
                    parts.push(json!({
                        "type": "image",
                        "source": {"type": "base64", "media_type": image.media_type, "data": image.data_base64},
                    }));
                }
                parts.push(json!({"type": "text", "text": message.content}));
                json!({"role": role, "content": parts})
            }
        })
        .collect();
    let mut body = json!({
        "model": request.model,
        "max_tokens": DEFAULT_MAX_TOKENS,
        "messages": messages,
        "stream": true,
    });
    if !system.is_empty() {
        body["system"] = system.join("\n\n").into();
    }
    if !request.tools.is_empty() {
        body["tools"] = request
            .tools
            .iter()
            .map(|tool| json!({"name": tool.name, "description": tool.description, "input_schema": tool.parameters}))
            .collect::<Vec<_>>()
            .into();
    }
    body
}

/// Turns one SSE event into stream events, merging `tool_use` fragments into `tools`.
fn parse_event(event: &crate::sse::SseEvent, tools: &mut Vec<ToolAcc>) -> Result<Vec<StreamEvent>, ProviderError> {
    if event.data.is_empty() {
        return Ok(Vec::new());
    }
    let parsed: Event =
        serde_json::from_str(&event.data).map_err(|error| ProviderError::Decode(format!("{error}: {}", event.data)))?;
    Ok(match parsed {
        Event::Error { error } => return Err(ProviderError::Server(error.message)),
        Event::MessageStart { message } => vec![StreamEvent::Usage(ReportedUsage {
            prompt_tokens: message.usage.input_tokens,
            ..ReportedUsage::default()
        })],
        Event::ContentBlockStart { index, content_block } => {
            if content_block.block_type == "tool_use" {
                while tools.len() <= index {
                    tools.push(ToolAcc::default());
                }
                tools[index].id = content_block.id.unwrap_or_default();
                tools[index].name = content_block.name.unwrap_or_default();
            }
            Vec::new()
        }
        Event::ContentBlockDelta { index, delta } => match delta {
            Delta::TextDelta { text } => vec![StreamEvent::TextDelta(text)],
            Delta::ThinkingDelta { thinking } => vec![StreamEvent::ReasoningDelta(thinking)],
            Delta::InputJsonDelta { partial_json } => {
                if let Some(acc) = tools.get_mut(index) {
                    acc.arguments.push_str(&partial_json);
                }
                Vec::new()
            }
            Delta::Other => Vec::new(),
        },
        Event::MessageDelta { delta, usage } => {
            let mut events = Vec::new();
            if let Some(usage) = usage {
                events.push(StreamEvent::Usage(ReportedUsage {
                    output_tokens: usage.output_tokens,
                    ..ReportedUsage::default()
                }));
            }
            if let Some(reason) = delta.stop_reason {
                // The tool_use blocks are complete, so emit them before the turn finishes.
                for call in tools.drain(..).map(ToolAcc::into_call) {
                    events.push(StreamEvent::ToolCall(call));
                }
                events.push(StreamEvent::Finished(match reason.as_str() {
                    "end_turn" | "stop_sequence" | "tool_use" => FinishReason::Stop,
                    "max_tokens" => FinishReason::Length,
                    other => FinishReason::Other(other.to_owned()),
                }));
            }
            events
        }
        Event::ContentBlockStop | Event::MessageStop | Event::Other => Vec::new(),
    })
}

/// Accumulates a streamed `tool_use` block, whose input JSON arrives in fragments.
#[derive(Default)]
struct ToolAcc {
    id: String,
    name: String,
    arguments: String,
}

impl ToolAcc {
    fn into_call(self) -> ToolCall {
        // Anthropic sends `{}` as no input deltas at all, so an empty buffer means empty arguments.
        let arguments = if self.arguments.is_empty() {
            Value::Object(serde_json::Map::new())
        } else {
            serde_json::from_str(&self.arguments).unwrap_or(Value::String(self.arguments.clone()))
        };
        let id = if self.id.is_empty() { format!("call_{}", uuid::Uuid::now_v7().simple()) } else { self.id };
        ToolCall { id, name: self.name, arguments }
    }
}

#[derive(Deserialize)]
struct ModelsResponse {
    #[serde(default)]
    data: Vec<WireModel>,
}

#[derive(Deserialize)]
struct WireModel {
    id: String,
    display_name: Option<String>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Event {
    MessageStart {
        message: MessageStart,
    },
    ContentBlockStart {
        #[serde(default)]
        index: usize,
        content_block: ContentBlock,
    },
    ContentBlockDelta {
        #[serde(default)]
        index: usize,
        delta: Delta,
    },
    ContentBlockStop,
    MessageDelta {
        delta: MessageDelta,
        usage: Option<Usage>,
    },
    MessageStop,
    Error {
        error: WireError,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct MessageStart {
    usage: Usage,
}

#[derive(Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    block_type: String,
    id: Option<String>,
    name: Option<String>,
}

#[derive(Deserialize)]
#[allow(clippy::enum_variant_names)] // names mirror Anthropic's type tags
#[serde(tag = "type", rename_all = "snake_case")]
enum Delta {
    TextDelta {
        text: String,
    },
    ThinkingDelta {
        thinking: String,
    },
    InputJsonDelta {
        partial_json: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct MessageDelta {
    stop_reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct Usage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct WireError {
    #[serde(default)]
    message: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sse::SseEvent;
    use recode_core::message::{ImageData, Message};

    fn event(data: &str) -> SseEvent {
        SseEvent { event: None, data: data.to_owned() }
    }

    fn parse(ev: &SseEvent) -> Vec<StreamEvent> {
        parse_event(ev, &mut Vec::new()).unwrap()
    }

    #[test]
    fn the_system_prompt_goes_to_the_top_level() {
        let request = ChatRequest {
            model: "claude".into(),
            messages: vec![Message::system("be brief"), Message::user("hi")],
            tools: vec![],
            options: crate::ProviderOptions::None,
        };
        let body = request_body(&request);
        assert_eq!(body["system"], "be brief");
        assert_eq!(body["messages"].as_array().unwrap().len(), 1);
        assert_eq!(body["messages"][0]["role"], "user");
    }

    #[test]
    fn an_image_becomes_a_base64_source_block_before_the_text() {
        let image = ImageData { media_type: "image/jpeg".into(), data_base64: "AAAA".into(), label: String::new() };
        let request = ChatRequest {
            model: "claude".into(),
            messages: vec![Message::user_with_images("describe", vec![image])],
            tools: vec![],
            options: crate::ProviderOptions::None,
        };
        let body = request_body(&request);
        assert_eq!(body["messages"][0]["content"][0]["type"], "image");
        assert_eq!(body["messages"][0]["content"][0]["source"]["media_type"], "image/jpeg");
        assert_eq!(body["messages"][0]["content"][1]["type"], "text");
    }

    #[test]
    fn a_full_event_sequence_is_understood() {
        assert!(matches!(
            parse(&event("{\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":10}}}"))[0],
            StreamEvent::Usage(_)
        ));
        let text = parse(&event(
            "{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}",
        ));
        assert_eq!(text, [StreamEvent::TextDelta("hi".into())]);
        let think = parse(&event(
            "{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"hmm\"}}",
        ));
        assert_eq!(think, [StreamEvent::ReasoningDelta("hmm".into())]);
        let end = parse(&event(
            "{\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":7}}",
        ));
        assert!(matches!(end[0], StreamEvent::Usage(_)));
        assert_eq!(end[1], StreamEvent::Finished(FinishReason::Stop));
    }

    #[test]
    fn unknown_events_and_pings_are_ignored() {
        assert!(parse(&event("{\"type\":\"ping\"}")).is_empty());
        assert!(
            parse(&event("{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\"}}"))
                .is_empty()
        );
    }

    #[test]
    fn an_error_event_is_reported() {
        let error = parse_event(
            &event("{\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"overloaded\"}}"),
            &mut Vec::new(),
        )
        .unwrap_err();
        assert!(matches!(error, ProviderError::Server(message) if message == "overloaded"));
    }

    #[test]
    fn a_tool_use_block_is_assembled_and_emitted_before_finishing() {
        let mut acc = Vec::new();
        parse_event(
            &event("{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tu_1\",\"name\":\"read\"}}"),
            &mut acc,
        )
        .unwrap();
        parse_event(
            &event("{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\"}}"),
            &mut acc,
        )
        .unwrap();
        parse_event(
            &event("{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"a.rs\\\"}\"}}"),
            &mut acc,
        )
        .unwrap();
        let end =
            parse_event(&event("{\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"}}"), &mut acc)
                .unwrap();
        assert_eq!(end.len(), 2);
        match &end[0] {
            StreamEvent::ToolCall(call) => {
                assert_eq!(call.id, "tu_1");
                assert_eq!(call.name, "read");
                assert_eq!(call.arguments, json!({"path": "a.rs"}));
            }
            other => panic!("expected a tool call, got {other:?}"),
        }
        assert_eq!(end[1], StreamEvent::Finished(FinishReason::Stop));
    }

    #[test]
    fn the_body_sends_tool_schemas_and_round_trips_tool_use_and_result() {
        let call = ToolCall { id: "tu_1".into(), name: "read".into(), arguments: json!({"path": "a"}) };
        let request = ChatRequest {
            model: "claude".into(),
            messages: vec![
                Message::user("read a.rs"),
                Message::assistant("let me look", vec![call.clone()]),
                Message::tool_result(&call, "contents"),
            ],
            tools: vec![crate::ToolDefinition {
                name: "read".into(),
                description: "read a file".into(),
                parameters: json!({"type": "object"}),
            }],
            options: crate::ProviderOptions::None,
        };
        let body = request_body(&request);
        assert_eq!(body["tools"][0]["name"], "read");
        assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
        // The assistant turn carries a text block then a tool_use block.
        assert_eq!(body["messages"][1]["content"][1]["type"], "tool_use");
        assert_eq!(body["messages"][1]["content"][1]["input"]["path"], "a");
        // The tool result is a user turn with a tool_result block tied to the call id.
        assert_eq!(body["messages"][2]["role"], "user");
        assert_eq!(body["messages"][2]["content"][0]["type"], "tool_result");
        assert_eq!(body["messages"][2]["content"][0]["tool_use_id"], "tu_1");
    }
}
