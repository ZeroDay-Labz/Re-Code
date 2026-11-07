//! `re-code -p "prompt"`: one request without the interface, for scripts and pipes. The same driver
//! runs; this client reads its events and prints the result as text, one JSON object, or one JSON event
//! per line. Nothing is approved on the user's behalf: a tool that would need approval is denied
//! (and named on stderr) unless `--allowed-tools` lists it, and the run then ends with exit code 2.

use recode_core::protocol::{ApprovalDecision, EventKind, PermissionMode, TurnOutcome};
use recode_core::usage::ReportedUsage;
use recode_tui::link::{Link, UiCommand, UiEvent};
use serde::Serialize;
use std::collections::HashMap;
use std::io::Write;

/// How the result is printed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum OutputFormat {
    /// The final answer only.
    Text,
    /// One JSON object with the answer, the tool calls and the provider's figures.
    Json,
    /// Every session event as a JSON line, as it happens.
    StreamJson,
}

/// What the command line asked for.
#[derive(Debug, Clone)]
pub struct Options {
    pub prompt: String,
    pub format: OutputFormat,
    pub model: Option<String>,
    pub permission_mode: Option<PermissionMode>,
    /// Tools that may run without asking (by name); everything else that would ask is denied.
    pub allowed_tools: Vec<String>,
}

/// Exit codes, as documented in `--help`.
pub const EXIT_OK: i32 = 0;
pub const EXIT_FAILED: i32 = 1;
pub const EXIT_DENIED: i32 = 2;
pub const EXIT_NO_MODEL: i32 = 3;

/// One tool call, for the JSON result.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct ToolCallSummary {
    pub name: String,
    pub target: String,
    pub success: Option<bool>,
    pub denied: bool,
}

/// The JSON result. Reported figures and estimates are kept apart.
#[derive(Debug, Clone, Default, Serialize)]
pub struct JsonResult {
    pub outcome: String,
    pub result: String,
    pub model: String,
    pub turns: usize,
    pub tool_calls: Vec<ToolCallSummary>,
    /// Summed over the run, as the provider reported it (`None` when it reported nothing).
    pub usage: Option<ReportedUsage>,
    /// What re-code estimated the last request held, and the window it was budgeted in.
    pub context: Option<ContextFigures>,
    pub notices: Vec<String>,
    pub denied: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ContextFigures {
    pub estimated_tokens: u64,
    pub window: Option<u64>,
}

/// Collects what the events say into a result.
#[derive(Debug, Default)]
pub struct Collector {
    pub result: JsonResult,
    text: String,
    calls: HashMap<String, usize>,
    turns_open: usize,
    finished: Option<TurnOutcome>,
}

impl Collector {
    /// Takes one event. Returns `Some(approval)` when something waits for an answer.
    pub fn take(&mut self, event: &EventKind) -> Option<(recode_core::ids::ApprovalId, String)> {
        match event {
            EventKind::TurnStarted { .. } => {
                self.turns_open += 1;
                self.result.turns += 1;
                self.text.clear();
            }
            EventKind::TextDelta { text } => self.text.push_str(text),
            EventKind::ToolCallRequested { call } => {
                self.calls.insert(call.id.clone(), self.result.tool_calls.len());
                self.result.tool_calls.push(ToolCallSummary {
                    name: call.name.clone(),
                    target: recode_tools::target_of(&call.name, &call.arguments),
                    success: None,
                    denied: false,
                });
            }
            EventKind::ToolFinished { call_id, success, .. } => {
                if let Some(index) = self.calls.get(call_id)
                    && let Some(call) = self.result.tool_calls.get_mut(*index)
                {
                    call.success = Some(*success);
                }
            }
            EventKind::ApprovalRequired { approval, call_id, description, .. } => {
                let name = self
                    .calls
                    .get(call_id)
                    .and_then(|index| self.result.tool_calls.get(*index))
                    .map_or_else(|| description.clone(), |call| call.name.clone());
                return Some((*approval, name));
            }
            EventKind::ApprovalResolved { decision: ApprovalDecision::Deny, .. } => {
                if let Some(call) = self.result.tool_calls.iter_mut().rev().find(|call| call.success.is_none()) {
                    call.denied = true;
                }
            }
            EventKind::UsageReported { usage } => {
                let total = self.result.usage.get_or_insert_with(ReportedUsage::default);
                let add = |slot: &mut Option<u64>, more: Option<u64>| {
                    if let Some(more) = more {
                        *slot = Some(slot.unwrap_or(0) + more);
                    }
                };
                add(&mut total.prompt_tokens, usage.prompt_tokens);
                add(&mut total.cached_prompt_tokens, usage.cached_prompt_tokens);
                add(&mut total.output_tokens, usage.output_tokens);
            }
            EventKind::ContextLedger { ledger } => {
                self.result.context = Some(ContextFigures { estimated_tokens: ledger.tokens(), window: ledger.window });
            }
            EventKind::TurnFinished { outcome, .. } => {
                self.turns_open = self.turns_open.saturating_sub(1);
                self.finished = Some(outcome.clone());
                self.result.result = self.text.trim().to_owned();
                self.result.outcome = match outcome {
                    TurnOutcome::Completed => "completed".to_owned(),
                    TurnOutcome::Interrupted => "interrupted".to_owned(),
                    TurnOutcome::Failed { reason } => format!("failed: {reason}"),
                };
            }
            _ => {}
        }
        None
    }

    /// The outcome once the turn has finished.
    #[must_use]
    pub fn finished(&self) -> Option<&TurnOutcome> {
        self.finished.as_ref()
    }
}

/// Runs the prompt and prints the result. Returns the exit code.
pub async fn run(mut link: Link, options: Options) -> i32 {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut collector = Collector::default();
    let mut model = String::new();
    let mut chosen = false;
    let mut sent = false;
    let mut denied_any = false;
    let mut err = std::io::stderr();
    while let Some(event) = link.events.recv().await {
        match event {
            UiEvent::Welcome(info) if !chosen => {
                let wanted = options.model.clone().or(info.last_model.clone());
                let Some(wanted) = wanted else {
                    let _ = writeln!(err, "No model: pass --model NAME (one of: {}).", names(&info.models));
                    return EXIT_NO_MODEL;
                };
                if !info.models.iter().any(|m| m.name == wanted) {
                    let _ = writeln!(err, "Model {wanted} isn't available. Known: {}.", names(&info.models));
                    return EXIT_NO_MODEL;
                }
                model = wanted.clone();
                chosen = true;
                let _ = link.commands.send(UiCommand::ChooseModel(wanted));
                if let Some(mode) = options.permission_mode {
                    let _ = link.commands.send(UiCommand::SetPermissionMode(mode));
                }
                let _ = link.commands.send(UiCommand::Send { text: options.prompt.clone(), images: Vec::new() });
                sent = true;
            }
            UiEvent::Notice(text) => {
                if options.format == OutputFormat::Json {
                    collector.result.notices.push(text);
                } else {
                    let _ = writeln!(err, "{text}");
                }
            }
            UiEvent::Session(kind) => {
                if options.format == OutputFormat::StreamJson
                    && let Ok(line) = serde_json::to_string(&kind)
                {
                    let _ = writeln!(out, "{line}");
                }
                if let Some((approval, name)) = collector.take(&kind) {
                    let allowed = options.allowed_tools.contains(&name);
                    let decision = if allowed { ApprovalDecision::AllowOnce } else { ApprovalDecision::Deny };
                    if !allowed {
                        denied_any = true;
                        collector.result.denied.push(name.clone());
                        let _ = writeln!(
                            err,
                            "Denied {name}: it would need approval. Allow it with --allowed-tools {name}, or run interactively."
                        );
                    }
                    let _ = link.commands.send(UiCommand::ResolveApproval(approval, decision));
                }
                if sent && collector.finished().is_some() {
                    break;
                }
            }
            _ => {}
        }
    }
    collector.result.model = model;
    match options.format {
        OutputFormat::Text => {
            let _ = writeln!(out, "{}", collector.result.result);
        }
        OutputFormat::Json => {
            let _ = writeln!(out, "{}", serde_json::to_string_pretty(&collector.result).unwrap_or_default());
        }
        OutputFormat::StreamJson => {}
    }
    let _ = out.flush();
    match collector.finished() {
        Some(TurnOutcome::Completed) if denied_any => EXIT_DENIED,
        Some(TurnOutcome::Completed) => EXIT_OK,
        _ => EXIT_FAILED,
    }
}

fn names(models: &[recode_tui::welcome::WelcomeModel]) -> String {
    let mut names: Vec<&str> = models.iter().map(|m| m.name.as_str()).collect();
    names.truncate(12);
    if names.is_empty() { "none".to_owned() } else { names.join(", ") }
}

#[cfg(test)]
mod tests {
    use super::*;
    use recode_core::ids::{ApprovalId, TurnId};
    use recode_core::message::ToolCall;
    use serde_json::json;

    #[test]
    fn the_collector_sums_up_a_turn_and_surfaces_approvals() {
        let mut collector = Collector::default();
        let turn = TurnId::new();
        collector.take(&EventKind::TurnStarted { turn, text: "fix it".into() });
        let call = ToolCall { id: "c1".into(), name: "shell".into(), arguments: json!({"command": "cargo test"}) };
        collector.take(&EventKind::ToolCallRequested { call });
        let approval = ApprovalId::new();
        let asked = collector.take(&EventKind::ApprovalRequired {
            approval,
            call_id: "c1".into(),
            description: "Run cargo test".into(),
            diff: None,
            always: None,
        });
        assert_eq!(asked, Some((approval, "shell".to_owned())));
        collector.take(&EventKind::ApprovalResolved { approval, decision: ApprovalDecision::Deny });
        collector.take(&EventKind::ToolFinished { call_id: "c1".into(), success: false, summary: "denied".into() });
        collector.take(&EventKind::TextDelta { text: "I couldn't ".into() });
        collector.take(&EventKind::TextDelta { text: "run the tests.".into() });
        collector.take(&EventKind::UsageReported {
            usage: ReportedUsage { prompt_tokens: Some(100), output_tokens: Some(10), ..ReportedUsage::default() },
        });
        collector.take(&EventKind::UsageReported {
            usage: ReportedUsage { prompt_tokens: Some(50), output_tokens: Some(5), ..ReportedUsage::default() },
        });
        collector.take(&EventKind::TurnFinished { turn, outcome: TurnOutcome::Completed });
        let result = &collector.result;
        assert_eq!(result.result, "I couldn't run the tests.");
        assert_eq!(result.outcome, "completed");
        assert_eq!(result.turns, 1);
        assert_eq!(
            result.tool_calls[0],
            ToolCallSummary { name: "shell".into(), target: "cargo test".into(), success: Some(false), denied: true }
        );
        assert_eq!(result.usage.as_ref().unwrap().prompt_tokens, Some(150));
        assert_eq!(result.usage.as_ref().unwrap().output_tokens, Some(15));
        let json = serde_json::to_value(result).unwrap();
        assert!(json.get("usage").is_some() && json.get("context").is_some(), "{json}");
    }
}
