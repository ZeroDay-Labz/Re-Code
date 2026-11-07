//! Ollama adapter tests. The mocked tests replay streams recorded from a real server.
//! The `#[ignore]`d tests need a running Ollama: `cargo test -p recode-providers -- --ignored`.

use futures_util::StreamExt;
use recode_core::message::{Message, ToolDefinition};
use recode_providers::ollama::{DEFAULT_URL, OllamaOptions, OllamaProvider};
use recode_providers::{ChatRequest, FinishReason, Provider, ProviderError, ProviderOptions, StreamEvent};
use serde_json::json;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CHAT_TEXT: &str = include_str!("fixtures/ollama-0.40.14/chat_text.ndjson");
const CHAT_LENGTH: &str = include_str!("fixtures/ollama-0.40.14/chat_length.ndjson");
const CHAT_TOOL: &str = include_str!("fixtures/ollama-0.40.14/chat_tool_call.ndjson");
const CHAT_THINKING: &str = include_str!("fixtures/ollama-0.40.14/chat_thinking_tool_call.ndjson");
const TAGS: &str = include_str!("fixtures/ollama-0.40.14/tags.json");
const SHOW: &str = include_str!("fixtures/ollama-0.40.14/show.json");

fn request() -> ChatRequest {
    ChatRequest {
        model: "m".into(),
        messages: vec![Message::user("hi")],
        tools: vec![],
        options: ProviderOptions::Ollama(OllamaOptions { num_ctx: Some(4096), ..OllamaOptions::default() }),
    }
}

async fn serve(route: &str, http_method: &str, status: u16, body: &str) -> (MockServer, OllamaProvider) {
    let server = MockServer::start().await;
    Mock::given(method(http_method))
        .and(path(route))
        .respond_with(ResponseTemplate::new(status).set_body_string(body))
        .mount(&server)
        .await;
    let provider = OllamaProvider::new(&server.uri()).unwrap();
    (server, provider)
}

async fn collect_chat(body: &str) -> Vec<Result<StreamEvent, ProviderError>> {
    let (_server, provider) = serve("/api/chat", "POST", 200, body).await;
    provider.stream(request(), CancellationToken::new()).collect().await
}

fn text_of(events: &[StreamEvent]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::TextDelta(text) => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn all_ok(events: Vec<Result<StreamEvent, ProviderError>>) -> Vec<StreamEvent> {
    events.into_iter().map(Result::unwrap).collect()
}

#[tokio::test]
async fn recorded_text_stream_gives_text_usage_and_one_finish() {
    let events = all_ok(collect_chat(CHAT_TEXT).await);
    assert!(!text_of(&events).is_empty());
    assert_eq!(events.last(), Some(&StreamEvent::Finished(FinishReason::Stop)));
    let usage = events.iter().find_map(|event| match event {
        StreamEvent::Usage(usage) => Some(usage),
        _ => None,
    });
    assert!(usage.unwrap().prompt_tokens.unwrap() > 0);
    assert_eq!(events.iter().filter(|event| matches!(event, StreamEvent::Finished(_))).count(), 1);
}

#[tokio::test]
async fn recorded_length_stop_is_distinct_from_a_normal_stop() {
    let events = all_ok(collect_chat(CHAT_LENGTH).await);
    assert_eq!(events.last(), Some(&StreamEvent::Finished(FinishReason::Length)));
}

#[tokio::test]
async fn recorded_tool_call_keeps_the_server_id_and_arguments() {
    let events = all_ok(collect_chat(CHAT_TOOL).await);
    let call = events
        .iter()
        .find_map(|event| match event {
            StreamEvent::ToolCall(call) => Some(call),
            _ => None,
        })
        .unwrap();
    assert_eq!(call.name, "read_file");
    assert_eq!(call.arguments, json!({"path": "src/main.rs"}));
    assert!(call.id.starts_with("call_"));
}

#[tokio::test]
async fn recorded_reasoning_is_separate_from_text() {
    let events = all_ok(collect_chat(CHAT_THINKING).await);
    assert!(events.iter().any(|event| matches!(event, StreamEvent::ReasoningDelta(_))));
    assert!(events.iter().any(|event| matches!(event, StreamEvent::ToolCall(_))));
    assert!(text_of(&events).is_empty(), "reasoning leaked into text");
}

#[tokio::test]
async fn a_body_that_stops_early_is_an_error_not_a_quiet_success() {
    let partial: String = CHAT_TEXT.lines().next().unwrap().to_owned() + "\n";
    let events = collect_chat(&partial).await;
    assert!(matches!(events.last(), Some(Err(ProviderError::EndedEarly))));
}

#[tokio::test]
async fn a_final_line_with_no_newline_is_still_read() {
    let events = all_ok(collect_chat(CHAT_TEXT.trim_end()).await);
    assert_eq!(events.last(), Some(&StreamEvent::Finished(FinishReason::Stop)));
}

#[tokio::test]
async fn an_error_inside_the_stream_is_reported() {
    let events = collect_chat("{\"error\":\"model runner crashed\"}\n").await;
    assert!(matches!(events.last(), Some(Err(ProviderError::Server(message))) if message == "model runner crashed"));
}

#[tokio::test]
async fn invalid_json_is_reported() {
    let events = collect_chat("not json\n").await;
    assert!(matches!(events.last(), Some(Err(ProviderError::Decode(_)))));
}

#[tokio::test]
async fn a_missing_model_reports_the_servers_own_message() {
    // Response recorded from Ollama 0.40.14.
    let (_server, provider) = serve("/api/chat", "POST", 404, "{\"error\":\"model 'x' not found\"}").await;
    let events: Vec<_> = provider.stream(request(), CancellationToken::new()).collect().await;
    assert!(matches!(
        &events[0],
        Err(ProviderError::Http { status: 404, message, .. }) if message == "model 'x' not found"
    ));
}

#[tokio::test]
async fn cancelling_while_waiting_for_the_model_ends_the_stream_promptly() {
    let server = MockServer::start().await;
    Mock::given(path("/api/chat"))
        .respond_with(ResponseTemplate::new(200).set_delay(std::time::Duration::from_secs(60)))
        .mount(&server)
        .await;
    let provider = OllamaProvider::new(&server.uri()).unwrap();
    let cancel = CancellationToken::new();
    let mut stream = provider.stream(request(), cancel.clone());
    let trigger = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        cancel.cancel();
    });
    let first = tokio::time::timeout(std::time::Duration::from_secs(2), stream.next()).await;
    trigger.await.unwrap();
    assert!(matches!(first.expect("cancel took too long"), Some(Ok(StreamEvent::Finished(FinishReason::Cancelled)))));
    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn nothing_listening_is_reported_as_unreachable() {
    // Port 9 (discard) is essentially never in use on a development machine.
    let provider = OllamaProvider::new("http://127.0.0.1:9").unwrap();
    assert!(matches!(provider.health().await, Err(ProviderError::Unreachable { .. })));
}

#[tokio::test]
async fn recorded_metadata_is_understood() {
    let (server, provider) = serve("/api/tags", "GET", 200, TAGS).await;
    Mock::given(path("/api/show")).respond_with(ResponseTemplate::new(200).set_body_string(SHOW)).mount(&server).await;
    let models = provider.list_models().await.unwrap();
    assert!(models.iter().any(|model| model.name == "llama3.2:3b" && model.size_bytes.is_some()));
    let capabilities = provider.capabilities("llama3.2:3b").await.unwrap();
    assert!(capabilities.tools.unwrap().value);
    assert!(!capabilities.vision.unwrap().value);
    assert!(capabilities.context_limit.unwrap().value > 0);
    assert_eq!(capabilities.kv_bytes_per_token.unwrap().value, 114_688, "llama3.2:3b in the recorded /api/show");
}

// Live tests. Slow, and they load a small model.

#[tokio::test]
#[ignore = "needs a running Ollama with llama3.2:3b"]
async fn live_complete_tool_exchange() {
    let provider = OllamaProvider::new(DEFAULT_URL).unwrap();
    provider.health().await.unwrap();
    let tool = ToolDefinition {
        name: "read_file".into(),
        description: "Read a file and return its contents".into(),
        parameters: json!({"type": "object", "required": ["path"], "properties": {"path": {"type": "string"}}}),
    };
    let options = ProviderOptions::Ollama(OllamaOptions {
        num_ctx: Some(4096),
        keep_alive: Some("1m".into()),
        ..OllamaOptions::default()
    });
    let mut messages = vec![Message::user("Use the read_file tool to read src/main.rs, then tell me what it prints.")];
    let make = |messages: &Vec<Message>| ChatRequest {
        model: "llama3.2:3b".into(),
        messages: messages.clone(),
        tools: vec![tool.clone()],
        options: options.clone(),
    };

    let first = all_ok(provider.stream(make(&messages), CancellationToken::new()).collect().await);
    let call = first
        .iter()
        .find_map(|event| match event {
            StreamEvent::ToolCall(call) => Some(call.clone()),
            _ => None,
        })
        .expect("the model made no tool call");
    messages.push(Message::assistant("", vec![call.clone()]));
    messages.push(Message::tool_result(&call, "fn main() { println!(\"Greetings from zebra-41\"); }"));

    let second = all_ok(provider.stream(make(&messages), CancellationToken::new()).collect().await);
    assert!(text_of(&second).to_lowercase().contains("zebra-41"), "got: {}", text_of(&second));
}

#[tokio::test]
#[ignore = "needs a running Ollama with llama3.2:3b"]
async fn live_cancel_stops_a_running_generation() {
    let provider = OllamaProvider::new(DEFAULT_URL).unwrap();
    let mut request = request();
    request.model = "llama3.2:3b".into();
    request.messages = vec![Message::user("Write a very long story about the sea.")];
    let cancel = CancellationToken::new();
    let mut stream = provider.stream(request, cancel.clone());
    while let Some(event) = stream.next().await {
        if matches!(event.unwrap(), StreamEvent::TextDelta(_)) {
            break;
        }
    }
    cancel.cancel();
    assert_eq!(stream.next().await.unwrap().unwrap(), StreamEvent::Finished(FinishReason::Cancelled));
}

#[tokio::test]
#[ignore = "needs a running Ollama with a vision model (qwen3.8:27b)"]
async fn live_vision_accepts_an_image() {
    use recode_core::message::{ImageData, Message};
    // A 1x1 red PNG.
    const RED_PNG_B64: &str =
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
    let provider = OllamaProvider::new(DEFAULT_URL).unwrap();
    let image = ImageData { media_type: "image/png".into(), data_base64: RED_PNG_B64.into(), label: "red".into() };
    let request = ChatRequest {
        model: "qwen3.8:27b".into(),
        messages: vec![Message::user_with_images("What colour is this image? One word.", vec![image])],
        tools: vec![],
        options: ProviderOptions::Ollama(OllamaOptions {
            num_ctx: Some(4096),
            think: Some(false),
            ..OllamaOptions::default()
        }),
    };
    let events = all_ok(provider.stream(request, CancellationToken::new()).collect().await);
    let reply = text_of(&events).to_lowercase();
    assert!(!reply.is_empty(), "the vision model returned nothing");
    assert!(reply.contains("red"), "expected 'red' in: {reply}");
}
