//! Live checks of the free-model providers. Listing needs no key; chatting needs one, read from
//! `RE_CODE_TEST_ZEN_KEY` and `RE_CODE_TEST_OPENROUTER_KEY`. All are ignored by default:
//! `cargo test -p recode-providers --test live_free -- --ignored`.

use futures_util::StreamExt;
use recode_core::message::{Message, ToolDefinition};
use recode_providers::openai::OpenAiProvider;
use recode_providers::{ChatRequest, FinishReason, ModelInfo, Provider, ProviderOptions, StreamEvent};
use tokio_util::sync::CancellationToken;

const ZEN: &str = "https://opencode.ai/zen/v1";
const OPENROUTER: &str = "https://openrouter.ai/api/v1";

async fn free_tool_models(base: &str, key: &str) -> Vec<ModelInfo> {
    let provider = OpenAiProvider::new(base, key).unwrap();
    let models = provider.list_models().await.expect("the listing is public");
    models.into_iter().filter(|m| m.facts.free == Some(true) && m.facts.tools == Some(true)).collect()
}

#[tokio::test]
#[ignore = "reaches the public model listings over the network"]
async fn both_providers_list_free_tool_capable_models_without_a_key() {
    let zen = free_tool_models(ZEN, "").await;
    let openrouter = free_tool_models(OPENROUTER, "").await;
    eprintln!(
        "ZEN free+tools: {:?}\nOPENROUTER free+tools: {:?}",
        zen.iter().map(|m| &m.name).collect::<Vec<_>>(),
        openrouter.iter().map(|m| &m.name).collect::<Vec<_>>()
    );
    assert!(!zen.is_empty(), "Zen's free list changes weekly, but it's never been empty");
    assert!(!openrouter.is_empty());
    assert!(zen.iter().all(|m| m.facts.context.is_some()), "Zen reports context sizes");
}

/// A chat and a tool-call round trip on the first free tool-capable model, with a real key.
async fn free_model_round_trip(base: &str, key: &str) {
    let models = free_tool_models(base, key).await;
    let model = models.first().expect("a free tool-capable model").name.clone();
    eprintln!("Using {model} on {base}");
    let provider = OpenAiProvider::new(base, key).unwrap();

    let request = ChatRequest {
        model: model.clone(),
        messages: vec![Message::user("Reply with exactly the word OK and nothing else.")],
        tools: Vec::new(),
        options: ProviderOptions::None,
    };
    let events: Vec<_> = provider.stream(request, CancellationToken::new()).collect().await;
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            Ok(StreamEvent::TextDelta(t)) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    assert!(text.to_uppercase().contains("OK"), "{events:?}");
    assert!(events.iter().any(|e| matches!(e, Ok(StreamEvent::Finished(FinishReason::Stop)))));

    let tool = ToolDefinition {
        name: "read".to_owned(),
        description: "Read a file's contents.".to_owned(),
        parameters: serde_json::json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}),
    };
    let request = ChatRequest {
        model,
        messages: vec![Message::user("Use the read tool to read src/main.rs. Call the tool; don't answer in words.")],
        tools: vec![tool],
        options: ProviderOptions::None,
    };
    let events: Vec<_> = provider.stream(request, CancellationToken::new()).collect().await;
    let call = events.iter().find_map(|e| match e {
        Ok(StreamEvent::ToolCall(call)) => Some(call.clone()),
        _ => None,
    });
    let call = call.unwrap_or_else(|| panic!("the free model didn't call the tool: {events:?}"));
    assert_eq!(call.name, "read");
    assert_eq!(call.arguments.get("path").and_then(|v| v.as_str()), Some("src/main.rs"));
}

#[tokio::test]
#[ignore = "needs RE_CODE_TEST_ZEN_KEY"]
async fn opencode_zen_free_model_chats_and_calls_a_tool() {
    let key = std::env::var("RE_CODE_TEST_ZEN_KEY").expect("set RE_CODE_TEST_ZEN_KEY");
    free_model_round_trip(ZEN, &key).await;
}

#[tokio::test]
#[ignore = "needs RE_CODE_TEST_OPENROUTER_KEY"]
async fn openrouter_free_model_chats_and_calls_a_tool() {
    let key = std::env::var("RE_CODE_TEST_OPENROUTER_KEY").expect("set RE_CODE_TEST_OPENROUTER_KEY");
    free_model_round_trip(OPENROUTER, &key).await;
}
