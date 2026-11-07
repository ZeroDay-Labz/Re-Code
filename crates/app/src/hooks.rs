//! Hooks: the user's own commands run at fixed points, as Claude Code does. `~/.config/re-code/hooks.toml`
//! is the user's (run as any command of theirs); `.re-code/hooks.toml` is the project's (loaded only once
//! the folder is trusted, and run in the sandbox without network unless the hook asks for it).
//!
//! ```toml
//! [[hook]]
//! event = "pre_tool_use"          # pre_tool_use | post_tool_use | stop | session_start
//! tools = ["shell", "edit"]       # optional: only these tools (globs allowed); default: all
//! command = "scripts/check-edit.sh"
//! timeout_secs = 30               # default 30
//! network = false                 # project hooks only: allow network inside the sandbox
//! ```
//!
//! The hook gets JSON on stdin (`event`, `project`, `mode`, `tool {name, arguments, call_id}`, `output
//! {content, success}`, `result` for stop). Exit 0 continues; exit 2 blocks (for `pre_tool_use` the tool
//! doesn't run and stderr goes to the model; for `stop` stderr becomes the next request, at most twice);
//! anything else is reported and ignored. Stdout may be JSON: `{"decision": "block", "reason": "…",
//! "updated_input": {…}, "add_context": "…"}`.

use recode_core::extensions::glob_matches;
use recode_tools::Workspace;
use recode_tools::sandbox::{self, Policy};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

pub const USER_FILE: &str = "hooks.toml";
pub const PROJECT_FILE: &str = ".re-code/hooks.toml";
const DEFAULT_TIMEOUT_SECS: u64 = 30;
const MAX_TIMEOUT_SECS: u64 = 600;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookEvent {
    PreToolUse,
    PostToolUse,
    Stop,
    SessionStart,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookDef {
    pub event: HookEvent,
    #[serde(default)]
    pub tools: Vec<String>,
    pub command: String,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    #[serde(default)]
    pub network: bool,
}

fn default_timeout() -> u64 {
    DEFAULT_TIMEOUT_SECS
}

#[derive(Debug, Default, Deserialize)]
struct HookFile {
    #[serde(default)]
    hook: Vec<HookDef>,
}

/// Where a hook comes from, which decides how it runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    User,
    Project,
}

/// Every hook in force.
#[derive(Debug, Default, Clone)]
pub struct Hooks {
    pub hooks: Vec<(Scope, HookDef)>,
    /// Files that couldn't be read, in words.
    pub problems: Vec<String>,
    /// Project hooks found but held back because the folder isn't trusted.
    pub held_back: usize,
}

/// What a hook said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookOutcome {
    pub name: String,
    pub event: HookEvent,
    pub ok: bool,
    /// One line for the transcript.
    pub summary: String,
    /// The hook asked for the action not to happen (exit 2 or `decision: block`), with its reason.
    pub block: Option<String>,
    /// New arguments for the tool (`pre_tool_use` only).
    pub updated_input: Option<serde_json::Value>,
    /// Text for the model.
    pub add_context: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct HookReply {
    decision: Option<String>,
    reason: Option<String>,
    updated_input: Option<serde_json::Value>,
    add_context: Option<String>,
}

/// Reads both hook files. Project hooks need trust; without it they are counted, not loaded.
#[must_use]
pub fn load(user_dir: &Path, project: Option<&Path>, project_trusted: bool) -> Hooks {
    let mut hooks = Hooks::default();
    for (scope, path) in [(Scope::User, user_dir.join(USER_FILE))]
        .into_iter()
        .chain(project.map(|project| (Scope::Project, project.join(PROJECT_FILE))))
    {
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                hooks.problems.push(format!("{} can't be read: {error}", path.display()));
                continue;
            }
        };
        let file: HookFile = match toml::from_str(&text) {
            Ok(file) => file,
            Err(error) => {
                hooks.problems.push(format!("{} isn't valid: {}", path.display(), error.message()));
                continue;
            }
        };
        if scope == Scope::Project && !project_trusted {
            hooks.held_back += file.hook.len();
            continue;
        }
        for mut def in file.hook {
            def.timeout_secs = def.timeout_secs.clamp(1, MAX_TIMEOUT_SECS);
            hooks.hooks.push((scope, def));
        }
    }
    hooks
}

impl Hooks {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }

    /// The hooks for an event, and for a tool when one is named.
    pub fn matching(&self, event: HookEvent, tool: Option<&str>) -> impl Iterator<Item = &(Scope, HookDef)> {
        self.hooks.iter().filter(move |(_, def)| {
            def.event == event
                && match tool {
                    None => true,
                    Some(name) => def.tools.is_empty() || def.tools.iter().any(|pattern| glob_matches(pattern, name)),
                }
        })
    }

    /// Runs every hook for the event with the payload, in file order. Blocking is synchronous: hooks
    /// are the user's own and short, and the caller is on a worker thread already.
    #[must_use]
    pub fn run(
        &self,
        event: HookEvent,
        tool: Option<&str>,
        payload: &serde_json::Value,
        workspace: Option<&Workspace>,
        grants: &[sandbox::Grant],
    ) -> Vec<HookOutcome> {
        let input = serde_json::to_string(payload).unwrap_or_default();
        self.matching(event, tool).map(|(scope, def)| run_one(*scope, def, &input, workspace, grants)).collect()
    }
}

fn run_one(
    scope: Scope,
    def: &HookDef,
    input: &str,
    workspace: Option<&Workspace>,
    grants: &[sandbox::Grant],
) -> HookOutcome {
    let name = hook_name(&def.command);
    let mut outcome = HookOutcome {
        name: name.clone(),
        event: def.event,
        ok: false,
        summary: String::new(),
        block: None,
        updated_input: None,
        add_context: None,
    };
    let argv = match recode_core::extensions::split_command(&def.command) {
        Ok(argv) if !argv.is_empty() => argv,
        _ => {
            outcome.summary = "the command can't be parsed".to_owned();
            return outcome;
        }
    };
    let timeout = Duration::from_secs(def.timeout_secs);
    let started = std::time::Instant::now();
    let (code, stdout, stderr, timed_out) = match (scope, workspace) {
        // A project hook is the repository's code: it runs where the model's commands run.
        (Scope::Project, Some(workspace)) => {
            let finished = sandbox::run_with_input(
                workspace,
                &argv,
                Policy { network: def.network },
                grants,
                &AtomicBool::new(false),
                timeout,
                Some(input),
            );
            (finished.code, finished.stdout, finished.stderr, finished.timed_out)
        }
        (Scope::Project, None) => {
            outcome.summary = "project hooks need a project folder".to_owned();
            return outcome;
        }
        (Scope::User, _) => run_plain(&argv, input, timeout, workspace.map(Workspace::root)),
    };
    let elapsed = started.elapsed().as_secs_f64();
    if timed_out {
        outcome.summary = format!("timed out after {} s; ignored", def.timeout_secs);
        return outcome;
    }
    let reply: HookReply = serde_json::from_str(stdout.trim()).unwrap_or_default();
    let message = reply.reason.clone().or_else(|| {
        let text = stderr.trim();
        (!text.is_empty()).then(|| text.to_owned())
    });
    let decided_block = reply.decision.as_deref().is_some_and(|d| matches!(d, "block" | "deny"));
    match code {
        Some(0) if !decided_block => {
            outcome.ok = true;
            outcome.summary = format!("ok · {elapsed:.1} s");
        }
        Some(2) | Some(0) => {
            outcome.ok = true;
            outcome.block = Some(message.clone().unwrap_or_else(|| "the hook said no".to_owned()));
            outcome.summary = format!("blocked · {elapsed:.1} s");
        }
        Some(code) => {
            outcome.summary =
                format!("exit {code}; ignored{}", message.as_deref().map(|m| format!(": {m}")).unwrap_or_default());
        }
        None => outcome.summary = "didn't finish; ignored".to_owned(),
    }
    outcome.updated_input = reply.updated_input;
    outcome.add_context = reply.add_context.filter(|text| !text.trim().is_empty());
    let _ = name;
    outcome
}

/// A user hook: their own command, run as they would run it, with the JSON on stdin.
fn run_plain(
    argv: &[String],
    input: &str,
    timeout: Duration,
    cwd: Option<&Path>,
) -> (Option<i32>, String, String, bool) {
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};
    let mut command = Command::new(&argv[0]);
    command.args(&argv[1..]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return (None, String::new(), format!("couldn't start: {error}"), false),
    };
    if let Some(mut stdin) = child.stdin.take() {
        let text = input.to_owned();
        std::thread::spawn(move || {
            let _ = stdin.write_all(text.as_bytes());
        });
    }
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut text = String::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_string(&mut text);
            }
            text
        })
    };
    let stdout = drain(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let stderr = drain(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let deadline = std::time::Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if std::time::Instant::now() >= deadline => {
                timed_out = true;
                let _ = child.kill();
                break child.wait().ok();
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => break None,
        }
    };
    let code = if timed_out { None } else { status.and_then(|s| s.code()) };
    (code, stdout.join().unwrap_or_default(), stderr.join().unwrap_or_default(), timed_out)
}

/// A short name for a hook: its command's file name.
fn hook_name(command: &str) -> String {
    let first = command.split_whitespace().next().unwrap_or(command);
    PathBuf::from(first).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| first.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn write_user_hooks(dir: &Path, text: &str) {
        std::fs::write(dir.join(USER_FILE), text).unwrap();
    }

    #[test]
    fn hooks_load_match_tools_and_project_ones_wait_for_trust() {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("user");
        let project = dir.path().join("project");
        std::fs::create_dir_all(&user).unwrap();
        std::fs::create_dir_all(project.join(".re-code")).unwrap();
        write_user_hooks(
            &user,
            "[[hook]]\nevent = \"pre_tool_use\"\ntools = [\"shell\", \"mcp__*\"]\ncommand = \"check.sh\"\n",
        );
        std::fs::write(
            project.join(PROJECT_FILE),
            "[[hook]]\nevent = \"stop\"\ncommand = \"lint.sh\"\ntimeout_secs = 9999\n",
        )
        .unwrap();
        let untrusted = load(&user, Some(&project), false);
        assert_eq!(untrusted.hooks.len(), 1);
        assert_eq!(untrusted.held_back, 1);
        let trusted = load(&user, Some(&project), true);
        assert_eq!(trusted.hooks.len(), 2);
        assert_eq!(trusted.hooks[1].1.timeout_secs, MAX_TIMEOUT_SECS, "clamped");
        assert_eq!(trusted.matching(HookEvent::PreToolUse, Some("shell")).count(), 1);
        assert_eq!(trusted.matching(HookEvent::PreToolUse, Some("mcp__rea__x")).count(), 1);
        assert_eq!(trusted.matching(HookEvent::PreToolUse, Some("edit")).count(), 0);
        assert_eq!(trusted.matching(HookEvent::Stop, None).count(), 1);
        std::fs::write(user.join(USER_FILE), "[[hook]]\nevent = \"nope\"\ncommand = \"x\"\n").unwrap();
        let broken = load(&user, None, false);
        assert!(broken.hooks.is_empty());
        assert_eq!(broken.problems.len(), 1, "{:?}", broken.problems);
    }

    #[test]
    fn a_user_hook_gets_json_on_stdin_and_can_block_or_add_context() {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("user");
        std::fs::create_dir_all(&user).unwrap();
        // Blocks with exit 2 and a reason on stderr; echoes what it got so the payload is checked too.
        let script = user.join("block.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\ninput=$(cat)\ncase \"$input\" in *rm*) echo 'no deleting' >&2; exit 2;; esac\necho '{\"add_context\": \"checked by hook\", \"updated_input\": {\"command\": \"ls -la\"}}'\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        write_user_hooks(&user, &format!("[[hook]]\nevent = \"pre_tool_use\"\ncommand = \"{}\"\n", script.display()));
        let hooks = load(&user, None, false);
        let blocked = hooks.run(
            HookEvent::PreToolUse,
            Some("shell"),
            &json!({"event": "pre_tool_use", "tool": {"name": "shell", "arguments": {"command": "rm -rf x"}}}),
            None,
            &[],
        );
        assert_eq!(blocked.len(), 1);
        assert_eq!(blocked[0].block.as_deref(), Some("no deleting"));
        assert!(blocked[0].summary.starts_with("blocked"));
        let allowed = hooks.run(
            HookEvent::PreToolUse,
            Some("shell"),
            &json!({"event": "pre_tool_use", "tool": {"name": "shell", "arguments": {"command": "ls"}}}),
            None,
            &[],
        );
        assert_eq!(allowed[0].block, None);
        assert_eq!(allowed[0].add_context.as_deref(), Some("checked by hook"));
        assert_eq!(allowed[0].updated_input, Some(json!({"command": "ls -la"})));
        assert!(allowed[0].ok && allowed[0].summary.starts_with("ok"));
        // A hook that fails for its own reasons is reported and ignored, never blocks.
        write_user_hooks(&user, "[[hook]]\nevent = \"stop\"\ncommand = \"/bin/sh -c 'exit 7'\"\n");
        let hooks = load(&user, None, false);
        let failed = hooks.run(HookEvent::Stop, None, &json!({}), None, &[]);
        assert!(!failed[0].ok && failed[0].block.is_none() && failed[0].summary.contains("exit 7"));
        // A missing program is a report, not a crash.
        write_user_hooks(&user, "[[hook]]\nevent = \"stop\"\ncommand = \"/nonexistent/hook\"\n");
        let hooks = load(&user, None, false);
        let missing = hooks.run(HookEvent::Stop, None, &json!({}), None, &[]);
        assert!(!missing[0].ok && missing[0].summary.contains("didn't finish"), "{:?}", missing[0]);
    }
}
