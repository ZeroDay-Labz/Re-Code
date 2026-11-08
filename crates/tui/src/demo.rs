//! A recorded session, for trying the interface. Nothing here runs a model or touches a file: it
//! produces the same events the engine will, and the interface labels it as a demo throughout.

use recode_core::ids::{ApprovalId, TurnId};
use recode_core::message::ToolCall;
use recode_core::protocol::{AgentState, ApprovalDecision, EventKind, TodoItem, TodoStatus, TurnOutcome};
use recode_core::usage::{ReportedUsage, TokenCount};
use serde_json::json;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

const REASONING: &str = "The delay is computed but discarded, so every retry fires immediately.\nI should read the loop before changing anything, then check who else calls retry_delay.";
const PLAN: &str =
    "The delay is computed but **never applied**, so all retries fire at once. I'll read the loop first.\n";
const SUMMARY: &str = r#"## Fixed

The retry loop now waits between attempts, and the tests pass.

The problem was on line 42: `retry_delay` returned a duration that was immediately discarded.

```rust
for attempt in 0..MAX_RETRIES {
    match send(&request).await {
        Ok(response) => return Ok(response),
        Err(error) if error.is_transient() => {
            tokio::time::sleep(retry_delay(attempt)).await;
        }
        Err(error) => return Err(error),
    }
}
```

| Attempt | Delay | With jitter |
| --- | --- | --- |
| 1 | 200 ms | 150–250 ms |
| 2 | 400 ms | 300–500 ms |
| 3 | 800 ms | 600–1000 ms |

Things to check next:

1. `MAX_RETRIES` is hard-coded, and might belong in the config.
2. The final error doesn't mention how many attempts were made.
   - Worth adding if this shows up in logs.

> Try resizing the window now. Text, this table and the code above all reflow. 世界 and 🦀 are here to test wide characters.
"#;
const DIFF: &str = "@@ -39,7 +39,7 @@ pub async fn send_with_retry(request: &Request) -> Result<Response> {\n     for attempt in 0..MAX_RETRIES {\n         match send(request).await {\n             Ok(response) => return Ok(response),\n             Err(error) if error.is_transient() => {\n-                let _delay = retry_delay(attempt);\n+                tokio::time::sleep(retry_delay(attempt)).await;\n             }\n             Err(error) => return Err(error),\n         }";
const TEST_OUTPUT: &[&str] = &[
    "   Compiling api-server v0.4.1\n",
    "    Finished `test` profile [unoptimized + debuginfo] target(s)\n",
    "     Running unittests src/lib.rs\n",
    "test auth::tests::retries_transient_errors ... ok\n",
    "test auth::tests::waits_between_attempts ... ok\n",
    "test auth::tests::gives_up_after_max_retries ... ok\n",
    "test result: ok. 3 passed; 0 failed; 0 ignored\n",
];

/// A step in the recording.
#[derive(Debug, Clone)]
enum Step {
    Emit(u64, EventKind),
    WaitForApproval,
}

fn emit(delay_ms: u64, kind: EventKind) -> Step {
    Step::Emit(delay_ms, kind)
}

fn state(delay_ms: u64, state: AgentState) -> Step {
    emit(delay_ms, EventKind::StateChanged { state })
}

fn context(tokens: u64) -> Step {
    emit(
        0,
        EventKind::ContextMeasured {
            used: TokenCount::Estimated { tokens, method: "recorded demo".into() },
            limit: 32768,
        },
    )
}

fn todo(text: &str, status: TodoStatus) -> TodoItem {
    TodoItem { text: text.to_owned(), status }
}

fn tool(id: &str, name: &str, argument: &str, value: &str) -> ToolCall {
    ToolCall { id: id.into(), name: name.into(), arguments: json!({ argument: value }), extra: None }
}

/// Splits text into small pieces, as a model's stream would.
fn stream(steps: &mut Vec<Step>, text: &str, reasoning: bool) {
    let mut pending = String::new();
    for word in text.split_inclusive([' ', '\n']) {
        pending.push_str(word);
        if pending.len() >= 6 {
            let text = std::mem::take(&mut pending);
            let kind = if reasoning { EventKind::ReasoningDelta { text } } else { EventKind::TextDelta { text } };
            steps.push(emit(28, kind));
        }
    }
    if !pending.is_empty() {
        let kind = if reasoning {
            EventKind::ReasoningDelta { text: pending }
        } else {
            EventKind::TextDelta { text: pending }
        };
        steps.push(emit(28, kind));
    }
}

/// Plays the recording against the clock. It can be told about approvals and interrupts, so those
/// keys behave as they will with a real agent.
pub struct Player {
    steps: VecDeque<Step>,
    after_approval: Vec<Step>,
    after_denial: Vec<Step>,
    turn: TurnId,
    approval: Option<ApprovalId>,
    waiting: bool,
    due: Option<Instant>,
}

impl Player {
    /// Creates a player with nothing queued.
    #[must_use]
    pub fn new() -> Self {
        Self {
            steps: VecDeque::new(),
            after_approval: Vec::new(),
            after_denial: Vec::new(),
            turn: TurnId::new(),
            approval: None,
            waiting: false,
            due: None,
        }
    }

    /// Queues the recording as the answer to `prompt`.
    pub fn start(&mut self, prompt: &str, now: Instant) {
        self.turn = TurnId::new();
        let turn = self.turn;
        let approval = ApprovalId::new();
        self.approval = Some(approval);
        let mut steps = vec![
            emit(0, EventKind::TurnStarted { turn, text: prompt.to_owned() }),
            state(150, AgentState::LoadingModel),
            context(620),
            state(1800, AgentState::EvaluatingPrompt),
            context(1840),
            state(900, AgentState::Thinking),
        ];
        stream(&mut steps, REASONING, true);
        steps.push(emit(
            0,
            EventKind::TodoUpdated {
                items: vec![
                    todo("Read the retry loop", TodoStatus::Done),
                    todo("Apply the delay", TodoStatus::InProgress),
                    todo("Run the tests", TodoStatus::Pending),
                ],
            },
        ));
        steps.push(state(200, AgentState::Responding));
        stream(&mut steps, PLAN, false);
        steps.extend([
            state(150, AgentState::RunningTool { name: "read".into() }),
            emit(0, EventKind::ToolCallRequested { call: tool("t1", "read", "path", "src/auth.rs") }),
            emit(350, EventKind::ToolFinished { call_id: "t1".into(), success: true, summary: "214 lines".into() }),
            context(4210),
            state(0, AgentState::RunningTool { name: "grep".into() }),
            emit(150, EventKind::ToolCallRequested { call: tool("t2", "grep", "pattern", "retry_delay") }),
            emit(250, EventKind::ToolFinished { call_id: "t2".into(), success: true, summary: "3 matches".into() }),
            context(4580),
            state(200, AgentState::Thinking),
            emit(600, EventKind::ToolCallRequested { call: tool("t3", "edit", "path", "src/auth.rs") }),
            emit(
                0,
                EventKind::ApprovalRequired {
                    approval,
                    call_id: "t3".into(),
                    description: "Apply this change to src/auth.rs".into(),
                    diff: Some(DIFF.into()),
                    always: Some("always edit in this project".into()),
                },
            ),
            state(0, AgentState::AwaitingApproval),
            Step::WaitForApproval,
        ]);

        let mut allowed = vec![
            emit(0, EventKind::ToolFinished { call_id: "t3".into(), success: true, summary: "+1 −1".into() }),
            context(4720),
            state(150, AgentState::RunningTool { name: "shell".into() }),
            emit(0, EventKind::ToolCallRequested { call: tool("t4", "shell", "command", "cargo test auth") }),
        ];
        for line in TEST_OUTPUT {
            allowed.push(emit(380, EventKind::ToolOutput { call_id: "t4".into(), text: (*line).to_owned() }));
        }
        allowed.extend([
            emit(250, EventKind::ToolFinished { call_id: "t4".into(), success: true, summary: "3 passed".into() }),
            context(5150),
            state(200, AgentState::Responding),
        ]);
        stream(&mut allowed, SUMMARY, false);
        allowed.extend(finish(turn, TurnOutcome::Completed, 6030));

        let mut denied = vec![state(200, AgentState::Responding)];
        stream(&mut denied, "Understood, I won't make that change. Nothing was modified.\n", false);
        denied.extend(finish(turn, TurnOutcome::Completed, 4700));

        self.steps.extend(steps);
        self.after_approval = allowed;
        self.after_denial = denied;
        self.waiting = false;
        self.schedule(now);
    }

    fn schedule(&mut self, now: Instant) {
        self.due = match self.steps.front() {
            Some(Step::Emit(delay, _)) => Some(now + Duration::from_millis(*delay)),
            _ => None,
        };
    }

    /// When the next event is due, if the player isn't waiting on the user.
    #[must_use]
    pub fn next_due(&self) -> Option<Instant> {
        self.due
    }

    /// Returns every event that has come due.
    pub fn tick(&mut self, now: Instant) -> Vec<EventKind> {
        let mut out = Vec::new();
        while let Some(due) = self.due {
            if due > now {
                break;
            }
            match self.steps.pop_front() {
                Some(Step::Emit(_, kind)) => out.push(kind),
                Some(Step::WaitForApproval) | None => {}
            }
            match self.steps.front() {
                Some(Step::WaitForApproval) => {
                    self.steps.pop_front();
                    self.waiting = true;
                    self.due = None;
                }
                // Timing runs from when the last event was due, so a slow frame doesn't stretch the recording.
                Some(Step::Emit(delay, _)) => self.due = Some(due + Duration::from_millis(*delay)),
                None => self.due = None,
            }
        }
        out
    }

    /// Answers the pending approval. An unknown or stale approval is ignored.
    pub fn resolve(&mut self, approval: ApprovalId, decision: ApprovalDecision, now: Instant) -> Vec<EventKind> {
        if !self.waiting || self.approval != Some(approval) {
            return Vec::new();
        }
        self.waiting = false;
        let next = if decision == ApprovalDecision::Deny { &self.after_denial } else { &self.after_approval };
        self.steps = next.iter().cloned().collect();
        self.schedule(now);
        vec![EventKind::ApprovalResolved { approval, decision }]
    }

    /// Stops the recording, as interrupting an agent would.
    pub fn interrupt(&mut self) -> Vec<EventKind> {
        self.steps.clear();
        self.due = None;
        self.waiting = false;
        vec![EventKind::TurnFinished { turn: self.turn, outcome: TurnOutcome::Interrupted }]
    }
}

impl Default for Player {
    fn default() -> Self {
        Self::new()
    }
}

fn finish(turn: TurnId, outcome: TurnOutcome, tokens: u64) -> Vec<Step> {
    vec![
        emit(
            0,
            EventKind::UsageReported {
                // Fixed values for the recording. The status bar marks this whole session as a demo.
                usage: ReportedUsage {
                    prompt_tokens: Some(tokens),
                    cached_prompt_tokens: Some(tokens.saturating_sub(300)),
                    output_tokens: Some(420),
                    output_ms: Some(4200),
                    ..ReportedUsage::default()
                },
            },
        ),
        context(tokens),
        emit(0, EventKind::TurnFinished { turn, outcome }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_until_idle(player: &mut Player, start: Instant) -> Vec<EventKind> {
        let mut now = start;
        let mut events = Vec::new();
        while let Some(due) = player.next_due() {
            now = now.max(due);
            events.extend(player.tick(now));
        }
        events
    }

    fn pending_id(events: &[EventKind]) -> ApprovalId {
        events
            .iter()
            .find_map(|event| match event {
                EventKind::ApprovalRequired { approval, .. } => Some(*approval),
                _ => None,
            })
            .expect("the recording asks for approval")
    }

    #[test]
    fn it_stops_and_waits_for_approval() {
        let now = Instant::now();
        let mut player = Player::new();
        player.start("go", now);
        let events = run_until_idle(&mut player, now);
        assert!(matches!(events.last(), Some(EventKind::StateChanged { state: AgentState::AwaitingApproval })));
        assert_eq!(player.next_due(), None);
    }

    #[test]
    fn allowing_runs_the_tests_and_finishes() {
        let now = Instant::now();
        let mut player = Player::new();
        player.start("go", now);
        let approval = pending_id(&run_until_idle(&mut player, now));
        player.resolve(approval, ApprovalDecision::AllowOnce, now);
        let events = run_until_idle(&mut player, now);
        assert!(events.iter().any(|event| matches!(event, EventKind::ToolOutput { .. })));
        assert!(matches!(events.last(), Some(EventKind::TurnFinished { outcome: TurnOutcome::Completed, .. })));
    }

    #[test]
    fn denying_skips_the_change() {
        let now = Instant::now();
        let mut player = Player::new();
        player.start("go", now);
        let approval = pending_id(&run_until_idle(&mut player, now));
        player.resolve(approval, ApprovalDecision::Deny, now);
        let events = run_until_idle(&mut player, now);
        assert!(!events.iter().any(|event| matches!(event, EventKind::ToolOutput { .. })));
    }

    #[test]
    fn a_stale_approval_is_ignored() {
        let now = Instant::now();
        let mut player = Player::new();
        player.start("go", now);
        run_until_idle(&mut player, now);
        assert!(player.resolve(ApprovalId::new(), ApprovalDecision::AllowOnce, now).is_empty());
    }

    #[test]
    fn interrupting_ends_the_turn_and_stops_everything() {
        let now = Instant::now();
        let mut player = Player::new();
        player.start("go", now);
        assert!(matches!(
            player.interrupt().as_slice(),
            [EventKind::TurnFinished { outcome: TurnOutcome::Interrupted, .. }]
        ));
        assert_eq!(player.next_due(), None);
    }
}
