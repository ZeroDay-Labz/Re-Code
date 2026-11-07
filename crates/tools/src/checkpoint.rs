//! Checkpoints: the contents of every file before the agent changes it, grouped by turn, so a
//! turn's file changes can be undone (`docs/AGENT_ENGINE.md`, Checkpoints).
//!
//! This stores file contents itself and never touches git. It can only restore what file tools
//! changed: the effects of shell commands, MCP tools and custom tools aren't captured, and an undo
//! says so. A file changed again after the agent's turn (by the user, say) is left alone rather than
//! overwritten.

use crate::{Location, Workspace};

/// Files bigger than this aren't saved; undo reports them instead of restoring them.
pub const MAX_SAVED_BYTES: usize = 8 * 1024 * 1024;

/// A file's state: its bytes, or absent.
type State = Option<Vec<u8>>;

#[derive(Debug, Clone)]
struct FileCheckpoint {
    /// Where the file is (inside the project, or anywhere else on the computer).
    location: Location,
    /// How it's shown in reports.
    path: String,
    /// Before the turn first touched it. `None` inside means it didn't exist.
    before: Result<State, String>,
    /// After the turn's last change, to detect later edits.
    after: Option<State>,
}

#[derive(Debug, Clone, Default)]
struct Turn {
    label: String,
    files: Vec<FileCheckpoint>,
    /// What the turn did that can't be undone (commands, MCP calls, custom tools).
    untracked: Vec<String>,
}

/// What an undo did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UndoReport {
    /// The request the undone turn started from.
    pub label: String,
    /// Files put back to how they were.
    pub restored: Vec<String>,
    /// Files the turn created, now removed.
    pub removed: Vec<String>,
    /// Files changed since the turn, left as they are.
    pub skipped: Vec<String>,
    /// Files that couldn't be restored, with the reason.
    pub failed: Vec<(String, String)>,
    /// Effects that couldn't be undone.
    pub untracked: Vec<String>,
}

impl UndoReport {
    /// One plain sentence or two for the interface.
    #[must_use]
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if !self.restored.is_empty() {
            parts.push(format!("restored {}", self.restored.join(", ")));
        }
        if !self.removed.is_empty() {
            parts.push(format!("removed {}", self.removed.join(", ")));
        }
        let mut text = if parts.is_empty() {
            format!("Undid “{}”: no files needed changing.", self.label)
        } else {
            format!("Undid “{}”: {}.", self.label, parts.join("; "))
        };
        if !self.skipped.is_empty() {
            text.push_str(&format!(" Left alone because they changed since: {}.", self.skipped.join(", ")));
        }
        for (path, reason) in &self.failed {
            text.push_str(&format!(" Couldn't restore {path}: {reason}."));
        }
        if !self.untracked.is_empty() {
            text.push_str(&format!(" Not undone (commands can't be reversed): {}.", self.untracked.join("; ")));
        }
        text
    }
}

/// Per-turn checkpoints for one session.
#[derive(Debug, Default)]
pub struct Checkpoints {
    turns: Vec<Turn>,
}

impl Checkpoints {
    /// Starts a new turn, labelled with the request that started it.
    pub fn begin_turn(&mut self, label: &str) {
        let line = label.lines().find(|line| !line.trim().is_empty()).unwrap_or_default().trim();
        let mut label: String = line.chars().take(60).collect();
        if line.chars().count() > 60 {
            label.push('…');
        }
        self.turns.push(Turn { label, ..Turn::default() });
    }

    fn current(&mut self) -> &mut Turn {
        if self.turns.is_empty() {
            self.turns.push(Turn::default());
        }
        self.turns.last_mut().expect("a turn exists")
    }

    /// Saves a file's contents before the turn first changes it. Later calls for the same file in
    /// the same turn keep the first state.
    pub fn before_change(&mut self, workspace: &Workspace, path: &str) {
        let location = match workspace.locate(path) {
            Ok(Location::Inside(relative)) if relative == "." => return,
            Ok(location) => location,
            Err(_) => {
                // The change itself will fail on the same path, but never let it go unrecorded.
                self.untracked(&format!("a change to {path}, which couldn't be checkpointed"));
                return;
            }
        };
        let turn = self.current();
        if turn.files.iter().any(|file| file.location == location) {
            return;
        }
        let before = match workspace.read_existing_at(&location) {
            Ok(Some(bytes)) if bytes.len() > MAX_SAVED_BYTES => Err("it's too large to keep a copy".to_owned()),
            other => other,
        };
        let path = location.display();
        turn.files.push(FileCheckpoint { location, path, before, after: None });
    }

    /// Records a file's state after a change, so undo can tell whether it changed again since.
    pub fn after_change(&mut self, workspace: &Workspace, path: &str) {
        let Ok(location) = workspace.locate(path) else { return };
        let state = workspace.read_existing_at(&location).ok().flatten();
        if let Some(file) = self.current().files.iter_mut().find(|file| file.location == location) {
            file.after = Some(state);
        }
    }

    /// Notes something the turn did that can't be undone.
    pub fn untracked(&mut self, what: &str) {
        let what: String = what.chars().take(80).collect();
        self.current().untracked.push(what);
    }

    /// How many turns have begun, so a rewind can name how many to keep.
    #[must_use]
    pub fn turn_count(&self) -> usize {
        self.turns.len()
    }

    /// Undoes every turn after the first `keep`, newest first, restoring what each changed. Turns that
    /// changed nothing are simply dropped.
    pub fn undo_to(&mut self, workspace: &Workspace, keep: usize) -> Vec<UndoReport> {
        let mut reports = Vec::new();
        while self.turns.len() > keep {
            let turn = self.turns.pop().expect("more turns than kept");
            if turn.files.is_empty() && turn.untracked.is_empty() {
                continue;
            }
            let mut report = UndoReport { label: turn.label, untracked: turn.untracked, ..UndoReport::default() };
            for file in turn.files.into_iter().rev() {
                restore(workspace, file, &mut report);
            }
            reports.push(report);
        }
        reports
    }

    /// Whether any turn changed something.
    #[must_use]
    pub fn can_undo(&self) -> bool {
        self.turns.iter().any(|turn| !turn.files.is_empty() || !turn.untracked.is_empty())
    }

    /// Undoes the most recent turn that changed files or ran commands, newest change first.
    /// Returns `None` when there's nothing to undo.
    pub fn undo(&mut self, workspace: &Workspace) -> Option<UndoReport> {
        while let Some(turn) = self.turns.pop() {
            if turn.files.is_empty() && turn.untracked.is_empty() {
                continue;
            }
            let mut report = UndoReport { label: turn.label, untracked: turn.untracked, ..UndoReport::default() };
            for file in turn.files.into_iter().rev() {
                restore(workspace, file, &mut report);
            }
            return Some(report);
        }
        None
    }
}

fn restore(workspace: &Workspace, file: FileCheckpoint, report: &mut UndoReport) {
    let now = match workspace.read_existing_at(&file.location) {
        Ok(now) => now,
        Err(error) => return report.failed.push((file.path, error)),
    };
    let before = match file.before {
        Ok(before) => before,
        Err(reason) => return report.failed.push((file.path, reason)),
    };
    if now == before {
        return; // Already as it was (the change failed, or it was changed back).
    }
    if let Some(after) = &file.after
        && *after != now
    {
        return report.skipped.push(file.path);
    }
    let result = match &before {
        Some(bytes) => workspace.write_at(&file.location, bytes).map(|()| report.restored.push(file.path.clone())),
        None => workspace.delete_at(&file.location).map(|()| report.removed.push(file.path.clone())),
    };
    if let Err(error) = result {
        report.failed.push((file.path, error));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undo_to_puts_back_every_turn_after_the_kept_ones() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(dir.path()).unwrap();
        std::fs::write(dir.path().join("a.txt"), "one").unwrap();
        let mut checkpoints = Checkpoints::default();
        checkpoints.begin_turn("first");
        checkpoints.before_change(&workspace, "a.txt");
        std::fs::write(dir.path().join("a.txt"), "two").unwrap();
        checkpoints.after_change(&workspace, "a.txt");
        checkpoints.begin_turn("second, nothing changed");
        checkpoints.begin_turn("third");
        checkpoints.before_change(&workspace, "b.txt");
        std::fs::write(dir.path().join("b.txt"), "new").unwrap();
        checkpoints.after_change(&workspace, "b.txt");
        assert_eq!(checkpoints.turn_count(), 3);
        let reports = checkpoints.undo_to(&workspace, 1);
        assert_eq!(reports.len(), 1, "the empty turn makes no report");
        assert!(!dir.path().join("b.txt").exists(), "the third turn's file is gone");
        assert_eq!(std::fs::read_to_string(dir.path().join("a.txt")).unwrap(), "two", "the first turn is kept");
        assert_eq!(checkpoints.turn_count(), 1);
    }

    fn workspace() -> (tempfile::TempDir, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(dir.path()).unwrap();
        (dir, workspace)
    }

    fn change(checkpoints: &mut Checkpoints, workspace: &Workspace, path: &str, content: Option<&str>) {
        checkpoints.before_change(workspace, path);
        match content {
            Some(text) => workspace.write(path, text.as_bytes()).unwrap(),
            None => workspace.delete(path).unwrap(),
        }
        checkpoints.after_change(workspace, path);
    }

    fn read(workspace: &Workspace, path: &str) -> Option<String> {
        workspace.read_existing(path).unwrap().map(|bytes| String::from_utf8(bytes).unwrap())
    }

    #[test]
    fn undo_restores_edits_removes_new_files_and_brings_back_deleted_ones() {
        let (_dir, ws) = workspace();
        ws.write("a.rs", b"original").unwrap();
        ws.write("gone.rs", b"keep me").unwrap();
        let mut checkpoints = Checkpoints::default();
        checkpoints.begin_turn("refactor things");
        change(&mut checkpoints, &ws, "a.rs", Some("edited once"));
        change(&mut checkpoints, &ws, "./a.rs", Some("edited twice"));
        change(&mut checkpoints, &ws, "src/new.rs", Some("new file"));
        change(&mut checkpoints, &ws, "gone.rs", None);

        let report = checkpoints.undo(&ws).unwrap();
        assert_eq!(read(&ws, "a.rs").as_deref(), Some("original"), "the first state of the turn wins");
        assert_eq!(read(&ws, "src/new.rs"), None);
        assert_eq!(read(&ws, "gone.rs").as_deref(), Some("keep me"));
        assert_eq!(report.removed, vec!["src/new.rs"]);
        assert!(report.restored.contains(&"a.rs".to_owned()) && report.restored.contains(&"gone.rs".to_owned()));
        assert!(report.describe().contains("refactor things"));
        assert!(checkpoints.undo(&ws).is_none(), "only one turn to undo");
    }

    #[test]
    fn undo_goes_back_one_turn_at_a_time() {
        let (_dir, ws) = workspace();
        let mut checkpoints = Checkpoints::default();
        checkpoints.begin_turn("first");
        change(&mut checkpoints, &ws, "f", Some("one"));
        checkpoints.begin_turn("just a question");
        checkpoints.begin_turn("second");
        change(&mut checkpoints, &ws, "f", Some("two"));

        assert_eq!(checkpoints.undo(&ws).unwrap().label, "second");
        assert_eq!(read(&ws, "f").as_deref(), Some("one"));
        assert_eq!(checkpoints.undo(&ws).unwrap().label, "first", "turns without changes are skipped");
        assert_eq!(read(&ws, "f"), None);
        assert!(!checkpoints.can_undo());
    }

    #[test]
    fn a_file_changed_after_the_turn_is_left_alone() {
        let (_dir, ws) = workspace();
        ws.write("notes.md", b"mine").unwrap();
        let mut checkpoints = Checkpoints::default();
        checkpoints.begin_turn("tidy notes");
        change(&mut checkpoints, &ws, "notes.md", Some("agent's"));
        ws.write("notes.md", b"the user kept typing").unwrap();

        let report = checkpoints.undo(&ws).unwrap();
        assert_eq!(report.skipped, vec!["notes.md"]);
        assert_eq!(read(&ws, "notes.md").as_deref(), Some("the user kept typing"));
        assert!(report.describe().contains("changed since"));
    }

    #[test]
    fn commands_are_reported_as_not_undoable() {
        let (_dir, ws) = workspace();
        let mut checkpoints = Checkpoints::default();
        checkpoints.begin_turn("build it");
        checkpoints.untracked("shell: cargo build");
        let report = checkpoints.undo(&ws).unwrap();
        assert_eq!(report.untracked, vec!["shell: cargo build"]);
        assert!(report.describe().contains("can't be reversed"));
    }

    #[test]
    fn a_file_outside_the_project_is_restored_too() {
        let (_dir, ws) = workspace();
        let elsewhere = tempfile::tempdir().unwrap();
        let notes = elsewhere.path().join("notes.md");
        std::fs::write(&notes, "mine").unwrap();
        let path = notes.to_string_lossy().into_owned();
        let mut checkpoints = Checkpoints::default();
        checkpoints.begin_turn("tidy my notes");
        checkpoints.before_change(&ws, &path);
        std::fs::write(&notes, "agent's").unwrap();
        checkpoints.after_change(&ws, &path);
        let report = checkpoints.undo(&ws).unwrap();
        assert_eq!(std::fs::read_to_string(&notes).unwrap(), "mine");
        assert_eq!(report.restored.len(), 1);
    }

    #[test]
    fn the_same_file_by_two_spellings_is_one_checkpoint() {
        let (dir, ws) = workspace();
        ws.write("a.txt", b"first").unwrap();
        let absolute = dir.path().join("a.txt").to_string_lossy().into_owned();
        let mut checkpoints = Checkpoints::default();
        checkpoints.begin_turn("edit twice");
        checkpoints.before_change(&ws, "a.txt");
        ws.write("a.txt", b"second").unwrap();
        checkpoints.before_change(&ws, &absolute);
        ws.write("a.txt", b"third").unwrap();
        checkpoints.after_change(&ws, &absolute);
        checkpoints.undo(&ws).unwrap();
        assert_eq!(ws.read("a.txt").unwrap(), b"first");
    }
}
