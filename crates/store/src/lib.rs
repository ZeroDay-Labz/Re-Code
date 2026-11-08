//! SQLite persistence: sessions and the append-only event log.
//!
//! The event log is the source of truth, so a session can be rebuilt after a crash by replaying it.
//! The database can hold source code and command output, so it is readable only by its owner.

use recode_core::ids::{SessionId, UnixMillis};
use recode_core::protocol::{EventEnvelope, EventKind};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Each entry upgrades the schema by one version. Released entries must never be edited.
const MIGRATIONS: &[&str] = &[
    "
    CREATE TABLE sessions (
        id            TEXT PRIMARY KEY,
        project_path  TEXT NOT NULL,
        created_at_ms INTEGER NOT NULL
    ) STRICT;
    CREATE INDEX sessions_by_project ON sessions (project_path, created_at_ms);

    CREATE TABLE events (
        session_id TEXT NOT NULL REFERENCES sessions (id) ON DELETE CASCADE,
        seq        INTEGER NOT NULL,
        at_ms      INTEGER NOT NULL,
        kind       TEXT NOT NULL,
        PRIMARY KEY (session_id, seq)
    ) STRICT;
",
    "
    CREATE TABLE settings (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
    ) STRICT;
",
    "
    CREATE TABLE snapshots (
        session_id    TEXT PRIMARY KEY REFERENCES sessions (id) ON DELETE CASCADE,
        title         TEXT NOT NULL,
        conversation  TEXT NOT NULL,
        updated_at_ms INTEGER NOT NULL
    ) STRICT;
",
    "
    CREATE TABLE compactions (
        session_id TEXT NOT NULL REFERENCES sessions (id) ON DELETE CASCADE,
        seq        INTEGER NOT NULL,
        at_ms      INTEGER NOT NULL,
        report     TEXT NOT NULL,
        originals  TEXT NOT NULL,
        PRIMARY KEY (session_id, seq)
    ) STRICT;
    CREATE TABLE permissions (
        project       TEXT NOT NULL,
        grant_json    TEXT NOT NULL,
        created_at_ms INTEGER NOT NULL,
        PRIMARY KEY (project, grant_json)
    ) STRICT;
",
];

/// Errors from the store.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("cannot prepare database location {path}: {source}")]
    Location { path: PathBuf, source: std::io::Error },
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("cannot encode or decode an event: {0}")]
    Codec(#[from] serde_json::Error),
    #[error("invalid stored session id {0:?}")]
    BadSessionId(String),
    #[error(
        "this database has schema version {found}, but this build only understands up to {supported}. \
         Upgrade re-code to open it. The database has not been changed."
    )]
    TooNew { found: u32, supported: u32 },
}

/// A saved session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    pub id: SessionId,
    pub project_path: String,
    pub created_at: UnixMillis,
}

/// The latest saved state of a session's conversation, for resuming it exactly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub session: SessionId,
    /// The first request, shortened, so the session is recognisable.
    pub title: String,
    /// The conversation, as JSON owned by the caller.
    pub conversation: String,
    pub updated_at: UnixMillis,
}

/// One compaction of a session: what it did (JSON report) and the messages it replaced (JSON), so
/// nothing the model once saw is ever lost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionRecord {
    pub session: SessionId,
    pub seq: u64,
    pub at: UnixMillis,
    pub report: String,
    pub originals: String,
}

/// The re-code database.
#[derive(Debug)]
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Opens the database, creating it privately if necessary, and applies pending migrations.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        prepare_location(path).map_err(|source| StoreError::Location { path: path.to_owned(), source })?;
        let conn = Connection::open(path)?;
        // Before anything that takes a lock, so a second connection opening at the same time waits.
        conn.busy_timeout(Duration::from_secs(5))?;
        enable_wal(&conn)?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        Self::configure(conn)
    }

    /// Opens a temporary in-memory database.
    pub fn in_memory() -> Result<Self, StoreError> {
        Self::configure(Connection::open_in_memory()?)
    }

    fn configure(mut conn: Connection) -> Result<Self, StoreError> {
        conn.pragma_update(None, "foreign_keys", true)?;
        conn.busy_timeout(Duration::from_secs(5))?;
        migrate(&mut conn)?;
        Ok(Self { conn })
    }

    /// The schema version of this database.
    pub fn schema_version(&self) -> Result<u32, StoreError> {
        Ok(self.conn.pragma_query_value(None, "user_version", |row| row.get(0))?)
    }

    /// Reads a remembered value, such as the last model used.
    pub fn setting(&self, key: &str) -> Result<Option<String>, StoreError> {
        Ok(self.conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| row.get(0)).optional()?)
    }

    /// Remembers a value.
    pub fn set_setting(&self, key: &str, value: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// Every remembered value whose key starts with `prefix`, with the prefix removed.
    pub fn settings_with_prefix(&self, prefix: &str) -> Result<std::collections::BTreeMap<String, String>, StoreError> {
        let mut statement = self.conn.prepare("SELECT key, value FROM settings WHERE key LIKE ?1 || '%'")?;
        let rows = statement.query_map([prefix], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?;
        rows.map(|row| {
            let (key, value) = row?;
            Ok((key[prefix.len()..].to_owned(), value))
        })
        .collect()
    }

    /// Forgets a remembered value.
    pub fn delete_setting(&self, key: &str) -> Result<(), StoreError> {
        self.conn.execute("DELETE FROM settings WHERE key = ?1", [key])?;
        Ok(())
    }

    /// Removes a session with its events, snapshot and compactions. Nothing else refers to it.
    pub fn delete_session(&self, id: SessionId) -> Result<bool, StoreError> {
        let key = id.to_string();
        self.conn.execute("DELETE FROM compactions WHERE session_id = ?1", [&key])?;
        self.conn.execute("DELETE FROM snapshots WHERE session_id = ?1", [&key])?;
        self.conn.execute("DELETE FROM events WHERE session_id = ?1", [&key])?;
        let removed = self.conn.execute("DELETE FROM sessions WHERE id = ?1", [&key])?;
        Ok(removed > 0)
    }

    /// Records a new session.
    pub fn create_session(&self, project_path: &str, at: UnixMillis) -> Result<SessionRecord, StoreError> {
        let id = SessionId::new();
        self.conn.execute(
            "INSERT INTO sessions (id, project_path, created_at_ms) VALUES (?1, ?2, ?3)",
            params![id.to_string(), project_path, at.0],
        )?;
        Ok(SessionRecord { id, project_path: project_path.to_owned(), created_at: at })
    }

    /// Lists a project's sessions, newest first.
    pub fn sessions_for_project(&self, project_path: &str) -> Result<Vec<SessionRecord>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT id, project_path, created_at_ms FROM sessions
             WHERE project_path = ?1 ORDER BY created_at_ms DESC, id DESC",
        )?;
        let rows = statement.query_map([project_path], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?))
        })?;
        rows.map(|row| {
            let (id, project_path, at) = row?;
            let id = SessionId::parse(&id).map_err(|_| StoreError::BadSessionId(id))?;
            Ok(SessionRecord { id, project_path, created_at: UnixMillis(at) })
        })
        .collect()
    }

    /// Appends an event, assigning the next sequence number.
    pub fn append_event(
        &mut self,
        session: SessionId,
        at: UnixMillis,
        kind: EventKind,
    ) -> Result<EventEnvelope, StoreError> {
        let encoded = serde_json::to_string(&kind)?;
        let transaction = self.write_transaction()?;
        let last: Option<i64> = transaction
            .query_row("SELECT MAX(seq) FROM events WHERE session_id = ?1", [session.to_string()], |row| row.get(0))
            .optional()?
            .flatten();
        let seq = last.map_or(0, |value| value + 1);
        transaction.execute(
            "INSERT INTO events (session_id, seq, at_ms, kind) VALUES (?1, ?2, ?3, ?4)",
            params![session.to_string(), seq, at.0, encoded],
        )?;
        transaction.commit()?;
        Ok(EventEnvelope { session, seq: seq.cast_unsigned(), at, kind })
    }

    /// Appends several events in one transaction, keeping their order.
    pub fn append_events(&mut self, session: SessionId, events: &[(UnixMillis, EventKind)]) -> Result<(), StoreError> {
        let transaction = self.write_transaction()?;
        let last: Option<i64> = transaction
            .query_row("SELECT MAX(seq) FROM events WHERE session_id = ?1", [session.to_string()], |row| row.get(0))
            .optional()?
            .flatten();
        let mut seq = last.map_or(0, |value| value + 1);
        {
            let mut insert =
                transaction.prepare("INSERT INTO events (session_id, seq, at_ms, kind) VALUES (?1, ?2, ?3, ?4)")?;
            for (at, kind) in events {
                insert.execute(params![session.to_string(), seq, at.0, serde_json::to_string(kind)?])?;
                seq += 1;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    /// Saves (replacing) a session's conversation snapshot.
    pub fn save_snapshot(
        &self,
        session: SessionId,
        title: &str,
        conversation: &str,
        at: UnixMillis,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO snapshots (session_id, title, conversation, updated_at_ms) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (session_id) DO UPDATE SET title = excluded.title, conversation = excluded.conversation,
             updated_at_ms = excluded.updated_at_ms",
            params![session.to_string(), title, conversation, at.0],
        )?;
        Ok(())
    }

    /// A project's saved sessions, most recently used first.
    pub fn snapshots_for_project(&self, project_path: &str) -> Result<Vec<Snapshot>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT s.session_id, s.title, s.conversation, s.updated_at_ms FROM snapshots s
             JOIN sessions ON sessions.id = s.session_id
             WHERE sessions.project_path = ?1 ORDER BY s.updated_at_ms DESC, s.session_id DESC LIMIT 50",
        )?;
        let rows = statement.query_map([project_path], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get(3)?))
        })?;
        rows.map(|row| {
            let (id, title, conversation, at) = row?;
            let session = SessionId::parse(&id).map_err(|_| StoreError::BadSessionId(id))?;
            Ok(Snapshot { session, title, conversation, updated_at: UnixMillis(at) })
        })
        .collect()
    }

    /// The most recently updated snapshot among a project's sessions.
    pub fn latest_snapshot(&self, project_path: &str) -> Result<Option<Snapshot>, StoreError> {
        let row = self
            .conn
            .query_row(
                "SELECT s.session_id, s.title, s.conversation, s.updated_at_ms FROM snapshots s
                 JOIN sessions ON sessions.id = s.session_id
                 WHERE sessions.project_path = ?1 ORDER BY s.updated_at_ms DESC, s.session_id DESC LIMIT 1",
                [project_path],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get(3)?)),
            )
            .optional()?;
        row.map(|(id, title, conversation, at)| {
            let session = SessionId::parse(&id).map_err(|_| StoreError::BadSessionId(id))?;
            Ok(Snapshot { session, title, conversation, updated_at: UnixMillis(at) })
        })
        .transpose()
    }

    /// A transaction that takes the write lock at the start. A deferred one that reads and then writes
    /// can't upgrade its lock if another connection wrote in between, and SQLite reports that as busy
    /// at once, without waiting; starting as a writer makes it wait its turn instead.
    /// Keeps a compaction's report and the messages it replaced.
    pub fn save_compaction(
        &self,
        session: SessionId,
        seq: u64,
        at: UnixMillis,
        report: &str,
        originals: &str,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO compactions (session_id, seq, at_ms, report, originals) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (session_id, seq) DO UPDATE SET report = excluded.report, originals = excluded.originals",
            params![session.to_string(), i64::try_from(seq).unwrap_or(i64::MAX), at.0, report, originals],
        )?;
        Ok(())
    }

    /// A session's compactions, oldest first.
    pub fn compactions_for(&self, session: SessionId) -> Result<Vec<CompactionRecord>, StoreError> {
        let mut statement = self
            .conn
            .prepare("SELECT seq, at_ms, report, originals FROM compactions WHERE session_id = ?1 ORDER BY seq")?;
        let rows = statement.query_map([session.to_string()], |row| {
            Ok(CompactionRecord {
                session,
                seq: u64::try_from(row.get::<_, i64>(0)?).unwrap_or_default(),
                at: UnixMillis(row.get(1)?),
                report: row.get(2)?,
                originals: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Remembers a permission grant (JSON) for a project, keyed by the canonical project path.
    pub fn add_permission(&self, project: &str, grant: &str, at: UnixMillis) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO permissions (project, grant_json, created_at_ms) VALUES (?1, ?2, ?3)
             ON CONFLICT (project, grant_json) DO NOTHING",
            params![project, grant, at.0],
        )?;
        Ok(())
    }

    /// A project's remembered grants (JSON), oldest first.
    pub fn permissions_for(&self, project: &str) -> Result<Vec<String>, StoreError> {
        let mut statement = self
            .conn
            .prepare("SELECT grant_json FROM permissions WHERE project = ?1 ORDER BY created_at_ms, grant_json")?;
        let rows = statement.query_map([project], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Forgets one remembered grant.
    pub fn remove_permission(&self, project: &str, grant: &str) -> Result<(), StoreError> {
        self.conn.execute("DELETE FROM permissions WHERE project = ?1 AND grant_json = ?2", params![project, grant])?;
        Ok(())
    }

    fn write_transaction(&mut self) -> Result<rusqlite::Transaction<'_>, StoreError> {
        Ok(self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?)
    }

    /// Returns a session's events in order, for recovery and display.
    pub fn replay(&self, session: SessionId) -> Result<Vec<EventEnvelope>, StoreError> {
        let mut statement =
            self.conn.prepare("SELECT seq, at_ms, kind FROM events WHERE session_id = ?1 ORDER BY seq")?;
        let rows = statement.query_map([session.to_string()], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, String>(2)?))
        })?;
        rows.map(|row| {
            let (seq, at, kind) = row?;
            Ok(EventEnvelope {
                session,
                seq: seq.cast_unsigned(),
                at: UnixMillis(at),
                kind: serde_json::from_str(&kind)?,
            })
        })
        .collect()
    }
}

/// Switches to write-ahead logging. When two connections open a new file at once, SQLite can report
/// this switch as busy without waiting, so it's retried briefly.
fn enable_wal(conn: &Connection) -> Result<(), StoreError> {
    let mut attempts = 0;
    loop {
        match conn.pragma_update(None, "journal_mode", "WAL") {
            Err(rusqlite::Error::SqliteFailure(error, _))
                if error.code == rusqlite::ErrorCode::DatabaseBusy && attempts < 100 =>
            {
                attempts += 1;
                std::thread::sleep(Duration::from_millis(20));
            }
            other => return Ok(other?),
        }
    }
}

fn migrate(conn: &mut Connection) -> Result<(), StoreError> {
    let supported = u32::try_from(MIGRATIONS.len()).expect("migration count fits in u32");
    let found: u32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if found > supported {
        return Err(StoreError::TooNew { found, supported });
    }
    if found == supported {
        return Ok(()); // Nothing to do, so no write lock is taken.
    }
    let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // Read again under the write lock: another connection may have migrated while this one waited.
    let found: u32 = transaction.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if found > supported {
        return Err(StoreError::TooNew { found, supported });
    }
    for sql in &MIGRATIONS[found as usize..] {
        transaction.execute_batch(sql)?;
    }
    transaction.pragma_update(None, "user_version", supported)?;
    transaction.commit()?;
    Ok(())
}

/// Creates the directory and file with owner-only permissions before SQLite opens the file.
/// SQLite gives its WAL and shared-memory files the same permissions as the database.
fn prepare_location(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder.create(parent)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use recode_core::ids::TurnId;
    use recode_core::protocol::{AgentState, TurnOutcome};

    fn sample_events() -> Vec<EventKind> {
        let turn = TurnId::new();
        vec![
            EventKind::TurnStarted { turn, text: "fix it".into() },
            EventKind::StateChanged { state: AgentState::Responding },
            EventKind::TextDelta { text: "done".into() },
            EventKind::TurnFinished { turn, outcome: TurnOutcome::Completed },
        ]
    }

    #[test]
    fn replay_returns_events_in_order_without_gaps() {
        let mut store = Store::in_memory().unwrap();
        let session = store.create_session("/p", UnixMillis(1)).unwrap();
        let expected = sample_events();
        for kind in &expected {
            store.append_event(session.id, UnixMillis(2), kind.clone()).unwrap();
        }
        let replayed = store.replay(session.id).unwrap();
        assert_eq!(replayed.iter().map(|e| e.seq).collect::<Vec<_>>(), [0, 1, 2, 3]);
        assert_eq!(replayed.into_iter().map(|e| e.kind).collect::<Vec<_>>(), expected);
    }

    #[test]
    fn sessions_do_not_mix() {
        let mut store = Store::in_memory().unwrap();
        let a = store.create_session("/a", UnixMillis(1)).unwrap();
        let b = store.create_session("/b", UnixMillis(1)).unwrap();
        let kind = EventKind::TextDelta { text: "a".into() };
        store.append_event(a.id, UnixMillis(2), kind).unwrap();
        assert_eq!(store.replay(a.id).unwrap().len(), 1);
        assert!(store.replay(b.id).unwrap().is_empty());
        assert_eq!(store.sessions_for_project("/a").unwrap(), [a]);
    }

    #[test]
    fn event_for_unknown_session_is_rejected() {
        let mut store = Store::in_memory().unwrap();
        let kind = EventKind::TextDelta { text: "x".into() };
        assert!(store.append_event(SessionId::new(), UnixMillis(0), kind).is_err());
    }

    #[test]
    fn data_survives_reopening() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/db.sqlite3");
        let session = {
            let mut store = Store::open(&path).unwrap();
            let session = store.create_session("/p", UnixMillis(1)).unwrap();
            store.append_event(session.id, UnixMillis(2), sample_events().remove(0)).unwrap();
            session
        };
        let store = Store::open(&path).unwrap();
        assert_eq!(store.replay(session.id).unwrap().len(), 1);
        assert_eq!(store.schema_version().unwrap() as usize, MIGRATIONS.len());
    }

    #[test]
    fn settings_are_remembered_and_replaced() {
        let store = Store::in_memory().unwrap();
        assert_eq!(store.setting("k").unwrap(), None);
        store.set_setting("k", "one").unwrap();
        store.set_setting("k", "two").unwrap();
        assert_eq!(store.setting("k").unwrap().as_deref(), Some("two"));
    }

    #[test]
    fn a_database_from_the_first_version_upgrades_and_keeps_its_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db.sqlite3");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(MIGRATIONS[0]).unwrap();
            conn.pragma_update(None, "user_version", 1).unwrap();
            conn.execute("INSERT INTO sessions VALUES ('0190a000-0000-7000-8000-000000000000', '/old', 5)", [])
                .unwrap();
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap() as usize, MIGRATIONS.len());
        assert_eq!(store.sessions_for_project("/old").unwrap().len(), 1);
        store.set_setting("k", "v").unwrap();
    }

    #[test]
    fn compactions_and_permissions_are_kept() {
        let store = Store::in_memory().unwrap();
        let session = store.create_session("/p", UnixMillis(1)).unwrap().id;
        store.save_compaction(session, 1, UnixMillis(2), r#"{"before":10}"#, "[]").unwrap();
        store.save_compaction(session, 2, UnixMillis(3), r#"{"before":20}"#, "[{}]").unwrap();
        store.save_compaction(session, 2, UnixMillis(3), r#"{"before":21}"#, "[{}]").unwrap();
        let records = store.compactions_for(session).unwrap();
        assert_eq!(records.len(), 2, "the same seq is replaced, not duplicated");
        assert_eq!(records[1].report, r#"{"before":21}"#);
        assert_eq!(records[0].originals, "[]");

        store.add_permission("/p", r#"{"kind":"edits"}"#, UnixMillis(1)).unwrap();
        store.add_permission("/p", r#"{"kind":"edits"}"#, UnixMillis(2)).unwrap();
        store.add_permission("/p", r#"{"kind":"shell"}"#, UnixMillis(3)).unwrap();
        store.add_permission("/other", r#"{"kind":"shell"}"#, UnixMillis(3)).unwrap();
        assert_eq!(store.permissions_for("/p").unwrap(), [r#"{"kind":"edits"}"#, r#"{"kind":"shell"}"#]);
        store.remove_permission("/p", r#"{"kind":"edits"}"#).unwrap();
        assert_eq!(store.permissions_for("/p").unwrap(), [r#"{"kind":"shell"}"#]);
        assert_eq!(store.permissions_for("/other").unwrap().len(), 1);
    }

    #[test]
    fn database_from_a_newer_version_is_refused_and_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db.sqlite3");
        drop(Store::open(&path).unwrap());
        let future = u32::try_from(MIGRATIONS.len()).unwrap() + 1;
        Connection::open(&path).unwrap().pragma_update(None, "user_version", future).unwrap();

        assert!(matches!(Store::open(&path), Err(StoreError::TooNew { .. })));
        let version: u32 =
            Connection::open(&path).unwrap().pragma_query_value(None, "user_version", |row| row.get(0)).unwrap();
        assert_eq!(version, future);
    }

    #[test]
    fn settings_can_be_listed_by_prefix() {
        let store = Store::in_memory().unwrap();
        store.set_setting("ui:panel_width", "40").unwrap();
        store.set_setting("ui:density", "compact").unwrap();
        store.set_setting("last_model", "m").unwrap();
        let ui = store.settings_with_prefix("ui:").unwrap();
        assert_eq!(ui.len(), 2);
        assert_eq!(ui["panel_width"], "40");
    }

    #[test]
    fn batched_events_continue_the_sequence() {
        let mut store = Store::in_memory().unwrap();
        let session = store.create_session("/p", UnixMillis(1)).unwrap();
        let events = sample_events();
        store.append_event(session.id, UnixMillis(2), events[0].clone()).unwrap();
        let rest: Vec<_> = events[1..].iter().cloned().map(|kind| (UnixMillis(3), kind)).collect();
        store.append_events(session.id, &rest).unwrap();
        let replayed = store.replay(session.id).unwrap();
        assert_eq!(replayed.iter().map(|e| e.seq).collect::<Vec<_>>(), [0, 1, 2, 3]);
        assert_eq!(replayed.into_iter().map(|e| e.kind).collect::<Vec<_>>(), events);
    }

    #[test]
    fn the_latest_snapshot_for_a_project_wins_and_is_replaced_in_place() {
        let store = Store::in_memory().unwrap();
        let old = store.create_session("/p", UnixMillis(1)).unwrap();
        let new = store.create_session("/p", UnixMillis(2)).unwrap();
        let elsewhere = store.create_session("/q", UnixMillis(3)).unwrap();
        assert_eq!(store.latest_snapshot("/p").unwrap(), None);
        store.save_snapshot(old.id, "old", "[1]", UnixMillis(10)).unwrap();
        store.save_snapshot(new.id, "new", "[2]", UnixMillis(20)).unwrap();
        store.save_snapshot(elsewhere.id, "other", "[3]", UnixMillis(30)).unwrap();
        assert_eq!(store.latest_snapshot("/p").unwrap().unwrap().title, "new");
        let listed: Vec<String> = store.snapshots_for_project("/p").unwrap().into_iter().map(|s| s.title).collect();
        assert_eq!(listed, ["new", "old"], "most recent first, other projects left out");
        store.save_snapshot(old.id, "old, continued", "[1,1]", UnixMillis(40)).unwrap();
        let latest = store.latest_snapshot("/p").unwrap().unwrap();
        assert_eq!((latest.session, latest.conversation.as_str()), (old.id, "[1,1]"));
        // Deleting a session takes its snapshot with it and leaves the others alone.
        assert!(store.delete_session(old.id).unwrap());
        assert!(!store.delete_session(old.id).unwrap(), "already gone");
        let listed: Vec<String> = store.snapshots_for_project("/p").unwrap().into_iter().map(|s| s.title).collect();
        assert_eq!(listed, ["new"]);
        assert_eq!(store.sessions_for_project("/p").unwrap().len(), 1);
        assert_eq!(store.latest_snapshot("/q").unwrap().unwrap().title, "other");
    }

    #[test]
    fn two_connections_can_write_at_once_without_a_busy_error() {
        // The interface's settings and the session writer use separate connections to one file.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db.sqlite3");
        let mut first = Store::open(&path).unwrap();
        let session = first.create_session("/p", UnixMillis(1)).unwrap();
        let writer = {
            let path = path.clone();
            std::thread::spawn(move || {
                let mut second = Store::open(&path).unwrap();
                for n in 0..200 {
                    second.append_event(session.id, UnixMillis(n), EventKind::TextDelta { text: "x".into() }).unwrap();
                }
            })
        };
        for n in 0..200 {
            first.set_setting("k", &n.to_string()).unwrap();
            first.append_events(session.id, &[(UnixMillis(n), EventKind::TextDelta { text: "y".into() })]).unwrap();
        }
        writer.join().unwrap();
        let events = first.replay(session.id).unwrap();
        assert_eq!(events.len(), 400);
        assert!(events.iter().enumerate().all(|(index, event)| event.seq == index as u64), "no gaps or repeats");
    }

    #[test]
    fn a_new_database_can_be_opened_by_two_connections_at_once() {
        for _ in 0..20 {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("db.sqlite3");
            let other = {
                let path = path.clone();
                std::thread::spawn(move || Store::open(&path).map(drop))
            };
            Store::open(&path).unwrap();
            other.join().unwrap().unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn database_is_private_to_its_owner() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("data");
        let path = parent.join("db.sqlite3");
        drop(Store::open(&path).unwrap());
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(&parent), 0o700);
    }
}
