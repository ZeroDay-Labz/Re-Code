//! End-to-end tests for the cloud adapters, replaying recorded SSE bodies over a mock server.

use futures_util::StreamExt;
use recode_core::message::Message;
use recode_providers::anthropic::AnthropicProvider;
use recode_providers::openai::OpenAiProvider;
use recode_providers::{ChatRequest, FinishReason, Provider, ProviderError, ProviderOptions, StreamEvent};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn request(model: &str) -> ChatRequest {
    ChatRequest {
        model: model.into(),
        messages: vec![Message::system("be brief"), Message::user("hi")],
        tools: vec![],
        options: ProviderOptions::None,
    }
}

fn text(events: &[StreamEvent]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::TextDelta(text) => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

async fn collect(stream: impl StreamExt<Item = Result<StreamEvent, ProviderError>>) -> Vec<StreamEvent> {
    stream.map(Result::unwrap).collect().await
}

const OPENAI_SSE: &str = "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"},\"finish_reason\":null}]}\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"},\"finish_reason\":null}]}\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\" there\"},\"finish_reason\":null}]}\n\n\
data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
data: {\"choices\":[],\"usage\":{\"prompt_tokens\":9,\"completion_tokens\":2}}\n\n\
data: [DONE]\n\n";

#[tokio::test]
async fn openai_streams_text_then_usage_then_finish() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(header("authorization", "Bearer secret-key"))
        .respond_with(ResponseTemplate::new(200).set_body_string(OPENAI_SSE))
        .mount(&server)
        .await;
    let provider = OpenAiProvider::new(&server.uri(), "secret-key").unwrap();
    let events = collect(provider.stream(request("gpt-x"), CancellationToken::new())).await;
    assert_eq!(text(&events), "Hello there");
    assert!(events.iter().any(|event| matches!(event, StreamEvent::Usage(usage) if usage.prompt_tokens == Some(9))));
    assert_eq!(events.last(), Some(&StreamEvent::Finished(FinishReason::Stop)));
    assert_eq!(events.iter().filter(|event| matches!(event, StreamEvent::Finished(_))).count(), 1);
}

#[tokio::test]
async fn openai_lists_models() {
    let server = MockServer::start().await;
    Mock::given(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{\"data\":[{\"id\":\"gpt-b\"},{\"id\":\"gpt-a\"}]}"))
        .mount(&server)
        .await;
    let provider = OpenAiProvider::new(&server.uri(), "k").unwrap();
    let models = provider.list_models().await.unwrap();
    assert_eq!(models.iter().map(|model| model.name.as_str()).collect::<Vec<_>>(), ["gpt-a", "gpt-b"]);
}

#[tokio::test]
async fn a_bad_key_is_reported_as_a_rejected_key() {
    let server = MockServer::start().await;
    Mock::given(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(401).set_body_string("{\"error\":{\"message\":\"Incorrect API key\"}}"))
        .mount(&server)
        .await;
    let provider = OpenAiProvider::new(&server.uri(), "wrong").unwrap();
    let events: Vec<_> = provider.stream(request("m"), CancellationToken::new()).collect().await;
    assert!(
        matches!(&events[0], Err(ProviderError::NeedsKey { message, .. }) if message.contains("Incorrect API key"))
    );
    let text = events[0].as_ref().unwrap_err().to_string();
    assert!(text.contains("/free") && text.contains("Providers tab"), "{text}");
}

#[tokio::test]
async fn thinking_left_inside_the_text_is_routed_to_reasoning() {
    // As Gemini's OpenAI-compatible endpoint streamed it on 2026-11-07: the tags arrive split across chunks.
    const SSE: &str = "data: {\"choices\":[{\"delta\":{\"content\":\"<thou\"},\"finish_reason\":null}]}\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\"ght>The user greets me.</thought>Hi\"},\"finish_reason\":null}]}\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\" there!\"},\"finish_reason\":\"stop\"}]}\n\n\
data: [DONE]\n\n";
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SSE))
        .mount(&server)
        .await;
    let provider = OpenAiProvider::new(&server.uri(), "k").unwrap();
    let events = collect(provider.stream(request("gemma"), CancellationToken::new())).await;
    assert_eq!(text(&events), "Hi there!");
    let reasoning: String = events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::ReasoningDelta(text) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(reasoning, "The user greets me.");
    assert_eq!(events.last(), Some(&StreamEvent::Finished(FinishReason::Stop)));
}

#[tokio::test]
async fn a_data_policy_refusal_is_explained() {
    // OpenRouter's answer when the account's privacy settings exclude every host of a free model.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(404).set_body_string(
            "{\"error\":{\"message\":\"No endpoints found matching your data policy (Free model publication). Configure: https://openrouter.ai/settings/privacy\",\"code\":404}}",
        ))
        .mount(&server)
        .await;
    let provider = OpenAiProvider::new(&server.uri(), "k").unwrap();
    let events: Vec<_> = provider.stream(request("x/y:free"), CancellationToken::new()).collect().await;
    let Err(ProviderError::PolicyBlocked { message, .. }) = &events[0] else { panic!("{events:?}") };
    assert!(message.contains("data policy"));
    assert!(events[0].as_ref().unwrap_err().to_string().contains("settings/privacy"));
}

#[tokio::test]
async fn a_rate_limit_is_reported_with_the_wait_and_a_usage_limit_counts_as_one() {
    let server = MockServer::start().await;
    Mock::given(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "20")
                .set_body_string("{\"error\":{\"message\":\"Rate limit exceeded\"}}"),
        )
        .mount(&server)
        .await;
    let provider = OpenAiProvider::new(&server.uri(), "k").unwrap();
    let events: Vec<_> = provider.stream(request("m"), CancellationToken::new()).collect().await;
    let Err(ProviderError::RateLimited { retry_after, message, .. }) = &events[0] else { panic!("{events:?}") };
    assert_eq!(*retry_after, Some(std::time::Duration::from_secs(20)));
    assert_eq!(message, "Rate limit exceeded");
    assert!(events[0].as_ref().unwrap_err().to_string().contains("retry after 20 s"));

    // OpenCode Zen answers an exhausted free allowance with its own error type.
    let zen = MockServer::start().await;
    Mock::given(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(429).set_body_string(
            "{\"type\":\"error\",\"error\":{\"type\":\"FreeUsageLimitError\",\"message\":\"Free usage limit reached.\"}}",
        ))
        .mount(&zen)
        .await;
    let provider = OpenAiProvider::new(&zen.uri(), "k").unwrap();
    let events: Vec<_> = provider.stream(request("big-pickle"), CancellationToken::new()).collect().await;
    assert!(matches!(&events[0], Err(ProviderError::RateLimited { message, .. }) if message.contains("Free usage")));
}

const ANTHROPIC_SSE: &str = "event: message_start\n\
data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":12}}}\n\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\" friend\"}}\n\n\
event: message_delta\n\
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":3}}\n\n\
event: message_stop\n\
data: {\"type\":\"message_stop\"}\n\n";

#[tokio::test]
async fn anthropic_streams_with_the_api_key_header_and_system_prompt() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "sk-test"))
        .and(header("anthropic-version", "2023-06-01"))
        .respond_with(ResponseTemplate::new(200).set_body_string(ANTHROPIC_SSE))
        .mount(&server)
        .await;
    let provider = AnthropicProvider::new(&server.uri(), "sk-test").unwrap();
    let events = collect(provider.stream(request("claude-x"), CancellationToken::new())).await;
    assert_eq!(text(&events), "Hi friend");
    assert_eq!(events.last(), Some(&StreamEvent::Finished(FinishReason::Stop)));
}

#[tokio::test]
async fn anthropic_lists_models_with_display_names() {
    let server = MockServer::start().await;
    Mock::given(path("/v1/models"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("{\"data\":[{\"id\":\"claude-x\",\"display_name\":\"Claude X\"}]}"),
        )
        .mount(&server)
        .await;
    let provider = AnthropicProvider::new(&server.uri(), "k").unwrap();
    let models = provider.list_models().await.unwrap();
    assert_eq!(models[0].name, "claude-x");
    assert_eq!(models[0].family.as_deref(), Some("Claude X"));
}

#[tokio::test]
async fn cancelling_a_cloud_stream_ends_it_promptly() {
    let server = MockServer::start().await;
    Mock::given(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_delay(std::time::Duration::from_secs(60)))
        .mount(&server)
        .await;
    let provider = OpenAiProvider::new(&server.uri(), "k").unwrap();
    let cancel = CancellationToken::new();
    let mut stream = provider.stream(request("m"), cancel.clone());
    let trigger = tokio::spawn({
        let cancel = cancel.clone();
        async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            cancel.cancel();
        }
    });
    let first = tokio::time::timeout(std::time::Duration::from_secs(2), stream.next()).await.expect("cancel too slow");
    trigger.await.unwrap();
    assert!(matches!(first, Some(Ok(StreamEvent::Finished(FinishReason::Cancelled)))));
}

// Live tests against Ollama's OpenAI-compatible endpoint, proving the adapter end to end.
// Run with: cargo test -p recode-providers --test cloud -- --ignored

#[tokio::test]
#[ignore = "needs a running Ollama with llama3.2:3b"]
async fn live_openai_endpoint_lists_and_streams() {
    let provider = OpenAiProvider::new("http://localhost:11434/v1", "").unwrap();
    let models = provider.list_models().await.unwrap();
    assert!(models.iter().any(|model| model.name == "llama3.2:3b"), "models: {models:?}");

    let events = collect(provider.stream(request("llama3.2:3b"), CancellationToken::new())).await;
    assert!(!text(&events).is_empty(), "the model produced no text");
    assert!(matches!(events.last(), Some(StreamEvent::Finished(FinishReason::Stop))));
    assert!(events.iter().any(|event| matches!(event, StreamEvent::Usage(usage) if usage.prompt_tokens.is_some())));
}
