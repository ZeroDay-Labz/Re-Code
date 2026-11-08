//! Live checks of the cloud adapters with a real key: a plain chat, then a tool-call round trip.
//! Everything comes from the environment so no key is written anywhere:
//!
//!   RE_CODE_TEST_OPENAI_BASE, RE_CODE_TEST_OPENAI_KEY, RE_CODE_TEST_OPENAI_MODEL   (any OpenAI-compatible host)
//!   RE_CODE_TEST_ANTHROPIC_KEY, RE_CODE_TEST_ANTHROPIC_MODEL
//!
//! `cargo test -p recode-providers --test live_cloud -- --ignored --nocapture`

use futures_util::StreamExt;
use recode_core::message::{Message, Role, ToolDefinition};
use recode_providers::anthropic::AnthropicProvider;
use recode_providers::openai::OpenAiProvider;
use recode_providers::{ChatRequest, Provider, ProviderOptions, StreamEvent};
use tokio_util::sync::CancellationToken;

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

fn read_tool() -> ToolDefinition {
    ToolDefinition {
        name: "read".to_owned(),
        description: "Read a file's contents.".to_owned(),
        parameters: serde_json::json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}),
    }
}

/// Chat, then a tool call, then the tool's result back to the model, on any provider.
async fn round_trip(provider: &dyn Provider, model: &str) {
    let request = ChatRequest {
        model: model.to_owned(),
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
    assert!(text.to_uppercase().contains("OK"), "plain chat: {events:?}");
    let usage = events.iter().find_map(|e| match e {
        Ok(StreamEvent::Usage(usage)) => Some(usage.clone()),
        _ => None,
    });
    eprintln!("chat ok on {model}; reported usage: {usage:?}");

    let request = ChatRequest {
        model: model.to_owned(),
        messages: vec![Message::user("Use the read tool to read src/main.rs. Call the tool; don't answer in words.")],
        tools: vec![read_tool()],
        options: ProviderOptions::None,
    };
    let events: Vec<_> = provider.stream(request, CancellationToken::new()).collect().await;
    let call = events
        .iter()
        .find_map(|e| match e {
            Ok(StreamEvent::ToolCall(call)) => Some(call.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("the model didn't call the tool: {events:?}"));
    assert_eq!(call.name, "read");
    assert_eq!(call.arguments.get("path").and_then(|v| v.as_str()), Some("src/main.rs"), "{call:?}");
    eprintln!("tool call ok: {} {}", call.name, call.arguments);

    // The round trip: the assistant's call and our result go back, and the model answers in words.
    let mut messages = vec![
        Message::user("Use the read tool to read src/main.rs, then tell me in one short sentence what the file does."),
        Message::assistant("", vec![call.clone()]),
        Message::tool_result(&call, "fn main() { println!(\"hello from re-code\"); }"),
    ];
    messages.retain(|m| m.role != Role::System);
    let request =
        ChatRequest { model: model.to_owned(), messages, tools: vec![read_tool()], options: ProviderOptions::None };
    let events: Vec<_> = provider.stream(request, CancellationToken::new()).collect().await;
    let answer: String = events
        .iter()
        .filter_map(|e| match e {
            Ok(StreamEvent::TextDelta(t)) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        answer.to_lowercase().contains("hello") || answer.to_lowercase().contains("print"),
        "round trip: {events:?}"
    );
    eprintln!("round trip ok: {}", answer.trim());
}

#[tokio::test]
#[ignore = "needs RE_CODE_TEST_OPENAI_BASE, _KEY and _MODEL"]
async fn an_openai_compatible_provider_chats_calls_a_tool_and_takes_the_result_back() {
    let (Some(base), Some(key), Some(model)) =
        (var("RE_CODE_TEST_OPENAI_BASE"), var("RE_CODE_TEST_OPENAI_KEY"), var("RE_CODE_TEST_OPENAI_MODEL"))
    else {
        panic!("set RE_CODE_TEST_OPENAI_BASE, RE_CODE_TEST_OPENAI_KEY and RE_CODE_TEST_OPENAI_MODEL");
    };
    let provider = OpenAiProvider::new(&base, &key).unwrap();
    round_trip(&provider, &model).await;
}

#[tokio::test]
#[ignore = "needs RE_CODE_TEST_ANTHROPIC_KEY and _MODEL; costs a fraction of a cent"]
async fn anthropic_chats_calls_a_tool_and_takes_the_result_back() {
    let (Some(key), Some(model)) = (var("RE_CODE_TEST_ANTHROPIC_KEY"), var("RE_CODE_TEST_ANTHROPIC_MODEL")) else {
        panic!("set RE_CODE_TEST_ANTHROPIC_KEY and RE_CODE_TEST_ANTHROPIC_MODEL");
    };
    let base = var("RE_CODE_TEST_ANTHROPIC_BASE").unwrap_or_else(|| "https://api.anthropic.com".to_owned());
    let provider = AnthropicProvider::new(&base, &key).unwrap();
    round_trip(&provider, &model).await;
}
