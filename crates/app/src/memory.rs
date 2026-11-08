//! Memory that re-code keeps itself: facts the model saves with the `memory` tool while it works and
//! gets back in later sessions. Each memory is a Markdown file with a short front matter; `MEMORY.md`
//! beside them is an index for people to read, rewritten after every change. What the model sees is
//! built from the files themselves, so a memory edited by hand counts at once.
//!
//! Memories live in re-code's own data folder, never in the project: they don't end up in git, and an
//! untrusted folder can't plant any. There are two scopes: this project's (keyed by its canonical path)
//! and the user's own, for every project. Nothing that looks like a secret is saved.

use recode_core::message::{ToolCall, ToolDefinition};
use serde_json::json;
use std::path::{Path, PathBuf};

pub const TOOL: &str = "memory";
/// The longest memory, in characters.
const MAX_BODY: usize = 8_000;
/// The most of the index the model gets with every request, in characters (about 2k tokens).
const MAX_INDEX: usize = 8_000;
const INDEX_FILE: &str = "MEMORY.md";
const KINDS: &[&str] = &["user", "feedback", "project", "reference"];

/// Whether re-code keeps memory: the `ui:memory` setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The model reads and saves memories.
    On,
    /// The model gets the index and may read, but saves nothing.
    ReadOnly,
    /// No index, no tool.
    Off,
}

impl Mode {
    #[must_use]
    pub fn parse(value: Option<&str>) -> Self {
        match value {
            Some("off") => Self::Off,
            Some("read-only") => Self::ReadOnly,
            _ => Self::On,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Project,
    User,
}

impl Scope {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::User => "yours",
        }
    }

    fn parse(value: Option<&str>) -> Self {
        if value == Some("user") { Self::User } else { Self::Project }
    }
}

/// The two memory folders for a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Places {
    /// This project's memories; `None` without a project folder.
    pub project: Option<PathBuf>,
    pub user: PathBuf,
}

impl Places {
    /// The folders under `data_dir` for the project with this canonical key (empty: no project).
    #[must_use]
    pub fn new(data_dir: &Path, project_key: &str) -> Self {
        let root = data_dir.join("memory");
        let project = (!project_key.is_empty()).then(|| root.join(crate::trust::short_hash(project_key)));
        Self { project, user: root.join("user") }
    }

    fn dir(&self, scope: Scope) -> Option<&Path> {
        match scope {
            Scope::Project => self.project.as_deref(),
            Scope::User => Some(&self.user),
        }
    }
}

/// One memory, read from its file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Memory {
    pub name: String,
    pub description: String,
    pub kind: String,
    pub scope: Scope,
    pub path: PathBuf,
    pub body: String,
}

impl Memory {
    /// Estimated tokens of the file, for `/memory`.
    #[must_use]
    pub fn tokens(&self) -> u64 {
        crate::compact::tokens_at(self.body.chars().count() + self.description.chars().count() + 40, 3.0)
    }
}

/// Every memory, project first, by name.
#[must_use]
pub fn list(places: &Places) -> Vec<Memory> {
    let mut all = Vec::new();
    for scope in [Scope::Project, Scope::User] {
        let Some(dir) = places.dir(scope) else { continue };
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        let mut found: Vec<Memory> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension().is_some_and(|e| e == "md") && path.file_name().is_some_and(|n| n != INDEX_FILE)
            })
            .filter_map(|path| read(&path, scope))
            .collect();
        found.sort_by(|a, b| a.name.cmp(&b.name));
        all.extend(found);
    }
    all
}

fn read(path: &Path, scope: Scope) -> Option<Memory> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_file() || meta.len() > 64 * 1024 {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    let (front, body) = recode_extensions::frontmatter::split(&text);
    let front = recode_extensions::frontmatter::parse(front.unwrap_or_default());
    let stem = path.file_stem()?.to_string_lossy().into_owned();
    Some(Memory {
        name: front.name.unwrap_or(stem),
        description: front.description.unwrap_or_default(),
        kind: front.extra.get("type").or_else(|| front.metadata.get("type")).cloned().unwrap_or_default(),
        scope,
        path: path.to_path_buf(),
        body: body.trim().to_owned(),
    })
}

/// The system-prompt part: the index, and when the tool is offered, when to save. `None` when there
/// is nothing to say (no memories and no tool).
#[must_use]
pub fn system_part(memories: &[Memory], mode: Mode) -> Option<String> {
    if mode == Mode::Off || (memories.is_empty() && mode == Mode::ReadOnly) {
        return None;
    }
    let mut text = String::from("# Memory\n");
    if mode == Mode::On {
        text.push_str(
            "You have a memory that lasts between sessions: the memory tool saves, updates, reads and forgets \
             facts. Save what will help in a later session and isn't in the code or git: who the user is and how \
             they like to work, corrections they gave and approaches they confirmed (with the reason), and project \
             decisions or constraints. Don't save what the repository already records, secrets, or what only \
             matters in this conversation. Before saving, check the list below and update an existing memory \
             rather than adding a duplicate. Memories can be out of date: check one against the code before \
             relying on it.\n",
        );
    } else {
        text.push_str("Memories from earlier sessions (read one in full with the memory tool; saving is off):\n");
    }
    if memories.is_empty() {
        text.push_str("No memories yet.");
        return Some(text);
    }
    for scope in [Scope::Project, Scope::User] {
        let these: Vec<&Memory> = memories.iter().filter(|m| m.scope == scope).collect();
        if these.is_empty() {
            continue;
        }
        text.push_str(match scope {
            Scope::Project => "\nThis project's memories:\n",
            Scope::User => "\nThe user's memories (every project):\n",
        });
        for (shown, memory) in these.iter().enumerate() {
            let kind = if memory.kind.is_empty() { String::new() } else { format!(" ({})", memory.kind) };
            let line = format!("- {}{kind}: {}\n", memory.name, memory.description);
            if text.chars().count() + line.chars().count() > MAX_INDEX {
                text.push_str(&format!("- … and {} more (memory list shows them)\n", these.len() - shown));
                break;
            }
            text.push_str(&line);
        }
    }
    Some(text.trim_end().to_owned())
}

/// What a call changed, for the headless result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub action: &'static str,
    pub scope: Scope,
    pub name: String,
}

#[must_use]
pub fn tool_definition(mode: Mode) -> ToolDefinition {
    let actions = if mode == Mode::On { json!(["list", "read", "save", "forget"]) } else { json!(["list", "read"]) };
    ToolDefinition {
        name: TOOL.to_owned(),
        description: if mode == Mode::On {
            "Your memory between sessions. list shows every memory; read gives one in full; save writes one (a \
             new name adds it, an existing name replaces it); forget removes one. scope is project (default) or \
             user (for every project). A memory needs a short kebab-case name, a one-line description that says \
             when it matters, a type (user, feedback, project or reference) and the body."
        } else {
            "Your memory from earlier sessions, read-only here: list shows every memory, read gives one in full."
        }
        .to_owned(),
        parameters: json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": actions},
                "scope": {"type": "string", "enum": ["project", "user"]},
                "name": {"type": "string"},
                "description": {"type": "string"},
                "type": {"type": "string", "enum": KINDS},
                "body": {"type": "string"}
            },
            "required": ["action"]
        }),
    }
}

/// Runs a `memory` call. Returns the text for the model, whether it succeeded, a short summary for
/// the tool row and what changed.
#[must_use]
pub fn run(places: &Places, read_only: bool, call: &ToolCall) -> (String, bool, String, Option<Change>) {
    let field = |key: &str| call.arguments.get(key).and_then(|v| v.as_str()).map(str::trim).unwrap_or_default();
    let scope = Scope::parse(call.arguments.get("scope").and_then(|v| v.as_str()));
    let name = field("name");
    let fail = |text: String| (text, false, "refused".to_owned(), None);
    match field("action") {
        "list" => {
            let all = list(places);
            if all.is_empty() {
                return ("No memories yet.".to_owned(), true, "none yet".to_owned(), None);
            }
            let lines: Vec<String> = all
                .iter()
                .map(|m| format!("{} · {} · {}: {}", m.scope.label(), m.kind, m.name, m.description))
                .collect();
            (lines.join("\n"), true, format!("{} memories", all.len()), None)
        }
        "read" => match list(places).into_iter().find(|m| m.name == name && (m.scope == scope || !has_scope(call))) {
            Some(memory) => (
                format!(
                    "{} ({}, {}): {}\n\n{}",
                    memory.name,
                    memory.scope.label(),
                    memory.kind,
                    memory.description,
                    memory.body
                ),
                true,
                "read".to_owned(),
                None,
            ),
            None => fail(format!("Error: there's no memory named '{name}'. memory list shows them.")),
        },
        "save" | "forget" if read_only => fail(
            "Error: memory is read-only here (plan mode, a helper task, or the user set it so). Nothing was saved."
                .to_owned(),
        ),
        "save" => {
            let description = field("description");
            let kind = field("type");
            let body = field("body");
            match save(places, scope, name, description, kind, body) {
                Ok(replaced) => {
                    let verb = if replaced { "Updated" } else { "Saved" };
                    (
                        format!("{verb} the memory '{name}' ({}).", scope.label()),
                        true,
                        format!("remembered · {}", scope.label()),
                        Some(Change { action: "save", scope, name: name.to_owned() }),
                    )
                }
                Err(reason) => fail(format!("Error: {reason}")),
            }
        }
        "forget" => match forget(places, scope, name) {
            Ok(()) => (
                format!("Forgot the memory '{name}' ({}).", scope.label()),
                true,
                format!("forgot · {}", scope.label()),
                Some(Change { action: "forget", scope, name: name.to_owned() }),
            ),
            Err(reason) => fail(format!("Error: {reason}")),
        },
        other => fail(format!("Error: unknown action '{other}'; use list, read, save or forget.")),
    }
}

/// Whether a call would change memory (and so needs the user's approval).
#[must_use]
pub fn writes(call: &ToolCall) -> bool {
    matches!(call.arguments.get("action").and_then(|v| v.as_str()).map(str::trim), Some("save" | "forget"))
}

/// What an approval card says about a save or forget.
#[must_use]
pub fn describe(call: &ToolCall) -> String {
    let field = |key: &str| call.arguments.get(key).and_then(|v| v.as_str()).map(str::trim).unwrap_or_default();
    let scope = Scope::parse(call.arguments.get("scope").and_then(|v| v.as_str()));
    let whose = match scope {
        Scope::Project => "for this project",
        Scope::User => "for every project",
    };
    if field("action") == "forget" {
        return format!("Forget the memory \"{}\" ({whose}).", field("name"));
    }
    let body: String = field("body").chars().take(600).collect();
    format!(
        "Remember this {whose}, in later sessions too:\n{} ({}): {}\n\n{body}",
        field("name"),
        field("type"),
        field("description")
    )
}

fn has_scope(call: &ToolCall) -> bool {
    call.arguments.get("scope").and_then(|v| v.as_str()).is_some()
}

/// Writes a memory. Returns whether it replaced one.
pub fn save(
    places: &Places,
    scope: Scope,
    name: &str,
    description: &str,
    kind: &str,
    body: &str,
) -> Result<bool, String> {
    let dir = places.dir(scope).ok_or("there's no project folder here, so save it with scope user")?;
    recode_extensions::frontmatter::check_name(name, None)?;
    if description.is_empty() || description.contains('\n') {
        return Err("give a one-line description that says when the memory matters".to_owned());
    }
    if !KINDS.contains(&kind) {
        return Err(format!("type must be one of {}", KINDS.join(", ")));
    }
    if body.is_empty() {
        return Err("the memory has no body".to_owned());
    }
    if body.chars().count() > MAX_BODY {
        return Err(format!("a memory holds at most {MAX_BODY} characters; keep it to the fact and why it matters"));
    }
    for text in [name, description, body] {
        if crate::compact::redact(text) != text {
            return Err(
                "that looks like it contains a secret (a key, token or password), so it wasn't saved".to_owned()
            );
        }
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("can't create {}: {e}", dir.display()))?;
    let path = dir.join(format!("{name}.md"));
    let replaced = path.exists();
    let text = format!("---\nname: {name}\ndescription: {}\ntype: {kind}\n---\n\n{body}\n", one_line(description));
    write_atomically(&path, &text)?;
    write_index(dir, scope)?;
    Ok(replaced)
}

/// Removes a memory.
pub fn forget(places: &Places, scope: Scope, name: &str) -> Result<(), String> {
    let dir = places.dir(scope).ok_or("there's no project folder here")?;
    let found = list(places).into_iter().find(|m| m.scope == scope && m.name == name);
    let Some(memory) = found else {
        return Err(format!("there's no {} memory named '{name}'", scope.label()));
    };
    std::fs::remove_file(&memory.path).map_err(|e| format!("can't remove {}: {e}", memory.path.display()))?;
    write_index(dir, scope)
}

fn one_line(text: &str) -> String {
    text.replace(['\r', '\n'], " ")
}

fn write_atomically(path: &Path, text: &str) -> Result<(), String> {
    let temp = path.with_extension("md.tmp");
    std::fs::write(&temp, text).map_err(|e| format!("can't write {}: {e}", temp.display()))?;
    std::fs::rename(&temp, path).map_err(|e| format!("can't write {}: {e}", path.display()))
}

/// Rewrites `MEMORY.md` from the files in `dir`: one line per memory, for people to read.
fn write_index(dir: &Path, scope: Scope) -> Result<(), String> {
    let places = match scope {
        Scope::Project => Places { project: Some(dir.to_path_buf()), user: PathBuf::new() },
        Scope::User => Places { project: None, user: dir.to_path_buf() },
    };
    let mut text = String::from("# Memory\n\nWritten by re-code; edit the memory files beside this one.\n\n");
    for memory in list(&places).into_iter().filter(|m| m.scope == scope) {
        let file = memory.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        text.push_str(&format!("- [{}]({file}) — {}\n", memory.name, memory.description));
    }
    write_atomically(&dir.join(INDEX_FILE), &text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(arguments: serde_json::Value) -> ToolCall {
        ToolCall { id: "c".into(), name: TOOL.into(), arguments }
    }

    fn places(dir: &Path) -> Places {
        Places::new(dir, "/home/u/project")
    }

    #[test]
    fn a_memory_is_saved_indexed_updated_and_forgotten() {
        let dir = tempfile::tempdir().unwrap();
        let places = places(dir.path());
        let save = json!({"action": "save", "name": "fedora-commands", "description": "Which package manager to use",
                          "type": "feedback", "body": "Always use dnf, never apt. The user runs Fedora."});
        let (text, ok, summary, change) = run(&places, false, &call(save.clone()));
        assert!(ok && text.starts_with("Saved"), "{text}");
        assert_eq!(summary, "remembered · project");
        assert_eq!(change.unwrap().action, "save");
        let index = std::fs::read_to_string(places.project.as_ref().unwrap().join(INDEX_FILE)).unwrap();
        assert!(index.contains("- [fedora-commands](fedora-commands.md) — Which package manager to use"), "{index}");
        let memories = list(&places);
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].kind, "feedback");
        assert_eq!(memories[0].body, "Always use dnf, never apt. The user runs Fedora.");
        let part = system_part(&memories, Mode::On).unwrap();
        assert!(part.contains("- fedora-commands (feedback): Which package manager to use"), "{part}");

        // The same name replaces it; a user memory goes in its own folder.
        let (text, ok, ..) = run(&places, false, &call(save));
        assert!(ok && text.starts_with("Updated"), "{text}");
        let user = json!({"action": "save", "scope": "user", "name": "terse", "description": "Answer style",
                          "type": "user", "body": "Prefers short answers."});
        assert!(run(&places, false, &call(user)).1);
        assert_eq!(list(&places).len(), 2);
        assert!(places.user.join("terse.md").is_file());

        let (text, ok, ..) = run(&places, false, &call(json!({"action": "read", "name": "terse"})));
        assert!(ok && text.contains("Prefers short answers."), "{text}");
        let (_, ok, summary, _) = run(&places, false, &call(json!({"action": "forget", "name": "fedora-commands"})));
        assert!(ok && summary == "forgot · project");
        assert_eq!(list(&places).len(), 1);
        let index = std::fs::read_to_string(places.project.as_ref().unwrap().join(INDEX_FILE)).unwrap();
        assert!(!index.contains("fedora-commands"));
    }

    #[test]
    fn secrets_bad_names_and_read_only_mode_save_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let places = places(dir.path());
        let secret = json!({"action": "save", "name": "key", "description": "The OpenRouter key", "type": "reference",
                            "body": "api_key = sk-or-v1-0123456789abcdef0123456789abcdef"});
        let (text, ok, ..) = run(&places, false, &call(secret));
        assert!(!ok && text.contains("secret"), "{text}");
        let bad = json!({"action": "save", "name": "Not A Name", "description": "x", "type": "user", "body": "y"});
        assert!(!run(&places, false, &call(bad)).1);
        let good = json!({"action": "save", "name": "fine", "description": "x", "type": "user", "body": "y"});
        let (text, ok, ..) = run(&places, true, &call(good));
        assert!(!ok && text.contains("read-only"), "{text}");
        assert!(list(&places).is_empty());
        assert!(system_part(&[], Mode::Off).is_none());
        assert!(system_part(&[], Mode::ReadOnly).is_none());
        assert!(system_part(&[], Mode::On).unwrap().contains("No memories yet."));
        assert!(tool_definition(Mode::ReadOnly).parameters.to_string().contains("[\"list\",\"read\"]"));
    }

    #[test]
    fn a_hand_written_memory_counts_and_the_index_is_capped() {
        let dir = tempfile::tempdir().unwrap();
        let places = places(dir.path());
        let project = places.project.clone().unwrap();
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join("mine.md"),
            "---\nname: mine\ndescription: Written by hand\ntype: project\n---\nBody.\n",
        )
        .unwrap();
        std::fs::write(project.join(INDEX_FILE), "# not a memory").unwrap();
        let memories = list(&places);
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].description, "Written by hand");
        let many: Vec<Memory> = (0..400)
            .map(|i| Memory {
                name: format!("m{i}"),
                description: "a fairly long description of when this memory matters".into(),
                kind: "project".into(),
                scope: Scope::Project,
                path: PathBuf::new(),
                body: String::new(),
            })
            .collect();
        let part = system_part(&many, Mode::ReadOnly).unwrap();
        assert!(part.chars().count() < MAX_INDEX + 200);
        assert!(part.contains("more (memory list shows them)"), "{}", &part[part.len() - 200..]);
        // No project folder: only the user's scope.
        let without = Places::new(dir.path(), "");
        assert!(without.project.is_none());
        let save = json!({"action": "save", "name": "x", "description": "d", "type": "user", "body": "b"});
        let (text, ok, ..) = run(&without, false, &call(save));
        assert!(!ok && text.contains("scope user"), "{text}");
        assert_eq!(Mode::parse(Some("read-only")), Mode::ReadOnly);
        assert_eq!(Mode::parse(None), Mode::On);
    }
}
