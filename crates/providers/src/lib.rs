//! Connections to model providers.
//!
//! A provider answers "how do I talk to a model?". Starting the server that hosts the model is a
//! separate concern, and so is delegating to a complete external agent.

mod http;
mod ndjson;
mod sse;
mod think_tags;

pub mod anthropic;
pub mod catalog;
pub mod ollama;
pub mod openai;
pub mod openrouter;

use async_trait::async_trait;
use futures_util::stream::BoxStream;
use recode_core::capability::ModelCapabilities;
use recode_core::message::{Message, ToolCall, ToolDefinition};
use recode_core::usage::ReportedUsage;
use serde::Serialize;
use tokio_util::sync::CancellationToken;

/// Errors talking to a provider. Messages are written to be shown to the user.
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("cannot reach {url}: {reason}")]
    Unreachable { url: String, reason: String },
    #[error("{url} answered with HTTP {status}: {message}")]
    Http { url: String, status: u16, message: String },
    #[error("the server reported an error: {0}")]
    Server(String),
    #[error("the server sent a response that could not be understood: {0}")]
    Decode(String),
    #[error("the response ended before the model finished")]
    EndedEarly,
    /// The provider is throttling, or a free allowance is used up. `retry_after` comes from the server when it says.
    #[error("{url} is rate-limiting this model: {message}{}. Try again shortly, or Ctrl+L to pick another model.", retry_hint(*retry_after))]
    RateLimited { url: String, retry_after: Option<std::time::Duration>, message: String },
    /// The provider serves this model only to its own app, whatever the key. OpenCode Zen answers a
    /// free-model request from any other client with a "free usage limit" error that says so.
    #[error(
        "{url} serves this free model only to its own app ({message}). Pick another model with Ctrl+L, or /free for OpenRouter's free models."
    )]
    ClientOnly { url: String, message: String },
    /// The account's own settings rule out every host for this model (OpenRouter's privacy settings
    /// can exclude the free hosts, which may train on prompts).
    #[error(
        "{url} has no host for this model that your privacy settings allow ({message}). On OpenRouter, allow free endpoints at https://openrouter.ai/settings/privacy, or pick another model with Ctrl+L."
    )]
    PolicyBlocked { url: String, message: String },
    /// The account must pay for this model first (HTTP 402): it isn't free for this key, whatever a
    /// listing said.
    #[error(
        "{url} wants payment before it serves this model ({message}). It isn't free on your account: add credits with the provider, or pick another model with Ctrl+L."
    )]
    PaymentRequired { url: String, message: String },
    /// The server wants a valid API key.
    #[error(
        "{url} needs a valid API key ({message}). Add one on the Providers tab, or type /free for a free-models key."
    )]
    NeedsKey { url: String, message: String },
}

fn retry_hint(retry_after: Option<std::time::Duration>) -> String {
    retry_after.map(|wait| format!(" (retry after {} s)", wait.as_secs().max(1))).unwrap_or_default()
}

/// A running server's identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Health {
    pub version: String,
}

/// A model available from a provider.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModelInfo {
    pub name: String,
    pub size_bytes: Option<u64>,
    pub family: Option<String>,
    pub parameter_size: Option<String>,
    pub quantization: Option<String>,
    /// What the listing said about the model, where it said anything.
    pub facts: recode_core::model_facts::ModelFacts,
}

/// Options that exist only on a particular provider. They are kept typed rather than flattened
/// into a common subset, so nothing is lost for providers that offer more.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum ProviderOptions {
    #[default]
    None,
    Ollama(ollama::OllamaOptions),
}

/// A request for a model response.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDefinition>,
    pub options: ProviderOptions,
}

/// Why a model stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinishReason {
    Stop,
    /// Output hit a limit. Any tool call in this response may be incomplete.
    Length,
    /// The caller cancelled the request.
    Cancelled,
    Other(String),
}

/// One item from a response stream.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    TextDelta(String),
    ReasoningDelta(String),
    ToolCall(ToolCall),
    Usage(ReportedUsage),
    Finished(FinishReason),
}

/// A connection to a model provider.
#[async_trait]
pub trait Provider: Send + Sync {
    /// Checks that the server is running and identifies it.
    async fn health(&self) -> Result<Health, ProviderError>;

    /// Lists the available models.
    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError>;

    /// Returns what is known about a model.
    async fn capabilities(&self, model: &str) -> Result<ModelCapabilities, ProviderError>;

    /// Streams a response. Cancelling ends the stream with [`FinishReason::Cancelled`]. A stream
    /// that is otherwise successful always ends with exactly one [`StreamEvent::Finished`].
    fn stream(
        &self,
        request: ChatRequest,
        cancel: CancellationToken,
    ) -> BoxStream<'static, Result<StreamEvent, ProviderError>>;
}
