//! The boundary between the engine and its clients.
//!
//! Clients send [`Command`]s and receive [`EventEnvelope`]s. Both serialise cleanly, so the
//! store can replay them and a future socket or ACP client needs no new engine design.

use crate::ids::{ApprovalId, SessionId, TurnId, UnixMillis};
use crate::message::ToolCall;
use crate::usage::{Ledger, ReportedUsage, TokenCount};
use serde::{Deserialize, Serialize};

/// A client's request to the engine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum Command {
    SendMessage { session: SessionId, text: String },
    Interrupt { session: SessionId },
    ResolveApproval { session: SessionId, approval: ApprovalId, decision: ApprovalDecision },
}

/// A user's answer to a permission request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    AllowOnce,
    AllowForSession,
    /// Allow, and remember it for this project across sessions.
    AllowForProject,
    Deny,
}

/// How much the agent may do without asking (`docs/SECURITY.md`, Permissions). Cycled with Shift+Tab.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    /// Every change, command and reach outside the project is asked (the default).
    #[default]
    Ask,
    /// Edits and new files inside the project run without asking; everything else is asked.
    AcceptEdits,
    /// Reading tools only; the agent writes a plan instead of changing anything.
    Plan,
    /// Anything the sandbox contains runs without asking; secrets and unsandboxed commands still ask.
    AutoSandbox,
}

impl PermissionMode {
    pub const ALL: [Self; 4] = [Self::Ask, Self::AcceptEdits, Self::Plan, Self::AutoSandbox];

    /// The next mode in the Shift+Tab cycle.
    #[must_use]
    pub fn next(self) -> Self {
        let at = Self::ALL.iter().position(|mode| *mode == self).unwrap_or(0);
        Self::ALL[(at + 1) % Self::ALL.len()]
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::AcceptEdits => "accept edits",
            Self::Plan => "plan",
            Self::AutoSandbox => "auto (sandbox)",
        }
    }

    /// One sentence on what the mode does.
    #[must_use]
    pub fn describe(self) -> &'static str {
        match self {
            Self::Ask => "Every change, command and reach outside the project is asked first.",
            Self::AcceptEdits => {
                "Edits and new files inside the project run without asking; deletes, commands and anything outside still ask."
            }
            Self::Plan => {
                "Reading tools only: the agent explores and writes a plan (/plan shows it) instead of changing anything."
            }
            Self::AutoSandbox => {
                "Anything the sandbox contains runs without asking; files that look like secrets and commands outside the sandbox still ask."
            }
        }
    }
}

/// What an agent is doing. These are separate because a local model can spend a long time in
/// each, and a single spinner can't tell the user whether anything is wrong.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum AgentState {
    Idle,
    LoadingModel,
    EvaluatingPrompt,
    Thinking,
    Responding,
    RunningTool {
        name: String,
    },
    AwaitingApproval,
    Stalled {
        silent_for_secs: u64,
    },
    /// Shrinking the conversation to fit the model's context.
    Compacting,
}

/// One task in the model's own list of what it's doing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoItem {
    pub text: String,
    pub status: TodoStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Done,
}

/// How a turn ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "outcome")]
pub enum TurnOutcome {
    Completed,
    Interrupted,
    Failed { reason: String },
}

/// Something that happened. Written to the store before any client sees it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum EventKind {
    TurnStarted {
        turn: TurnId,
        text: String,
    },
    StateChanged {
        state: AgentState,
    },
    TextDelta {
        text: String,
    },
    ReasoningDelta {
        text: String,
    },
    ToolCallRequested {
        call: ToolCall,
    },
    ToolOutput {
        call_id: String,
        text: String,
    },
    ToolFinished {
        call_id: String,
        success: bool,
        summary: String,
    },
    ApprovalRequired {
        approval: ApprovalId,
        call_id: String,
        description: String,
        diff: Option<String>,
        /// What answering "always" would allow, in words; `None` when it isn't offered.
        #[serde(default)]
        always: Option<String>,
    },
    ApprovalResolved {
        approval: ApprovalId,
        decision: ApprovalDecision,
    },
    UsageReported {
        usage: ReportedUsage,
    },
    ContextMeasured {
        used: TokenCount,
        limit: u64,
    },
    /// What the request about to go out holds, by part (estimated).
    ContextLedger {
        ledger: Ledger,
    },
    /// The user switched how much the agent may do without asking.
    PermissionModeChanged {
        mode: PermissionMode,
    },
    /// A message typed while the agent worked reached the model mid-turn, after a batch of tool calls.
    QueuedDelivered {
        text: String,
    },
    /// One of the user's hooks ran.
    HookRan {
        name: String,
        /// `pre_tool_use`, `post_tool_use`, `stop` or `session_start`.
        event: String,
        ok: bool,
        summary: String,
    },
    /// The conversation was wound back to before request number `to_turn` (from 0). `files` are the
    /// paths put back; `summary` is kept in place of what was removed, when asked for.
    Rewound {
        to_turn: usize,
        files: Vec<String>,
        summary: Option<String>,
    },
    TurnFinished {
        turn: TurnId,
        outcome: TurnOutcome,
    },
    /// A turn's file changes were undone. `paths` are the files put back (restored or removed).
    Undone {
        summary: String,
        paths: Vec<String>,
    },
    /// The model updated its task list.
    TodoUpdated {
        items: Vec<TodoItem>,
    },
    /// The conversation was compacted to fit the model's context. The sizes are estimates.
    Compacted {
        before_tokens: u64,
        after_tokens: u64,
        elided: usize,
        summarised: usize,
        problem: Option<String>,
        /// What the user asked the summary to focus on, if anything.
        #[serde(default)]
        focus: Option<String>,
    },
}

/// An event with its position in the session log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub session: SessionId,
    /// Position in the session, starting at 0, with no gaps.
    pub seq: u64,
    pub at: UnixMillis,
    pub kind: EventKind,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn events_survive_a_round_trip() {
        let kinds = vec![
            EventKind::TurnStarted { turn: TurnId::new(), text: "hi".into() },
            EventKind::StateChanged { state: AgentState::RunningTool { name: "read".into() } },
            EventKind::StateChanged { state: AgentState::Stalled { silent_for_secs: 90 } },
            EventKind::ToolCallRequested {
                call: ToolCall { id: "c".into(), name: "read".into(), arguments: json!({"path": "a"}) },
            },
            EventKind::UsageReported { usage: ReportedUsage { prompt_tokens: Some(1), ..ReportedUsage::default() } },
            EventKind::TurnFinished { turn: TurnId::new(), outcome: TurnOutcome::Failed { reason: "x".into() } },
        ];
        for kind in kinds {
            let text = serde_json::to_string(&kind).unwrap();
            let back: EventKind = serde_json::from_str(&text).unwrap();
            assert_eq!(kind, back);
        }
    }

    #[test]
    fn commands_survive_a_round_trip() {
        let command = Command::ResolveApproval {
            session: SessionId::new(),
            approval: ApprovalId::new(),
            decision: ApprovalDecision::AllowForSession,
        };
        let text = serde_json::to_string(&command).unwrap();
        assert_eq!(command, serde_json::from_str(&text).unwrap());
    }
}
