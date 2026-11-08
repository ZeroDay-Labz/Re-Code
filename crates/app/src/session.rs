//! Saving a session as it happens, and preparing a saved one to continue.
//!
//! Every session event goes to the store's append-only event log, which is what the transcript is
//! rebuilt from. After each turn the exact conversation sent to the model is saved too, as a
//! snapshot, so continuing restores precisely what the model saw (rebuilding it from interface
//! events would lose detail, and compaction rewrites history). The store is written from its own
//! thread with its own connection, so saving never holds up the interface or the model.

use recode_core::ids::{SessionId, TurnId, UnixMillis};
use recode_core::message::Message;
use recode_core::protocol::{EventKind, TurnOutcome};
use recode_store::Store;
use recode_tui::link::UiEvent;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::UnboundedSender;

/// What a snapshot holds: everything needed to carry on exactly where the session left off.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SavedConversation {
    pub model: String,
    pub messages: Vec<Message>,
    /// Notes not yet delivered to the model (such as an undo).
    #[serde(default)]
    pub notes: Vec<String>,
    /// Tool results removed by compaction, by handle, so `read_result` still works.
    #[serde(default)]
    pub elided: BTreeMap<String, String>,
    /// The model's task list.
    #[serde(default)]
    pub todos: Vec<recode_core::protocol::TodoItem>,
    /// What `/compact <focus>` asked summaries to keep.
    #[serde(default)]
    pub compact_focus: Option<String>,
    /// How many compactions the session has had.
    #[serde(default)]
    pub compactions: u64,
    /// Files the model touched most recently, latest last.
    #[serde(default)]
    pub recent_files: Vec<String>,
    /// Skills the model loaded (name, SKILL.md path).
    #[serde(default)]
    pub invoked_skills: Vec<(String, String)>,
    /// Where each turn began (messages before, checkpoint turns before), for rewinding.
    #[serde(default)]
    pub turn_marks: Vec<(usize, usize)>,
}

enum Record {
    Event(UnixMillis, EventKind),
    Snapshot(SavedConversation),
    /// A compaction's report and the messages it replaced.
    Compaction(recode_core::usage::CompactionReport, Vec<Message>),
    /// Answer with `UiEvent::Compactions` for the current session.
    ListCompactions,
    /// Show the messages compaction `seq` replaced, in a pager.
    ShowCompaction(u64),
    /// Carry on writing to an existing session.
    Continue {
        session: SessionId,
        title: String,
    },
    /// Start a fresh session on the next event.
    New,
    /// Remove a saved session; the list of sessions is sent again afterwards.
    Delete(SessionId),
}

/// Writes the session to the store in the background.
#[derive(Clone)]
pub struct Recorder {
    sender: Option<mpsc::Sender<Record>>,
    /// The session being written, mirrored from the writer so the driver can tell whether a
    /// deleted session is the open one.
    current: Arc<Mutex<Option<SessionId>>>,
}

impl Recorder {
    /// A recorder that saves nothing, for when the database couldn't be opened.
    #[must_use]
    pub fn disabled() -> Self {
        Self { sender: None, current: Arc::new(Mutex::new(None)) }
    }

    /// The session being written to, once the first event created or continued one.
    #[must_use]
    pub fn current(&self) -> Option<SessionId> {
        self.current.lock().ok().and_then(|current| *current)
    }

    /// Deletes a saved session. The writer owns the database connection, so it does the deleting and
    /// then sends the fresh list.
    pub fn delete(&self, session: SessionId) {
        self.send(Record::Delete(session));
    }

    /// Starts the writer. The session itself is created on the first event, so opening re-code and
    /// leaving doesn't leave empty sessions behind.
    pub fn start(database: PathBuf, project: String, ui: UnboundedSender<UiEvent>) -> Self {
        let (sender, receiver) = mpsc::channel();
        let current = Arc::new(Mutex::new(None));
        let mirror = current.clone();
        let spawned = std::thread::Builder::new().name("session-writer".to_owned()).spawn(move || {
            Writer { database, project, ui, store: None, session: None, title: None, failed: false, current: mirror }
                .run(&receiver)
        });
        match spawned {
            Ok(_) => Self { sender: Some(sender), current },
            Err(error) => {
                tracing::warn!(%error, "can't start the session writer");
                Self::disabled()
            }
        }
    }

    fn send(&self, record: Record) {
        if let Some(sender) = &self.sender {
            let _ = sender.send(record);
        }
    }

    pub fn event(&self, kind: &EventKind) {
        self.send(Record::Event(UnixMillis::now(), kind.clone()));
    }

    pub fn snapshot(&self, conversation: SavedConversation) {
        self.send(Record::Snapshot(conversation));
    }

    pub fn continue_session(&self, session: SessionId, title: String) {
        self.send(Record::Continue { session, title });
    }

    pub fn new_session(&self) {
        self.send(Record::New);
    }

    pub fn compaction(&self, report: recode_core::usage::CompactionReport, originals: Vec<Message>) {
        self.send(Record::Compaction(report, originals));
    }

    pub fn list_compactions(&self) {
        self.send(Record::ListCompactions);
    }

    pub fn show_compaction(&self, seq: u64) {
        self.send(Record::ShowCompaction(seq));
    }
}

struct Writer {
    database: PathBuf,
    project: String,
    ui: UnboundedSender<UiEvent>,
    store: Option<Store>,
    session: Option<SessionId>,
    title: Option<String>,
    /// Report a failure once, not on every event.
    failed: bool,
    current: Arc<Mutex<Option<SessionId>>>,
}

impl Writer {
    fn set_session(&mut self, session: Option<SessionId>) {
        self.session = session;
        if let Ok(mut current) = self.current.lock() {
            *current = session;
        }
    }

    fn run(mut self, receiver: &mpsc::Receiver<Record>) {
        while let Ok(first) = receiver.recv() {
            // Take whatever else is already waiting, so a burst of tokens is one transaction.
            let mut batch = vec![first];
            batch.extend(receiver.try_iter());
            let mut events = Vec::new();
            for record in batch {
                match record {
                    Record::Event(at, kind) => {
                        if self.title.is_none()
                            && let EventKind::TurnStarted { text, .. } = &kind
                        {
                            self.title = Some(title_of(text));
                        }
                        push_coalesced(&mut events, at, kind);
                    }
                    other => {
                        self.flush(&mut events);
                        self.apply(other);
                    }
                }
            }
            self.flush(&mut events);
        }
    }

    fn apply(&mut self, record: Record) {
        match record {
            Record::Event(..) => {}
            Record::Snapshot(conversation) => {
                let Some(session) = self.session else { return };
                let result = serde_json::to_string(&conversation).map_err(|error| error.to_string()).and_then(|json| {
                    let title = self.title.clone().unwrap_or_default();
                    self.store()?.save_snapshot(session, &title, &json, UnixMillis::now()).map_err(|e| e.to_string())
                });
                self.report(result);
            }
            Record::Compaction(report, originals) => {
                let Some(session) = self.session else { return };
                let seq = report.seq;
                let result = serde_json::to_string(&report)
                    .and_then(|report| serde_json::to_string(&originals).map(|originals| (report, originals)))
                    .map_err(|error| error.to_string())
                    .and_then(|(report, originals)| {
                        self.store()?
                            .save_compaction(session, seq, UnixMillis::now(), &report, &originals)
                            .map_err(|e| e.to_string())
                    });
                self.report(result);
            }
            Record::ListCompactions => {
                let reports = self.compactions().unwrap_or_default();
                let _ = self.ui.send(UiEvent::Compactions(
                    reports
                        .into_iter()
                        .map(|(report, _)| (crate::chat::ago(UnixMillis(report.at_ms)), report))
                        .collect(),
                ));
            }
            Record::ShowCompaction(seq) => {
                let found = self.compactions().unwrap_or_default().into_iter().find(|(report, _)| report.seq == seq);
                match found {
                    Some((report, originals)) => {
                        let messages: Vec<Message> = serde_json::from_str(&originals).unwrap_or_default();
                        let title =
                            format!("Compaction {} · {} messages replaced by the summary", report.seq, messages.len());
                        let _ =
                            self.ui.send(UiEvent::Pager { title, text: crate::compact::render_messages(&messages) });
                    }
                    None => {
                        let _ = self.ui.send(UiEvent::Notice(format!("There's no compaction {seq} in this session.")));
                    }
                }
            }
            Record::Continue { session, title } => {
                self.set_session(Some(session));
                self.title = Some(title);
            }
            Record::New => {
                self.set_session(None);
                self.title = None;
            }
            Record::Delete(session) => {
                let result = self.store().and_then(|store| store.delete_session(session).map_err(|e| e.to_string()));
                match result {
                    Ok(true) => {
                        if self.session == Some(session) {
                            self.set_session(None);
                            self.title = None;
                        }
                        let _ = self.ui.send(UiEvent::Notice("Deleted the chat.".to_owned()));
                    }
                    Ok(false) => {
                        let _ = self.ui.send(UiEvent::Notice("That chat was already gone.".to_owned()));
                    }
                    Err(error) => {
                        let _ = self.ui.send(UiEvent::Notice(format!("Couldn't delete the chat: {error}")));
                    }
                }
                let project = self.project.clone();
                let summaries = self.store().ok().map(|store| crate::chat::session_summaries(Some(store), &project));
                let _ = self.ui.send(UiEvent::Sessions(summaries.unwrap_or_default()));
            }
        }
    }

    /// This session's compactions: each report with its originals as stored JSON.
    fn compactions(&mut self) -> Result<Vec<(recode_core::usage::CompactionReport, String)>, String> {
        let Some(session) = self.session else { return Ok(Vec::new()) };
        let records = self.store()?.compactions_for(session).map_err(|e| e.to_string())?;
        Ok(records
            .into_iter()
            .filter_map(|record| serde_json::from_str(&record.report).ok().map(|report| (report, record.originals)))
            .collect())
    }

    fn store(&mut self) -> Result<&mut Store, String> {
        if self.store.is_none() {
            self.store = Some(Store::open(&self.database).map_err(|error| error.to_string())?);
        }
        Ok(self.store.as_mut().expect("just opened"))
    }

    fn flush(&mut self, events: &mut Vec<(UnixMillis, EventKind)>) {
        if events.is_empty() {
            return;
        }
        let batch = std::mem::take(events);
        let result = (|| {
            let session = match self.session {
                Some(session) => session,
                None => {
                    let project = self.project.clone();
                    let record =
                        self.store()?.create_session(&project, UnixMillis::now()).map_err(|e| e.to_string())?;
                    self.set_session(Some(record.id));
                    record.id
                }
            };
            self.store()?.append_events(session, &batch).map_err(|error| error.to_string())
        })();
        self.report(result);
    }

    fn report(&mut self, result: Result<(), String>) {
        if let Err(error) = result {
            tracing::warn!(%error, "can't save the session");
            if !self.failed {
                self.failed = true;
                let _ = self.ui.send(UiEvent::Notice(format!("This session can't be saved: {error}")));
            }
        }
    }
}

/// Merges consecutive text or reasoning pieces into one event; the transcript reads the same.
fn push_coalesced(events: &mut Vec<(UnixMillis, EventKind)>, at: UnixMillis, kind: EventKind) {
    match (events.last_mut(), kind) {
        (Some((_, EventKind::TextDelta { text })), EventKind::TextDelta { text: more }) => text.push_str(&more),
        (Some((_, EventKind::ReasoningDelta { text })), EventKind::ReasoningDelta { text: more }) => {
            text.push_str(&more);
        }
        (_, kind) => events.push((at, kind)),
    }
}

/// A short, recognisable title from a request.
#[must_use]
pub fn title_of(text: &str) -> String {
    let line = text.lines().find(|line| !line.trim().is_empty()).unwrap_or_default().trim();
    let mut title: String = line.chars().take(60).collect();
    if line.chars().count() > 60 {
        title.push('…');
    }
    title
}

/// A saved session prepared for the interface: the events to show, and what the model must be told
/// about a turn that was cut off.
#[derive(Debug, Clone, PartialEq)]
pub struct Replay {
    pub events: Vec<EventKind>,
    /// The request of a turn that never finished, if there was one.
    pub interrupted: Option<String>,
}

/// Prepares stored events for display. Live states are dropped (nothing is running now), and a turn
/// that never finished is closed as interrupted. Its tool calls are not re-run: a command may have
/// partly completed, so the model is told instead and can look for itself.
#[must_use]
pub fn replay(stored: Vec<EventKind>) -> Replay {
    let mut events = Vec::with_capacity(stored.len());
    let mut open: Option<(TurnId, String)> = None;
    for kind in stored {
        match &kind {
            EventKind::StateChanged { .. } => continue,
            EventKind::TurnStarted { turn, text } => {
                if let Some((previous, _)) = open.take() {
                    events.push(EventKind::TurnFinished { turn: previous, outcome: TurnOutcome::Interrupted });
                }
                open = Some((*turn, text.clone()));
            }
            EventKind::TurnFinished { .. } => open = None,
            _ => {}
        }
        events.push(kind);
    }
    let interrupted = open.map(|(turn, text)| {
        events.push(EventKind::TurnFinished { turn, outcome: TurnOutcome::Interrupted });
        text
    });
    Replay { events, interrupted }
}

#[cfg(test)]
mod tests {
    use super::*;
    use recode_core::protocol::AgentState;

    #[test]
    fn a_burst_of_tokens_is_stored_as_one_event() {
        let mut events = Vec::new();
        for piece in ["Hel", "lo", "!"] {
            push_coalesced(&mut events, UnixMillis(1), EventKind::TextDelta { text: piece.into() });
        }
        push_coalesced(&mut events, UnixMillis(2), EventKind::ReasoningDelta { text: "a".into() });
        push_coalesced(&mut events, UnixMillis(2), EventKind::ReasoningDelta { text: "b".into() });
        push_coalesced(&mut events, UnixMillis(3), EventKind::TextDelta { text: "again".into() });
        let kinds: Vec<EventKind> = events.into_iter().map(|(_, kind)| kind).collect();
        assert_eq!(
            kinds,
            [
                EventKind::TextDelta { text: "Hello!".into() },
                EventKind::ReasoningDelta { text: "ab".into() },
                EventKind::TextDelta { text: "again".into() },
            ]
        );
    }

    #[test]
    fn a_turn_cut_off_by_a_crash_is_closed_as_interrupted_and_not_rerun() {
        let (done, cut) = (TurnId::new(), TurnId::new());
        let replayed = replay(vec![
            EventKind::TurnStarted { turn: done, text: "first".into() },
            EventKind::StateChanged { state: AgentState::Responding },
            EventKind::TurnFinished { turn: done, outcome: TurnOutcome::Completed },
            EventKind::TurnStarted { turn: cut, text: "run the migration".into() },
            EventKind::StateChanged { state: AgentState::RunningTool { name: "shell".into() } },
        ]);
        assert_eq!(replayed.interrupted.as_deref(), Some("run the migration"));
        assert!(!replayed.events.iter().any(|kind| matches!(kind, EventKind::StateChanged { .. })));
        assert_eq!(
            replayed.events.last(),
            Some(&EventKind::TurnFinished { turn: cut, outcome: TurnOutcome::Interrupted })
        );
    }

    #[test]
    fn a_finished_session_replays_without_changes() {
        let turn = TurnId::new();
        let stored = vec![
            EventKind::TurnStarted { turn, text: "hi".into() },
            EventKind::TextDelta { text: "hello".into() },
            EventKind::TurnFinished { turn, outcome: TurnOutcome::Completed },
        ];
        assert_eq!(replay(stored.clone()), Replay { events: stored, interrupted: None });
    }

    #[test]
    fn titles_are_short_and_from_the_first_line() {
        assert_eq!(title_of("\n  fix the login bug\nmore detail"), "fix the login bug");
        assert_eq!(title_of(&"x".repeat(80)).chars().count(), 61);
    }

    #[test]
    fn the_recorder_saves_events_and_a_snapshot_that_can_be_found_again() {
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("db.sqlite3");
        let (ui, mut ui_rx) = tokio::sync::mpsc::unbounded_channel();
        let recorder = Recorder::start(database.clone(), "/proj".into(), ui);
        let turn = TurnId::new();
        recorder.event(&EventKind::TurnStarted { turn, text: "add a test".into() });
        recorder.event(&EventKind::TextDelta { text: "On ".into() });
        recorder.event(&EventKind::TextDelta { text: "it.".into() });
        recorder.event(&EventKind::TurnFinished { turn, outcome: TurnOutcome::Completed });
        let saved = SavedConversation {
            model: "m".into(),
            messages: vec![Message::user("add a test")],
            ..SavedConversation::default()
        };
        recorder.snapshot(saved.clone());
        drop(recorder);

        // The writer finishes what it was sent; wait for the snapshot to land.
        let store = Store::open(&database).unwrap();
        let mut found = None;
        for _ in 0..200 {
            found = store.latest_snapshot("/proj").unwrap();
            if found.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        if let Ok(event) = ui_rx.try_recv() {
            panic!("the writer reported a problem: {event:?}");
        }
        let snapshot = found.expect("the snapshot was saved");
        assert_eq!(snapshot.title, "add a test");
        assert_eq!(serde_json::from_str::<SavedConversation>(&snapshot.conversation).unwrap(), saved);
        let events: Vec<EventKind> = store.replay(snapshot.session).unwrap().into_iter().map(|e| e.kind).collect();
        // Pieces are merged when they arrive together; either way the text reads the same.
        let text: String = events
            .iter()
            .filter_map(|kind| match kind {
                EventKind::TextDelta { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "On it.");
    }
}
