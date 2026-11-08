//! Keeping a conversation within the model's context (`docs/CONTEXT_ENGINE.md`, Compaction).
//!
//! Stage 1 elides old tool results, replacing each with a short handle; the original is kept and
//! `read_result` brings it back. It's deterministic and needs no model call. Stage 2, only if that
//! isn't enough, summarises older messages with a visible model call, keeping the objective, the
//! current request and the most recent steps verbatim. Both happen in one batch, so the prompt cache
//! is invalidated once per compaction rather than on every request.
//!
//! All sizes here are estimates from character counts, and are labelled that way wherever shown.

use recode_core::context::ContextWindow;
use recode_core::message::{Message, Role, ToolDefinition};
use recode_core::usage::{Component, ComponentKind, Ledger, TokenCount};
use serde_json::json;
use std::collections::BTreeMap;

/// The engine tool that brings back an elided tool result.
pub const READ_RESULT: &str = "read_result";

/// Deliberately pessimistic, since code and non-Latin text use more tokens per character than prose.
pub const CHARS_PER_TOKEN: usize = 3;

/// Tool results this recent are never elided: the model is usually still working with them.
const KEEP_RECENT_RESULTS: usize = 4;
/// Results shorter than this aren't worth a handle.
const MIN_ELIDED_CHARS: usize = 400;
/// Messages at the end kept verbatim when summarising.
const KEEP_TAIL_MESSAGES: usize = 8;

#[must_use]
pub fn read_result_tool() -> ToolDefinition {
    ToolDefinition {
        name: READ_RESULT.to_owned(),
        description: "Show an earlier tool result that was shortened to save space. Give its handle, such as r3, \
                      from the note that replaced it, and the line to start from (offset, from 1) to page through a \
                      long one; each answer says where the next part starts."
            .to_owned(),
        parameters: json!({
            "type": "object",
            "properties": {"handle": {"type": "string"}, "offset": {"type": "integer", "minimum": 1}},
            "required": ["handle"]
        }),
    }
}

/// Characters in a request: the messages plus the tool schemas sent with them.
#[must_use]
pub fn chars_of(messages: &[Message], tools: &[ToolDefinition]) -> usize {
    let mut chars: usize = messages
        .iter()
        .map(|message| {
            message.content.chars().count()
                + message
                    .tool_calls
                    .iter()
                    .map(|call| call.name.len() + call.arguments.to_string().len())
                    .sum::<usize>()
        })
        .sum();
    chars += tools
        .iter()
        .map(|tool| tool.name.len() + tool.description.len() + tool.parameters.to_string().len())
        .sum::<usize>();
    chars
}

/// Estimated tokens for a request at the default ratio.
#[cfg(test)]
pub fn estimate(messages: &[Message], tools: &[ToolDefinition]) -> u64 {
    estimate_with(messages, tools, f64::from(u8::try_from(CHARS_PER_TOKEN).unwrap_or(3)))
}

/// Estimated tokens for a request at `chars_per_token` (a calibrated ratio, or the default).
#[must_use]
pub fn estimate_with(messages: &[Message], tools: &[ToolDefinition], chars_per_token: f64) -> u64 {
    tokens_at(chars_of(messages, tools), chars_per_token)
}

/// Estimated tokens for `chars` characters at `chars_per_token`.
#[must_use]
pub fn tokens_at(chars: usize, chars_per_token: f64) -> u64 {
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let tokens = (chars as f64 / chars_per_token.max(0.5)).floor() as u64;
    tokens
}

/// How many characters a token costs for one model, learnt from what the provider reports. Stored as
/// JSON under the setting `cal:<model>`. Used only after a few samples; the default stands until then.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Calibration {
    pub chars: u64,
    pub tokens: u64,
    pub samples: u32,
}

impl Calibration {
    /// Samples before the learnt ratio is trusted.
    pub const MIN_SAMPLES: u32 = 3;

    /// Learns from one request: how many characters went out, how many tokens the provider counted.
    pub fn add(&mut self, chars: usize, tokens: u64) {
        if tokens == 0 {
            return;
        }
        self.chars = self.chars.saturating_add(u64::try_from(chars).unwrap_or(u64::MAX));
        self.tokens = self.tokens.saturating_add(tokens);
        self.samples = self.samples.saturating_add(1);
    }

    /// Characters per token to estimate with: the learnt ratio within 2–6 once there are enough
    /// samples, otherwise the default.
    #[must_use]
    pub fn ratio(&self) -> f64 {
        if self.samples < Self::MIN_SAMPLES || self.tokens == 0 {
            return f64::from(u8::try_from(CHARS_PER_TOKEN).unwrap_or(3));
        }
        #[allow(clippy::cast_precision_loss)]
        let ratio = self.chars as f64 / self.tokens as f64;
        ratio.clamp(2.0, 6.0)
    }

    /// How the estimate was made, for display next to a `~` figure.
    #[must_use]
    pub fn method(&self, model: &str) -> String {
        if self.samples < Self::MIN_SAMPLES || self.tokens == 0 {
            format!("{CHARS_PER_TOKEN} characters per token")
        } else {
            format!("{:.1} characters per token, calibrated from {} requests on {model}", self.ratio(), self.samples)
        }
    }
}

/// How a message's own text counts: its content plus any tool calls.
fn message_chars(message: &Message) -> usize {
    message.content.chars().count()
        + message.tool_calls.iter().map(|call| call.name.len() + call.arguments.to_string().len()).sum::<usize>()
}

/// What a request holds, by part. The system message is split by `system_parts` (kind and length in
/// characters, in order, as `Chat::set_system_parts` recorded them); anything beyond them is System.
#[must_use]
pub fn ledger(
    messages: &[Message],
    tools: &[ToolDefinition],
    system_parts: &[(ComponentKind, usize)],
    window: &ContextWindow,
    calibration: &Calibration,
    model: &str,
) -> Ledger {
    let ratio = calibration.ratio();
    let mut chars_by_part: BTreeMap<u8, (ComponentKind, usize, usize)> = BTreeMap::new();
    let mut add = |kind: ComponentKind, chars: usize, items: usize| {
        let order = match kind {
            ComponentKind::System => 0,
            ComponentKind::ProjectInstructions => 1,
            ComponentKind::Memory => 2,
            ComponentKind::SkillsIndex => 3,
            ComponentKind::InvokedSkill => 4,
            ComponentKind::McpNote => 5,
            ComponentKind::ToolDefinitions => 6,
            ComponentKind::Conversation => 7,
            ComponentKind::Summary => 8,
            ComponentKind::ToolResults => 9,
            ComponentKind::Elided => 10,
            ComponentKind::Images => 11,
        };
        let entry = chars_by_part.entry(order).or_insert((kind, 0, 0));
        entry.1 += chars;
        entry.2 += items;
    };
    let mut largest: Vec<(String, u64)> = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        match message.role {
            Role::System if index == 0 => {
                let total = message.content.chars().count();
                let mut used = 0;
                for (kind, chars) in system_parts {
                    let take = (*chars).min(total.saturating_sub(used));
                    if take > 0 {
                        add(*kind, take, 1);
                    }
                    used += take;
                }
                if total > used {
                    add(ComponentKind::System, total - used, usize::from(used == 0));
                }
            }
            Role::System | Role::User => add(ComponentKind::Conversation, message_chars(message), 1),
            Role::Assistant => {
                let kind = if message.content.starts_with(SUMMARY_PREFIX) {
                    ComponentKind::Summary
                } else {
                    ComponentKind::Conversation
                };
                add(kind, message_chars(message), 1);
            }
            Role::Tool => {
                let chars = message.content.chars().count();
                if is_elided(&message.content) {
                    add(ComponentKind::Elided, chars, 1);
                } else {
                    add(ComponentKind::ToolResults, chars, 1);
                    largest.push((describe_call(messages, index), tokens_at(chars, ratio)));
                }
            }
        }
        if !message.images.is_empty() {
            add(ComponentKind::Images, 0, message.images.len());
        }
    }
    if !tools.is_empty() {
        let chars: usize =
            tools.iter().map(|tool| tool.name.len() + tool.description.len() + tool.parameters.to_string().len()).sum();
        add(ComponentKind::ToolDefinitions, chars, tools.len());
    }
    largest.sort_by_key(|(_, tokens)| std::cmp::Reverse(*tokens));
    largest.truncate(3);
    let total_chars: usize = chars_by_part.values().map(|(_, chars, _)| *chars).sum();
    let components: Vec<Component> = chars_by_part
        .into_values()
        .map(|(kind, chars, count)| Component { kind, tokens: tokens_at(chars, ratio), count })
        .collect();
    Ledger {
        components,
        // Converted once from the whole, so it agrees with `estimate_with` to the token.
        total: TokenCount::Estimated { tokens: tokens_at(total_chars, ratio), method: calibration.method(model) },
        window: window.window,
        reserve: window.reserve,
        trigger: window.trigger,
        largest_results: largest,
    }
}

/// Hides what looks like a secret in text shown back to the user: bearer tokens, values after a name
/// like `api_key`, `token`, `secret` or `password` (as `name: value`, `name=value` or JSON), and long
/// opaque strings with a known prefix.
#[must_use]
pub fn redact(text: &str) -> String {
    const PREFIXES: &[&str] =
        &["sk-", "ghp_", "gho_", "github_pat_", "xoxb-", "xoxp-", "AKIA", "glpat-", "hf_", "sk-ant-", "sk-or-"];
    let is_break = |c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ',' | ';');
    let mut out = String::with_capacity(text.len());
    // 0: nothing pending; 1: a secret-like name was seen; 2: and a `:` or `=` followed it.
    let mut pending = 0u8;
    for word in text.split_inclusive(is_break) {
        let trimmed = word.trim_end_matches(is_break);
        let tail = &word[trimmed.len()..];
        if trimmed.is_empty() {
            out.push_str(word);
            continue;
        }
        if pending == 2 {
            out.push_str("[redacted]");
            out.push_str(tail);
            pending = 0;
            continue;
        }
        if pending == 1 {
            pending = if trimmed == ":" || trimmed == "=" { 2 } else { 0 };
            if pending == 2 {
                out.push_str(word);
                continue;
            }
        }
        let lower = trimmed.to_ascii_lowercase();
        if lower == "bearer" {
            out.push_str(word);
            pending = 2;
            continue;
        }
        if PREFIXES.iter().any(|p| trimmed.starts_with(p) && trimmed.len() >= p.len() + 8) {
            out.push_str("[redacted]");
            out.push_str(tail);
            continue;
        }
        match trimmed.find([':', '=']) {
            Some(at) if is_secret_name(&trimmed[..at]) => {
                let value = &trimmed[at + 1..];
                if value.is_empty() {
                    out.push_str(word);
                    pending = 2;
                } else {
                    out.push_str(&trimmed[..=at]);
                    out.push_str("[redacted]");
                    out.push_str(tail);
                }
                continue;
            }
            None if is_secret_name(trimmed) => pending = 1,
            _ => {}
        }
        out.push_str(word);
    }
    out
}

/// Whether a name's last part says it holds a secret.
fn is_secret_name(name: &str) -> bool {
    let name = name.trim_matches(|c: char| c == '"' || c == '\'');
    let last = name.rsplit(['_', '-', '.']).next().unwrap_or(name).to_ascii_lowercase();
    matches!(last.as_str(), "key" | "apikey" | "secret" | "password" | "token" | "authorization" | "bearer")
}

/// When compaction starts and what it aims for, from the context window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// The most a request may hold: the window minus what's reserved for the reply.
    pub budget: u64,
    /// Compaction starts here (estimated tokens); `None` means only on request.
    pub trigger: Option<u64>,
}

impl Limits {
    /// The limits for a window, or `None` when the window is unknown and nothing can be budgeted.
    #[must_use]
    pub fn for_window(window: &recode_core::context::ContextWindow) -> Option<Self> {
        Some(Self { budget: window.budget()?, trigger: window.trigger })
    }

    /// Elision alone is enough if it gets below this; otherwise older messages are summarised.
    #[must_use]
    pub fn target(self) -> u64 {
        self.budget / 2
    }
}

/// Lines `from` (1-based) onwards of `text`, at most `max_chars`, and the line after the last one
/// given when the text goes on.
#[must_use]
pub fn page(text: &str, from: usize, max_chars: usize) -> (String, usize, Option<usize>) {
    let total = text.lines().count();
    let mut out = String::new();
    let mut last = from.saturating_sub(1);
    for (index, line) in text.lines().enumerate().skip(from.saturating_sub(1)) {
        if out.chars().count() + line.chars().count() + 1 > max_chars && last >= from {
            break;
        }
        // A single line longer than the whole allowance is cut, not skipped.
        let line: String = line.chars().take(max_chars).collect();
        out.push_str(&line);
        out.push('\n');
        last = index + 1;
    }
    (out, total, (last < total).then_some(last + 1))
}

fn is_clipped(content: &str) -> bool {
    content.contains("to fit the context window. The rest is result r")
}

/// Cuts every tool result longer than `max_chars` to its first lines, keeping the whole of it under a
/// handle the model can page through with `read_result`. A single result never takes more than its
/// share of the window this way, however recent. Returns how many were cut.
pub fn clip(messages: &mut [Message], elided: &mut BTreeMap<String, String>, max_chars: usize) -> usize {
    let mut count = 0;
    for message in messages.iter_mut().filter(|m| m.role == Role::Tool) {
        if message.content.chars().count() <= max_chars || is_elided(&message.content) || is_clipped(&message.content) {
            continue;
        }
        let handle = format!("r{}", elided.len() + 1);
        let original = std::mem::take(&mut message.content);
        let (head, total, next) = page(&original, 1, max_chars.saturating_sub(300).max(200));
        let next = next.unwrap_or(total);
        message.content = format!(
            "{head}[re-code cut this result at line {} of {total} to fit the context window. The rest is result \
             {handle}: call read_result with \"{handle}\" and offset {next}. For a file, reading the part you need \
             (read with offset and limit) is cheaper.]",
            next.saturating_sub(1)
        );
        elided.insert(handle, original);
        count += 1;
    }
    count
}

/// Replaces old, long tool results with handles. Returns how many were elided.
pub fn elide(messages: &mut [Message], elided: &mut BTreeMap<String, String>) -> usize {
    elide_keeping(messages, elided, KEEP_RECENT_RESULTS)
}

/// [`elide`], keeping the `keep` most recent results whole.
pub fn elide_keeping(messages: &mut [Message], elided: &mut BTreeMap<String, String>, keep: usize) -> usize {
    let results: Vec<usize> =
        messages.iter().enumerate().filter(|(_, m)| m.role == Role::Tool).map(|(index, _)| index).collect();
    let old = results.len().saturating_sub(keep);
    let mut count = 0;
    for &index in &results[..old] {
        let message = &messages[index];
        if message.content.chars().count() < MIN_ELIDED_CHARS || is_elided(&message.content) {
            continue;
        }
        let what = describe_call(messages, index);
        let lines = message.content.lines().count();
        let handle = format!("r{}", elided.len() + 1);
        let original = std::mem::take(&mut messages[index].content);
        messages[index].content = format!(
            "[result {handle} elided to save space: {what}, {lines} lines. Call read_result with \"{handle}\" to see it again.]"
        );
        elided.insert(handle, original);
        count += 1;
    }
    count
}

fn is_elided(content: &str) -> bool {
    content.starts_with("[result r") && content.contains("elided to save space")
}

/// "shell cargo test" for the call a tool result answers.
fn describe_call(messages: &[Message], result: usize) -> String {
    let message = &messages[result];
    let name = message.tool_name.clone().unwrap_or_else(|| "tool".to_owned());
    let call = message
        .tool_call_id
        .as_ref()
        .and_then(|id| messages[..result].iter().rev().flat_map(|m| m.tool_calls.iter()).find(|call| &call.id == id));
    let target = call.map(|call| recode_tools::target_of(&call.name, &call.arguments)).unwrap_or_default();
    let target: String = target.chars().take(80).collect();
    if target.is_empty() { name } else { format!("{name} {target}") }
}

/// The messages a summary would replace, and the one kept verbatim inside that span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    /// The first user message (the objective); everything up to and including it is kept.
    pub objective: usize,
    /// Where the verbatim tail starts.
    pub tail: usize,
    /// The current request, if it falls inside the span; it's kept and placed after the summary.
    pub current: Option<usize>,
}

/// What could be summarised, or `None` if there's too little to gain.
#[must_use]
pub fn summary_region(messages: &[Message]) -> Option<Region> {
    let objective = messages.iter().position(|m| m.role == Role::User)?;
    let mut tail = messages.len().saturating_sub(KEEP_TAIL_MESSAGES);
    // Never start the tail on a tool result: it must stay with the call that asked for it.
    while tail > objective + 1 && messages[tail].role == Role::Tool {
        tail -= 1;
    }
    let current = messages.iter().rposition(|m| m.role == Role::User).filter(|&last| last > objective && last < tail);
    let span = tail.saturating_sub(objective + 1) - usize::from(current.is_some());
    (span >= 2).then_some(Region { objective, tail, current })
}

/// The span to summarise, written out plainly for the summarising model, within `max_chars`.
#[must_use]
pub fn render(messages: &[Message], region: &Region, max_chars: usize) -> String {
    let mut lines = Vec::new();
    for (index, message) in messages.iter().enumerate().take(region.tail).skip(region.objective + 1) {
        if Some(index) == region.current {
            continue;
        }
        let text: String = message.content.chars().take(2000).collect();
        match message.role {
            Role::User => lines.push(format!("User: {text}")),
            Role::Assistant => {
                if !text.trim().is_empty() {
                    lines.push(format!("Assistant: {text}"));
                }
                for call in &message.tool_calls {
                    let args: String = call.arguments.to_string().chars().take(300).collect();
                    lines.push(format!("Assistant called {}({args})", call.name));
                }
            }
            Role::Tool => {
                let name = message.tool_name.as_deref().unwrap_or("tool");
                let text: String = text.chars().take(600).collect();
                lines.push(format!("Result of {name}: {text}"));
            }
            Role::System => lines.push(format!("Note: {text}")),
        }
    }
    let mut rendered = lines.join("\n");
    if rendered.chars().count() > max_chars {
        // Keep the most recent part; the summary matters most for where things stand now.
        let skip = rendered.chars().count() - max_chars;
        rendered = format!("[the earliest part is omitted]\n{}", rendered.chars().skip(skip).collect::<String>());
    }
    rendered
}

/// Instructions for the summarising call.
pub const SUMMARY_SYSTEM: &str = "You compress a coding session so it can continue in less space. Write only the summary, under these headings: Objective (in the user's own words), Decisions and why, Findings, Unfinished work, Files touched, Unresolved errors. Keep file paths, function names, commands and error messages exactly as written. Don't invent anything, and leave a heading empty rather than guessing.";

/// Replaces the region with the summary, keeping the objective, the current request and the tail.
/// The instructions for the summarising request: the fixed headings, then what the user asked this
/// compaction to focus on, then the project's own "Compact instructions" section.
#[must_use]
pub fn summary_system(focus: Option<&str>, instructions: Option<&str>) -> String {
    let mut text = SUMMARY_SYSTEM.to_owned();
    if let Some(focus) = focus.map(str::trim).filter(|f| !f.is_empty()) {
        text.push_str("\n\nThe user asked this compaction to focus on: ");
        text.push_str(focus);
        text.push_str(". Keep everything about that in full detail.");
    }
    if let Some(instructions) = instructions.map(str::trim).filter(|i| !i.is_empty()) {
        text.push_str("\n\nThe project's own instructions for compaction:\n");
        text.push_str(instructions);
    }
    text
}

/// How a compaction summary message begins, so it can be told apart later.
pub const SUMMARY_PREFIX: &str =
    "Summary of the conversation and work so far, written by re-code when compacting it to fit the context:";

#[cfg(test)]
pub fn apply_summary(messages: &mut Vec<Message>, region: &Region, summary: &str) -> usize {
    apply_summary_keeping(messages, region, summary).len()
}

/// As [`apply_summary`], returning the messages the summary replaced so they can be kept.
#[must_use]
pub fn apply_summary_keeping(messages: &mut Vec<Message>, region: &Region, summary: &str) -> Vec<Message> {
    let current = region.current.map(|index| messages[index].clone());
    let tail = messages.split_off(region.tail);
    let mut removed = messages.split_off(region.objective + 1);
    if let Some(current) = region.current {
        // The current request is kept, not replaced.
        let index = current - (region.objective + 1);
        if index < removed.len() {
            removed.remove(index);
        }
    }
    messages.push(Message::assistant(format!("{SUMMARY_PREFIX}\n\n{}", summary.trim()), Vec::new()));
    messages.extend(current);
    messages.extend(tail);
    removed
}

/// Messages as plain text, one block each, for reading what a compaction replaced.
#[must_use]
pub fn render_messages(messages: &[Message]) -> String {
    let mut out = String::new();
    for message in messages {
        let role = match message.role {
            Role::System => "System",
            Role::User => "User",
            Role::Assistant => "Assistant",
            Role::Tool => "Tool result",
        };
        out.push_str(&format!("── {role} ──\n"));
        if !message.content.is_empty() {
            out.push_str(&message.content);
            out.push('\n');
        }
        for call in &message.tool_calls {
            out.push_str(&format!("[called {}({})]\n", call.name, call.arguments));
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use recode_core::message::ToolCall;

    fn call(id: &str, command: &str) -> ToolCall {
        ToolCall { id: id.to_owned(), name: "shell".to_owned(), arguments: json!({"command": command}) }
    }

    /// One request followed by many tool steps, the usual way an agent turn grows.
    fn long_turn(steps: usize) -> Vec<Message> {
        let mut messages = vec![Message::system("sys"), Message::user("make the tests pass")];
        for step in 0..steps {
            let c = call(&format!("c{step}"), &format!("cargo test step{step}"));
            messages.push(Message::assistant("", vec![c.clone()]));
            messages.push(Message::tool_result(&c, format!("line\n{}", "x".repeat(900))));
        }
        messages
    }

    #[test]
    fn old_results_in_a_single_long_turn_are_elided_and_recent_ones_kept() {
        let mut messages = long_turn(10);
        let mut elided = BTreeMap::new();
        let before = estimate(&messages, &[]);
        assert_eq!(elide(&mut messages, &mut elided), 6, "all but the 4 most recent results");
        assert!(estimate(&messages, &[]) < before / 2);
        let first = messages.iter().find(|m| m.role == Role::Tool).unwrap();
        assert!(first.content.contains("r1") && first.content.contains("shell cargo test step0"), "{}", first.content);
        assert!(elided["r1"].starts_with("line\n"), "the original is kept");
        let last = messages.last().unwrap();
        assert!(last.content.starts_with("line\n"), "recent results stay verbatim");
        // Running it again changes nothing: handles aren't elided twice.
        assert_eq!(elide(&mut messages, &mut elided), 0);
    }

    #[test]
    fn the_tail_never_begins_with_an_orphaned_tool_result() {
        let messages = long_turn(10);
        let region = summary_region(&messages).unwrap();
        assert_eq!(region.objective, 1);
        assert_ne!(messages[region.tail].role, Role::Tool);
        assert_eq!(region.current, None, "the only request is the objective");
    }

    #[test]
    fn a_summary_keeps_the_objective_the_current_request_and_the_recent_steps() {
        let mut messages = long_turn(6);
        messages.push(Message::user("now also update the docs"));
        for step in 0..4 {
            let c = call(&format!("d{step}"), "ls docs");
            messages.push(Message::assistant("", vec![c.clone()]));
            messages.push(Message::tool_result(&c, "README.md"));
        }
        let region = summary_region(&messages).unwrap();
        let current = region.current.expect("the newest request falls inside the span here");
        assert_eq!(messages[current].content, "now also update the docs");

        let tail_len = messages.len() - region.tail;
        let removed = apply_summary(&mut messages, &region, "Objective: make the tests pass");
        assert!(removed > 0);
        assert_eq!(messages[1].content, "make the tests pass", "the objective, verbatim");
        assert!(messages[2].content.contains("Objective: make the tests pass"));
        assert_eq!(messages[2].role, Role::Assistant);
        assert_eq!(messages[3].content, "now also update the docs");
        assert_eq!(messages.len(), 4 + tail_len);
        // Every tool result still follows the call that asked for it.
        for (index, message) in messages.iter().enumerate().filter(|(_, m)| m.role == Role::Tool) {
            let id = message.tool_call_id.as_ref().unwrap();
            assert!(messages[..index].iter().any(|m| m.tool_calls.iter().any(|c| &c.id == id)));
        }
    }

    #[test]
    fn a_short_conversation_has_nothing_to_summarise() {
        let messages = vec![Message::system("s"), Message::user("hi"), Message::assistant("hello", Vec::new())];
        assert_eq!(summary_region(&messages), None);
    }

    #[test]
    fn rendering_fits_the_limit_and_keeps_the_latest_part() {
        let messages = long_turn(10);
        let region = summary_region(&messages).unwrap();
        let rendered = render(&messages, &region, 500);
        assert!(rendered.starts_with("[the earliest part is omitted]"));
        assert!(rendered.chars().count() < 600);
    }

    #[test]
    fn the_ledger_splits_the_request_by_part_and_names_the_largest_results() {
        let system = format!("{}\n\n{}\n\n{}", "s".repeat(300), "k".repeat(600), "m".repeat(150));
        let parts = [(ComponentKind::System, 300), (ComponentKind::SkillsIndex, 602), (ComponentKind::McpNote, 152)];
        let mut messages = vec![Message::system(system), Message::user("fix the bug")];
        let call = recode_core::message::ToolCall {
            id: "c1".into(),
            name: "read".into(),
            arguments: json!({"path": "src/lib.rs"}),
        };
        messages.push(Message::assistant("", vec![call.clone()]));
        messages.push(Message::tool_result(&call, "x".repeat(900)));
        let mut elided = BTreeMap::new();
        // A handle counts as shortened, not as a result.
        messages.push(Message::tool_result(&call, "y".repeat(500)));
        messages.push(Message::tool_result(&call, "z".repeat(500)));
        messages.push(Message::tool_result(&call, "w".repeat(500)));
        messages.push(Message::tool_result(&call, "v".repeat(500)));
        messages.push(Message::tool_result(&call, "u".repeat(500)));
        assert_eq!(elide(&mut messages, &mut elided), 2);
        messages.push(Message::assistant(format!("{SUMMARY_PREFIX}\n\nObjective: it."), Vec::new()));
        let tools = vec![ToolDefinition {
            name: "read".into(),
            description: "d".repeat(200),
            parameters: json!({"type": "object", "properties": {"path": {"type": "string"}}}),
        }];
        let window = ContextWindow::resolve(&recode_core::context::WindowSetting::default(), Some(8192), true, 8192);
        let ledger = ledger(&messages, &tools, &parts, &window, &Calibration::default(), "m");
        let by_kind =
            |kind: ComponentKind| ledger.components.iter().find(|c| c.kind == kind).map(|c| (c.tokens, c.count));
        assert_eq!(by_kind(ComponentKind::System), Some((100, 1)));
        assert_eq!(by_kind(ComponentKind::SkillsIndex), Some((200, 1)));
        assert_eq!(by_kind(ComponentKind::McpNote), Some((50, 1)));
        assert_eq!(by_kind(ComponentKind::Elided).map(|(_, count)| count), Some(2));
        assert_eq!(by_kind(ComponentKind::ToolResults).map(|(_, count)| count), Some(4));
        assert!(by_kind(ComponentKind::Summary).is_some_and(|(tokens, _)| tokens > 0));
        assert!(by_kind(ComponentKind::ToolDefinitions).is_some_and(|(tokens, count)| tokens > 0 && count == 1));
        assert_eq!(ledger.tokens(), estimate(&messages, &tools), "the parts add up to the whole");
        assert_eq!(ledger.largest_results.len(), 3);
        assert!(ledger.largest_results[0].0.starts_with("read src/lib.rs"), "{:?}", ledger.largest_results);
        assert_eq!((ledger.window, ledger.reserve), (Some(8192), 1024));
        assert!(matches!(ledger.total, TokenCount::Estimated { .. }));
        // The same input twice gives the same ledger: the parts are stable for the prompt cache.
        assert_eq!(ledger, super::ledger(&messages, &tools, &parts, &window, &Calibration::default(), "m"));
    }

    #[test]
    fn the_replaced_messages_are_kept_and_the_current_request_is_not_among_them() {
        let mut messages = vec![Message::system("s"), Message::user("build the parser")];
        for n in 0..12 {
            messages.push(Message::assistant(format!("step {n}"), Vec::new()));
            messages.push(Message::user(format!("then {n}")));
        }
        messages.push(Message::user("current request"));
        for n in 0..4 {
            messages.push(Message::assistant(format!("recent {n}"), Vec::new()));
        }
        let region = summary_region(&messages).expect("long enough");
        let before = messages.clone();
        let removed = apply_summary_keeping(&mut messages, &region, "Objective: parser.");
        assert!(!removed.is_empty());
        assert!(removed.iter().all(|m| m.content != "current request"), "the current request is kept, not replaced");
        assert!(messages.iter().any(|m| m.content == "current request"));
        assert_eq!(messages.len() + removed.len(), before.len() + 1, "everything is either kept or in the originals");
        let text = render_messages(&removed);
        assert!(text.contains("── Assistant ──\nstep 0") && text.contains("── User ──\nthen 0"), "{text}");
    }

    #[test]
    fn the_ratio_is_learnt_from_reported_usage_and_bounded() {
        let mut calibration = Calibration::default();
        assert_eq!(calibration.ratio(), 3.0, "the default until there are samples");
        calibration.add(4000, 1000);
        calibration.add(4000, 1000);
        assert_eq!(calibration.ratio(), 3.0, "two samples aren't enough");
        calibration.add(4000, 1000);
        assert_eq!(calibration.ratio(), 4.0);
        assert!(calibration.method("m").contains("calibrated from 3 requests on m"));
        let mut wild = Calibration::default();
        for _ in 0..3 {
            wild.add(100_000, 1000);
        }
        assert_eq!(wild.ratio(), 6.0, "clamped above");
        let mut dense = Calibration::default();
        for _ in 0..3 {
            dense.add(1000, 1000);
        }
        assert_eq!(dense.ratio(), 2.0, "clamped below");
        assert_eq!(tokens_at(1200, 4.0), 300);
        let messages = vec![Message::user("x".repeat(3000))];
        assert_eq!(estimate_with(&messages, &[], 3.0), 1000);
        assert_eq!(estimate_with(&messages, &[], 4.0), 750);
    }

    #[test]
    fn the_summary_instructions_carry_the_focus_and_the_project_section() {
        let plain = summary_system(None, None);
        assert_eq!(plain, SUMMARY_SYSTEM);
        let focused = summary_system(Some(" the migration plan "), Some("Keep exact cargo commands."));
        assert!(focused.starts_with(SUMMARY_SYSTEM));
        assert!(focused.contains("focus on: the migration plan."), "{focused}");
        assert!(focused.ends_with("Keep exact cargo commands."), "{focused}");
        assert_eq!(summary_system(Some("  "), Some("")), SUMMARY_SYSTEM, "blank parts add nothing");
    }

    #[test]
    fn secrets_are_hidden_in_the_request_preview() {
        let text = r#"{"authorization": "Bearer sk-live-abcdefghijklmnop", "api_key": "AKIAABCDEFGHIJKLMNOP", "note": "token: ghp_0123456789abcdefghijklmnop", "path": "src/main.rs"}"#;
        let redacted = redact(text);
        assert!(
            !redacted.contains("sk-live") && !redacted.contains("AKIA") && !redacted.contains("ghp_"),
            "{redacted}"
        );
        assert!(redacted.contains("src/main.rs") && redacted.contains("[redacted]"), "{redacted}");
        assert_eq!(redact("plain words with a key: hidden"), "plain words with a key: [redacted]");
        assert_eq!(redact("nothing secret here"), "nothing secret here");
    }

    #[test]
    fn tool_schemas_count_towards_the_estimate() {
        let messages = vec![Message::user("hi")];
        assert!(estimate(&messages, &recode_tools::tool_definitions()) > estimate(&messages, &[]) + 100);
    }
}
