//! The agent's native tools, confined to a workspace directory.
//!
//! Every path a tool touches is resolved beneath the workspace root with the kernel's `openat2`
//! `RESOLVE_BENEATH`, so a tool can't escape via `..` or a symlink — the check and the open are one
//! operation, with no TOCTOU gap. Permissions and approval are the engine's job; a tool just runs.

pub mod checkpoint;
mod diff;
pub mod sandbox;
mod search;
pub mod sensitive;
pub mod workspace;

pub use checkpoint::Checkpoints;
pub use diff::unified_diff;
pub use search::project_files;
pub use workspace::{Location, Workspace};

use recode_core::message::ToolDefinition;
use serde_json::{Value, json};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

/// How long one shell command may run before it's stopped.
pub const SHELL_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// How dangerous a tool call is, which decides whether the engine asks first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Risk {
    /// Reads only; safe to run without asking inside the workspace.
    ReadOnly,
    /// Creates or changes a file.
    Modify,
    /// Deletes a file.
    Delete,
    /// Runs an arbitrary command.
    Shell,
}

/// The result of running a tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutput {
    pub success: bool,
    /// Text returned to the model.
    pub content: String,
    /// A one-line summary for the interface.
    pub summary: String,
    /// A unified diff, for edit/write, shown in the approval card and the timeline.
    pub diff: Option<String>,
}

impl ToolOutput {
    fn ok(content: impl Into<String>, summary: impl Into<String>) -> Self {
        Self { success: true, content: content.into(), summary: summary.into(), diff: None }
    }

    /// An error returned to the model so it can correct itself.
    pub fn error(message: impl Into<String>) -> Self {
        let message = message.into();
        Self { success: false, content: format!("Error: {message}"), summary: message, diff: None }
    }

    fn with_diff(mut self, diff: String) -> Self {
        self.diff = Some(diff);
        self
    }
}

/// The built-in tools. `todo` is handled by the engine (it's conversation state), not here.
pub const TOOL_NAMES: &[&str] = &["read", "list", "grep", "write", "edit", "delete", "shell", "git"];

/// The risk category of a tool by name. Unknown tools are treated as shell (most cautious).
#[must_use]
pub fn risk_of(name: &str) -> Risk {
    match name {
        "read" | "list" | "grep" | "git" => Risk::ReadOnly,
        "write" | "edit" => Risk::Modify,
        "delete" => Risk::Delete,
        _ => Risk::Shell,
    }
}

/// The JSON-schema definitions sent to a model. Kept small and plain for weaker local models.
#[must_use]
pub fn tool_definitions() -> Vec<ToolDefinition> {
    let string = json!({"type": "string"});
    let def = |name: &str, description: &str, props: Value, required: Vec<&str>| ToolDefinition {
        name: name.to_owned(),
        description: description.to_owned(),
        parameters: json!({"type": "object", "properties": props, "required": required}),
    };
    vec![
        def(
            "read",
            "Read a file's contents, with line numbers. Paths are relative to the project, or absolute or ~/… for files elsewhere on this computer (the user is asked first). For a long file, read the part you need with offset (the first line, from 1) and limit (how many lines); the result says how many lines the file has.",
            json!({"path": string, "offset": {"type": "integer", "minimum": 1}, "limit": {"type": "integer", "minimum": 1}}),
            vec!["path"],
        ),
        def(
            "list",
            "List the files in a directory: in the project, or anywhere on this computer by absolute or ~/… path.",
            json!({"path": string}),
            vec!["path"],
        ),
        def(
            "grep",
            "Search file contents for a regular expression, in the project or in a folder given by path.",
            json!({"pattern": string, "path": string}),
            vec!["pattern"],
        ),
        def(
            "write",
            "Create or overwrite a file with the given contents. Works anywhere on this computer; the user approves each change.",
            json!({"path": string, "content": string}),
            vec!["path", "content"],
        ),
        def(
            "edit",
            "Replace an exact piece of text in a file with new text. Works anywhere on this computer; the user approves each change.",
            json!({"path": string, "old": string, "new": string}),
            vec!["path", "old", "new"],
        ),
        def("delete", "Delete a file.", json!({"path": string}), vec!["path"]),
        def(
            "shell",
            "Run a shell command in the workspace and return its output. It runs in a sandbox: only the project folder is writable and there's no network. Set network to true only if the command needs the internet (for example to download dependencies); the user is asked. Set background to true for something long-running (a build, a server): it returns a task id at once, and task_output reads its output later.",
            json!({"command": string, "network": {"type": "boolean"}, "background": {"type": "boolean"}}),
            vec!["command"],
        ),
        def("git", "Run a read-only git command: status, diff, log, or show.", json!({"args": string}), vec!["args"]),
    ]
}

/// A preview of the change an edit/write would make, as a diff, without applying it. Used for the
/// approval card so the user sees exactly what will change before saying yes.
#[must_use]
pub fn preview(name: &str, args: &Value, workspace: &Workspace) -> Option<String> {
    let text = |key: &str| args.get(key).and_then(Value::as_str);
    match name {
        "write" => {
            let (path, content) = (text("path")?, text("content")?);
            let location = workspace.locate(path).ok()?;
            // A file that can't be read is not shown as new: no preview is better than a wrong one.
            let before = match workspace.read_existing_at(&location).ok()? {
                Some(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                None => String::new(),
            };
            Some(unified_diff(path, &before, content))
        }
        "edit" => {
            let (path, old, new) = (text("path")?, text("old")?, text("new")?);
            let location = workspace.locate(path).ok()?;
            let before = String::from_utf8_lossy(&workspace.read_at(&location).ok()?).into_owned();
            let after = apply_edit(&before, old, new).ok()?;
            Some(unified_diff(path, &before, &after))
        }
        _ => None,
    }
}

/// The target a tool acts on, for display (`read src/main.rs`).
#[must_use]
pub fn target_of(name: &str, args: &Value) -> String {
    let field = |key: &str| args.get(key).and_then(Value::as_str).unwrap_or_default().to_owned();
    match name {
        "grep" => field("pattern"),
        "shell" => field("command"),
        "git" => format!("git {}", field("args")),
        _ => field("path"),
    }
}

/// How many lines or bytes a single read returns before truncation.
const MAX_READ_BYTES: usize = 64 * 1024;
const MAX_OUTPUT_BYTES: usize = 16 * 1024;

/// Runs a filesystem/search/git/shell tool. Validation errors come back as [`ToolOutput::error`] so
/// the model can recover, rather than as a hard failure.
pub fn run(name: &str, args: &Value, workspace: &Workspace) -> ToolOutput {
    run_cancellable(name, args, workspace, &AtomicBool::new(false), &[])
}

/// Whether a shell call asks for network access.
#[must_use]
pub fn wants_network(args: &Value) -> bool {
    args.get("network").and_then(Value::as_bool).unwrap_or(false)
}

/// [`run`], stopping a running command when `cancel` is set. `grants` are the folders outside the
/// project the user has allowed, which shell commands can then reach too.
pub fn run_cancellable(
    name: &str,
    args: &Value,
    workspace: &Workspace,
    cancel: &AtomicBool,
    grants: &[sandbox::Grant],
) -> ToolOutput {
    let text = |key: &str| args.get(key).and_then(Value::as_str);
    match name {
        "read" => match text("path") {
            Some(path) => read(workspace, path, number(args, "offset"), number(args, "limit")),
            None => ToolOutput::error("read needs a 'path'."),
        },
        "list" => list(workspace, text("path").unwrap_or(".")),
        "grep" => match text("pattern") {
            Some(pattern) => search::grep(workspace, pattern, text("path")),
            None => ToolOutput::error("grep needs a 'pattern'."),
        },
        "write" => match (text("path"), text("content")) {
            (Some(path), Some(content)) => write(workspace, path, content),
            _ => ToolOutput::error("write needs 'path' and 'content'."),
        },
        "edit" => match (text("path"), text("old"), text("new")) {
            (Some(path), Some(old), Some(new)) => edit(workspace, path, old, new),
            _ => ToolOutput::error("edit needs 'path', 'old' and 'new'."),
        },
        "delete" => match text("path") {
            Some(path) => delete(workspace, path),
            None => ToolOutput::error("delete needs a 'path'."),
        },
        "shell" => match text("command") {
            Some(command) => {
                shell(workspace, command, sandbox::Policy { network: wants_network(args) }, grants, cancel)
            }
            None => ToolOutput::error("shell needs a 'command'."),
        },
        "git" => git(workspace, text("args").unwrap_or("status")),
        other => ToolOutput::error(format!("Unknown tool '{other}'. Available: {}.", TOOL_NAMES.join(", "))),
    }
}

/// A whole number argument; models send these as numbers or as strings.
fn number(args: &Value, key: &str) -> Option<usize> {
    let value = args.get(key)?;
    let number = value.as_u64().or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))?;
    usize::try_from(number).ok().filter(|n| *n > 0)
}

/// Lines `offset..offset+limit` (from 1) of a file, numbered, at most `MAX_READ_BYTES` of them. When it
/// stops before the end it says where the file goes on, so the model can read the next part.
fn read(workspace: &Workspace, path: &str, offset: Option<usize>, limit: Option<usize>) -> ToolOutput {
    let bytes = match workspace.locate(path).and_then(|location| workspace.read_at(&location)) {
        Ok(bytes) => bytes,
        Err(error) => return ToolOutput::error(error),
    };
    let text = String::from_utf8_lossy(&bytes);
    let total = text.lines().count();
    let start = offset.unwrap_or(1);
    if start > 1 && start > total {
        return ToolOutput::error(format!("line {start} is past the end: {path} has {total} lines"));
    }
    let end = limit.map_or(total, |limit| (start - 1).saturating_add(limit).min(total));
    let mut numbered = String::new();
    let mut last = start - 1;
    for (index, line) in text.lines().enumerate().take(end).skip(start - 1) {
        let row = format!("{:>5}  {line}\n", index + 1);
        if numbered.len() + row.len() > MAX_READ_BYTES && last >= start {
            break;
        }
        numbered.push_str(&row);
        last = index + 1;
    }
    if last < total {
        numbered.push_str(&format!(
            "… (showing lines {start}–{last} of {total}; read with offset {} for more)\n",
            last + 1
        ));
    }
    let summary = if start == 1 && last == total {
        format!("{total} lines")
    } else {
        format!("lines {start}–{last} of {total}")
    };
    ToolOutput::ok(numbered, summary)
}

fn list(workspace: &Workspace, path: &str) -> ToolOutput {
    match workspace.locate(path).and_then(|location| workspace.list_at(&location)) {
        Ok(mut entries) => {
            entries.sort();
            let summary = format!("{} entries", entries.len());
            ToolOutput::ok(entries.join("\n"), summary)
        }
        Err(error) => ToolOutput::error(error),
    }
}

fn write(workspace: &Workspace, path: &str, content: &str) -> ToolOutput {
    let location = match workspace.locate(path) {
        Ok(location) => location,
        Err(error) => return ToolOutput::error(error),
    };
    let before = workspace
        .read_existing_at(&location)
        .ok()
        .flatten()
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default();
    match workspace.write_at(&location, content.as_bytes()) {
        Ok(()) => {
            let diff = unified_diff(path, &before, content);
            ToolOutput::ok(format!("Wrote {path}."), format!("{} lines", content.lines().count())).with_diff(diff)
        }
        Err(error) => ToolOutput::error(error),
    }
}

fn edit(workspace: &Workspace, path: &str, old: &str, new: &str) -> ToolOutput {
    let location = match workspace.locate(path) {
        Ok(location) => location,
        Err(error) => return ToolOutput::error(error),
    };
    let bytes = match workspace.read_at(&location) {
        Ok(bytes) => bytes,
        Err(error) => return ToolOutput::error(error),
    };
    let before = String::from_utf8_lossy(&bytes).into_owned();
    let after = match apply_edit(&before, old, new) {
        Ok(after) => after,
        Err(error) => return ToolOutput::error(error),
    };
    match workspace.write_at(&location, after.as_bytes()) {
        Ok(()) => {
            let diff = unified_diff(path, &before, &after);
            ToolOutput::ok(format!("Edited {path}."), "edited".to_owned()).with_diff(diff)
        }
        Err(error) => ToolOutput::error(error),
    }
}

/// Applies an edit with a tiered, unique match: exact, then whitespace-tolerant.
fn apply_edit(before: &str, old: &str, new: &str) -> Result<String, String> {
    if old.is_empty() {
        return Err("The 'old' text is empty. Give the exact text to replace.".to_owned());
    }
    let exact = before.matches(old).count();
    if exact == 1 {
        return Ok(before.replacen(old, new, 1));
    }
    if exact > 1 {
        return Err(format!("The 'old' text appears {exact} times. Include more surrounding lines to make it unique."));
    }
    // Whitespace-tolerant: match on trimmed lines.
    if let Some(result) = match_trimmed(before, old, new) {
        return Ok(result);
    }
    Err("The 'old' text wasn't found. Read the file again and copy the exact text.".to_owned())
}

fn match_trimmed(before: &str, old: &str, new: &str) -> Option<String> {
    let lines: Vec<&str> = before.lines().collect();
    let old_lines: Vec<&str> = old.lines().map(str::trim_end).collect();
    if old_lines.is_empty() {
        return None;
    }
    let trimmed: Vec<&str> = lines.iter().map(|line| line.trim_end()).collect();
    let mut found = None;
    for start in 0..=trimmed.len().saturating_sub(old_lines.len()) {
        if trimmed[start..start + old_lines.len()] == old_lines[..] {
            if found.is_some() {
                return None; // not unique
            }
            found = Some(start);
        }
    }
    let start = found?;
    let mut out: Vec<&str> = lines[..start].to_vec();
    for line in new.lines() {
        out.push(line);
    }
    out.extend_from_slice(&lines[start + old_lines.len()..]);
    let mut result = out.join("\n");
    if before.ends_with('\n') {
        result.push('\n');
    }
    Some(result)
}

fn delete(workspace: &Workspace, path: &str) -> ToolOutput {
    match workspace.locate(path).and_then(|location| workspace.delete_at(&location)) {
        Ok(()) => ToolOutput::ok(format!("Deleted {path}."), "deleted".to_owned()),
        Err(error) => ToolOutput::error(error),
    }
}

fn shell(
    workspace: &Workspace,
    command: &str,
    policy: sandbox::Policy,
    grants: &[sandbox::Grant],
    cancel: &AtomicBool,
) -> ToolOutput {
    let argv = ["sh".to_owned(), "-c".to_owned(), command.to_owned()];
    command_output(&sandbox::run(workspace, &argv, policy, grants, cancel, SHELL_TIMEOUT), policy)
}

/// Turns a finished command into what the model and the interface see.
#[must_use]
pub fn command_output(finished: &sandbox::Finished, policy: sandbox::Policy) -> ToolOutput {
    let mut content = match finished.code {
        _ if finished.cancelled => "stopped: the user interrupted it\n".to_owned(),
        _ if finished.timed_out => format!("stopped: it ran longer than {} minutes\n", SHELL_TIMEOUT.as_secs() / 60),
        Some(code) => format!("exit code {code}\n"),
        None => "the command didn't run\n".to_owned(),
    };
    content.push_str(&tail(&finished.output, MAX_OUTPUT_BYTES));
    if let Some(hint) = sandbox::hint(finished, policy) {
        content.push_str("\n\n");
        content.push_str(hint);
    }
    let summary = match finished.code {
        _ if finished.cancelled => "interrupted".to_owned(),
        _ if finished.timed_out => "timed out".to_owned(),
        Some(0) => "exit 0".to_owned(),
        Some(code) => format!("failed ({code})"),
        None => "didn't run".to_owned(),
    };
    ToolOutput { success: finished.success(), content, summary, diff: None }
}

fn git(workspace: &Workspace, args: &str) -> ToolOutput {
    let allowed = ["status", "diff", "log", "show"];
    let first = args.split_whitespace().next().unwrap_or("status");
    if !allowed.contains(&first) {
        return ToolOutput::error(format!("git {first} isn't allowed here. Use: {}.", allowed.join(", ")));
    }
    use std::process::Command;
    let output = Command::new("git").current_dir(workspace.root()).args(args.split_whitespace()).output();
    match output {
        Ok(output) => {
            let text = tail(&String::from_utf8_lossy(&output.stdout), MAX_OUTPUT_BYTES);
            ToolOutput::ok(text, format!("git {first}"))
        }
        Err(error) => ToolOutput::error(format!("couldn't run git: {error}")),
    }
}

/// Keeps the last `limit` bytes, prefixed with a note when truncated.
fn tail(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    let start = text.len() - limit;
    // Align to a char boundary.
    let start = (start..text.len()).find(|&i| text.is_char_boundary(i)).unwrap_or(text.len());
    format!("… (earlier output trimmed)\n{}", &text[start..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_pages_through_a_long_file() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(dir.path()).unwrap();
        let text: String = (1..=50).map(|n| format!("line {n}\n")).collect();
        std::fs::write(dir.path().join("long.txt"), text).unwrap();
        let part = read(&workspace, "long.txt", Some(10), Some(5));
        assert!(
            part.content.starts_with("   10  line 10\n") && part.content.contains("   14  line 14\n"),
            "{}",
            part.content
        );
        assert!(part.content.contains("showing lines 10–14 of 50; read with offset 15 for more"), "{}", part.content);
        assert_eq!(part.summary, "lines 10–14 of 50");
        let whole = read(&workspace, "long.txt", None, None);
        assert_eq!(whole.summary, "50 lines");
        assert!(!whole.content.contains("for more"));
        assert!(read(&workspace, "long.txt", Some(99), None).content.contains("past the end"));
        assert_eq!(number(&json!({"offset": "7"}), "offset"), Some(7));
        assert_eq!(number(&json!({"offset": 0}), "offset"), None);
    }

    #[test]
    fn exact_edit_requires_a_unique_match() {
        assert_eq!(apply_edit("a\nb\nc\n", "b", "B").unwrap(), "a\nB\nc\n");
        assert!(apply_edit("x\nx\n", "x", "y").unwrap_err().contains("appears 2 times"));
        assert!(apply_edit("abc", "zzz", "y").unwrap_err().contains("wasn't found"));
        assert!(apply_edit("abc", "", "y").unwrap_err().contains("empty"));
    }

    #[test]
    fn whitespace_tolerant_edit_matches_trimmed_lines() {
        let before = "fn main() {\n    let x = 1;  \n}\n";
        let after = apply_edit(before, "let x = 1;", "let x = 2;").unwrap();
        assert!(after.contains("let x = 2;"), "{after}");
    }

    #[test]
    fn risk_and_names_line_up() {
        assert_eq!(risk_of("read"), Risk::ReadOnly);
        assert_eq!(risk_of("edit"), Risk::Modify);
        assert_eq!(risk_of("delete"), Risk::Delete);
        assert_eq!(risk_of("shell"), Risk::Shell);
        assert_eq!(tool_definitions().len(), TOOL_NAMES.len());
    }

    #[test]
    fn tail_keeps_the_end_on_a_boundary() {
        assert_eq!(tail("héllo", 100), "héllo");
        assert!(tail(&"x".repeat(100), 10).contains("trimmed"));
    }
}
