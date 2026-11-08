//! Token accounting.
//!
//! Estimates and provider-reported figures are different types, so the compiler prevents one
//! from being displayed as the other.

use serde::{Deserialize, Serialize};

/// Usage as reported by a provider. Never constructed from an estimate.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportedUsage {
    /// Tokens in the whole prompt, including any served from cache.
    pub prompt_tokens: Option<u64>,
    /// How many of those were served from cache.
    pub cached_prompt_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub load_ms: Option<u64>,
    pub prompt_eval_ms: Option<u64>,
    pub output_ms: Option<u64>,
}

impl ReportedUsage {
    /// Prompt tokens that had to be evaluated.
    #[must_use]
    pub fn uncached_prompt_tokens(&self) -> Option<u64> {
        let total = self.prompt_tokens?;
        Some(total.saturating_sub(self.cached_prompt_tokens.unwrap_or(0)))
    }
}

/// A token count that always states how it was obtained.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum TokenCount {
    Exact { tokens: u64, tokenizer: String },
    Estimated { tokens: u64, method: String },
}

impl TokenCount {
    /// Text for display. Estimates carry a `~` prefix everywhere.
    #[must_use]
    pub fn display(&self) -> String {
        match self {
            Self::Exact { tokens, .. } => tokens.to_string(),
            Self::Estimated { tokens, .. } => format!("~{tokens}"),
        }
    }
}

/// What part of a request a component of the context ledger is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentKind {
    /// re-code's own instructions to the model.
    System,
    /// Instruction files from the user and the project.
    ProjectInstructions,
    /// What re-code remembered from earlier sessions (the memory index).
    Memory,
    /// The list of skills the model may load.
    SkillsIndex,
    /// A skill the user invoked for this request, in full.
    InvokedSkill,
    /// The note about MCP servers.
    McpNote,
    /// Tool schemas sent with every request.
    ToolDefinitions,
    /// What the user and the model said.
    Conversation,
    /// Tool results still in full.
    ToolResults,
    /// Tool results shortened to a handle.
    Elided,
    /// A summary written by compaction.
    Summary,
    /// Images attached to messages (their token cost isn't estimated).
    Images,
}

impl ComponentKind {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::System => "System prompt",
            Self::ProjectInstructions => "Project instructions",
            Self::Memory => "Memory",
            Self::SkillsIndex => "Skills index",
            Self::InvokedSkill => "Invoked skill",
            Self::McpNote => "MCP note",
            Self::ToolDefinitions => "Tool definitions",
            Self::Conversation => "Conversation",
            Self::ToolResults => "Tool results",
            Self::Elided => "Shortened results",
            Self::Summary => "Compaction summary",
            Self::Images => "Images",
        }
    }
}

/// One part of what a request holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Component {
    pub kind: ComponentKind,
    /// Estimated tokens.
    pub tokens: u64,
    /// How many items (messages, tools, results, images) make it up.
    pub count: usize,
}

/// What filled the last request, by part, against the window it was budgeted in
/// (`docs/CONTEXT_ENGINE.md`, Ledger). Every figure but `reported` is an estimate and says so.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ledger {
    pub components: Vec<Component>,
    pub total: TokenCount,
    /// The window budgeted against, when known.
    pub window: Option<u64>,
    pub reserve: u64,
    /// Where compaction starts, when it's on.
    pub trigger: Option<u64>,
    /// The three largest tool results, named, with their estimated tokens.
    #[serde(default)]
    pub largest_results: Vec<(String, u64)>,
}

impl Ledger {
    /// Estimated tokens in all.
    #[must_use]
    pub fn tokens(&self) -> u64 {
        match &self.total {
            TokenCount::Exact { tokens, .. } | TokenCount::Estimated { tokens, .. } => *tokens,
        }
    }
}

/// What one compaction did. Sizes are estimates. The messages it replaced are kept with it in the
/// store, so nothing the model once saw is lost.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionReport {
    /// Position in the session, from 1.
    pub seq: u64,
    /// When, in Unix milliseconds.
    pub at_ms: i64,
    pub before_tokens: u64,
    pub after_tokens: u64,
    pub elided: usize,
    pub summarised: usize,
    #[serde(default)]
    pub focus: Option<String>,
    #[serde(default)]
    pub problem: Option<String>,
    /// The first request, kept verbatim.
    #[serde(default)]
    pub kept_objective: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uncached_is_total_minus_cached() {
        // Figures measured on Ollama 0.40.14: the total includes cached tokens.
        let usage =
            ReportedUsage { prompt_tokens: Some(3447), cached_prompt_tokens: Some(3446), ..ReportedUsage::default() };
        assert_eq!(usage.uncached_prompt_tokens(), Some(1));
    }

    #[test]
    fn estimates_are_marked_when_displayed() {
        let estimated = TokenCount::Estimated { tokens: 9140, method: "chars".into() };
        let exact = TokenCount::Exact { tokens: 9140, tokenizer: "t".into() };
        assert_eq!(estimated.display(), "~9140");
        assert_eq!(exact.display(), "9140");
    }
}
