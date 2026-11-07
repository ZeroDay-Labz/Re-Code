//! Permissions: what the agent may do without asking, and what "always" remembers. Modes
//! (`PermissionMode`) widen or narrow the default; grants record the user's answers, for the session
//! or for the project (`docs/SECURITY.md`, Permissions).

use crate::chat::{CALL_MCP_TOOL, LIST_MCP_TOOLS, MCP_DIRECT_PREFIX};
use crate::compact;
use recode_core::extensions::CustomTool;
use recode_core::message::{ToolCall, ToolDefinition};
use recode_core::protocol::PermissionMode;
use recode_tools::sandbox;
use recode_tools::{Risk, Workspace, risk_of, target_of};
use std::collections::BTreeSet;

/// What "always" can remember for a call.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Grant {
    /// This exact call (a shell command, an MCP tool).
    Exact(String),
    /// Creating and changing files anywhere in the project (SECURITY.md's "accept edits").
    ProjectEdits,
    /// Deleting files in the project, which "accept edits" deliberately doesn't cover.
    ProjectDeletes,
    /// A folder outside the project and everything under it: reading, or reading and changing.
    Folder { dir: std::path::PathBuf, write: bool },
}

/// Whether a call needs asking, and if so what "always" would allow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reach {
    /// Runs without asking: reads inside the project, MCP discovery.
    Free,
    Ask {
        /// `None` when it can't be allowed "always" (secrets, deletes outside, commands outside the sandbox).
        grant: Option<Grant>,
        /// For the approval card's "a" key.
        label: Option<String>,
        /// Set when the call reaches outside the project, for the card.
        outside: Option<String>,
        /// The file looks like it holds secrets.
        sensitive: bool,
    },
}

/// The path a file tool acts on, defaulting to the project for list and grep.
pub fn tool_path(call: &ToolCall) -> Option<String> {
    let path = call.arguments.get("path").and_then(|v| v.as_str()).map(str::to_owned);
    match call.name.as_str() {
        "list" | "grep" => Some(path.unwrap_or_else(|| ".".to_owned())),
        "read" | "write" | "edit" | "delete" => path,
        _ => None,
    }
}

/// What a call reaches, deciding whether it asks and what "always" would mean.
pub fn reach(call: &ToolCall, workspace: &Workspace, custom: &std::collections::HashMap<String, CustomTool>) -> Reach {
    if call.name == LIST_MCP_TOOLS || call.name == compact::READ_RESULT {
        return Reach::Free;
    }
    let exact = |key: String, label: String| Reach::Ask {
        grant: Some(Grant::Exact(key)),
        label: Some(label),
        outside: None,
        sensitive: false,
    };
    if call.name == CALL_MCP_TOOL || call.name.starts_with(MCP_DIRECT_PREFIX) {
        let (server, tool) = if call.name == CALL_MCP_TOOL {
            let text = |key| call.arguments.get(key).and_then(|v| v.as_str()).unwrap_or_default().to_owned();
            (text("server"), text("tool"))
        } else {
            let rest = &call.name[MCP_DIRECT_PREFIX.len()..];
            let (server, tool) = rest.split_once("__").unwrap_or((rest, ""));
            (server.to_owned(), tool.to_owned())
        };
        return exact(format!("mcp:{server}:{tool}"), format!("always {tool} on {server}"));
    }
    if call.name == "shell" || custom.contains_key(&call.name) {
        let key = if call.name == "shell" {
            let network = if recode_tools::wants_network(&call.arguments) { "net" } else { "offline" };
            format!("shell:{network}:{}", target_of("shell", &call.arguments))
        } else {
            format!("custom:{}:{}", call.name, call.arguments)
        };
        // A command outside the sandbox is asked every time (SECURITY.md).
        if !sandbox::isolation().is_sandboxed() {
            return Reach::Ask { grant: None, label: None, outside: None, sensitive: false };
        }
        return exact(key, "always this command".to_owned());
    }
    let Some(path) = tool_path(call) else {
        // An unknown tool is treated as the most cautious kind.
        return Reach::Ask { grant: None, label: None, outside: None, sensitive: false };
    };
    let Ok(location) = workspace.locate(&path) else {
        // The tool will refuse the path itself, changing nothing.
        return Reach::Free;
    };
    let sensitive = recode_tools::sensitive::is_sensitive(&location, workspace.root());
    let changes = !matches!(risk_of(&call.name), Risk::ReadOnly);
    match &location {
        recode_tools::Location::Inside(_) if !changes && !sensitive => Reach::Free,
        recode_tools::Location::Inside(_) => {
            let (grant, label) = match call.name.as_str() {
                _ if sensitive => (None, None),
                "delete" => (Some(Grant::ProjectDeletes), Some("always delete in this project".to_owned())),
                _ => (Some(Grant::ProjectEdits), Some("always edit in this project".to_owned())),
            };
            Reach::Ask { grant, label, outside: None, sensitive }
        }
        recode_tools::Location::Outside(real) => {
            // The folder a grant would cover: the directory itself for list and grep, else its parent.
            let dir = if real.is_dir() { real.clone() } else { real.parent().map_or_else(|| real.clone(), Into::into) };
            let shown = recode_tools::workspace::tilde(&dir);
            let (grant, label) = match call.name.as_str() {
                _ if sensitive => (None, None),
                "delete" => (None, None),
                _ if changes => {
                    (Some(Grant::Folder { dir, write: true }), Some(format!("always change files in {shown}")))
                }
                _ => (Some(Grant::Folder { dir, write: false }), Some(format!("always read in {shown}"))),
            };
            Reach::Ask { grant, label, outside: Some(location.display()), sensitive }
        }
    }
}

/// Per-session permission grants, so "always" during a session is remembered, scoped to what was asked.
#[derive(Default)]
pub struct Permissions {
    exact: BTreeSet<String>,
    project_edits: bool,
    project_deletes: bool,
    folders: Vec<(std::path::PathBuf, bool)>,
    /// How much runs without asking.
    pub mode: PermissionMode,
    /// Grants remembered for this project across sessions (from the `permissions` table).
    project: Vec<Grant>,
    /// Grants the user just asked to remember for the project, not yet written.
    new_project_grants: Vec<Grant>,
}

impl Permissions {
    /// Permissions with these folders open for reading and changing (from --add-dir or /add-dir).
    pub fn with_folders(folders: &[std::path::PathBuf]) -> Self {
        let mut permissions = Self::default();
        for dir in folders {
            permissions.allow(Grant::Folder { dir: dir.clone(), write: true });
        }
        permissions
    }

    /// Whether this call can run without asking.
    /// Whether a call runs without asking in the default mode (tests keep the older shape).
    #[cfg(test)]
    pub(crate) fn auto(&self, reach: &Reach) -> bool {
        match reach {
            Reach::Free => true,
            Reach::Ask { grant: Some(grant), .. } => self.has(grant),
            Reach::Ask { grant: None, .. } => false,
        }
    }

    fn has(&self, grant: &Grant) -> bool {
        if self.project.contains(grant) || self.project.iter().any(|known| covers(known, grant)) {
            return true;
        }
        match grant {
            Grant::Exact(key) => self.exact.contains(key),
            Grant::ProjectEdits => self.project_edits,
            Grant::ProjectDeletes => self.project_deletes,
            Grant::Folder { dir, write } => {
                self.folders.iter().any(|(granted, writable)| dir.starts_with(granted) && (*writable || !write))
            }
        }
    }

    pub fn allow(&mut self, grant: Grant) {
        match grant {
            Grant::Exact(key) => {
                self.exact.insert(key);
            }
            Grant::ProjectEdits => self.project_edits = true,
            Grant::ProjectDeletes => self.project_deletes = true,
            Grant::Folder { dir, write } => self.folders.push((dir, write)),
        }
    }

    /// Folders outside the project that commands may reach too, as the user allowed them.
    pub fn sandbox_grants(&self) -> Vec<sandbox::Grant> {
        self.folders.iter().map(|(path, writable)| sandbox::Grant { path: path.clone(), writable: *writable }).collect()
    }
}

/// Whether a remembered grant covers another: a folder grant covers its subfolders (and reading
/// when it allows writing).
fn covers(known: &Grant, wanted: &Grant) -> bool {
    match (known, wanted) {
        (Grant::Folder { dir: have, write: can_write }, Grant::Folder { dir, write }) => {
            dir.starts_with(have) && (*can_write || !write)
        }
        _ => known == wanted,
    }
}

/// What to do with a call, given the mode and the grants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Run,
    Ask,
    /// Not in this mode; the text goes back to the model as the tool's result.
    Refuse(String),
}

/// The tools plan mode leaves available: reading and bookkeeping, plus the plan itself.
const PLAN_TOOLS: &[&str] = &["read", "list", "grep", "git", "skill", "todo", compact::READ_RESULT, LIST_MCP_TOOLS];

impl Permissions {
    /// Takes up the grants remembered for this project.
    pub fn load_project(&mut self, grants: Vec<Grant>) {
        self.project = grants;
    }

    /// Remembers a grant for the project (written by the driver after the turn) and for now.
    pub fn allow_for_project(&mut self, grant: Grant) {
        self.allow(grant.clone());
        if !self.project.contains(&grant) {
            self.project.push(grant.clone());
            self.new_project_grants.push(grant);
        }
    }

    /// Grants the user asked to keep for the project since the last call, to be written.
    pub fn take_new_project_grants(&mut self) -> Vec<Grant> {
        std::mem::take(&mut self.new_project_grants)
    }

    /// Forgets a project grant (the store is updated by the driver).
    pub fn revoke_project(&mut self, grant: &Grant) {
        self.project.retain(|known| known != grant);
    }

    /// The project's remembered grants.
    pub fn project_grants(&self) -> &[Grant] {
        &self.project
    }

    /// What this session allowed, in words.
    pub fn session_grants(&self) -> Vec<String> {
        let mut out: Vec<String> = self.exact.iter().map(|key| describe_key(key)).collect();
        if self.project_edits {
            out.push("edit and create files in this project".to_owned());
        }
        if self.project_deletes {
            out.push("delete files in this project".to_owned());
        }
        for (dir, write) in &self.folders {
            let shown = recode_tools::workspace::tilde(dir);
            out.push(if *write {
                format!("read and change files in {shown}")
            } else {
                format!("read files in {shown}")
            });
        }
        out
    }

    /// Decides a call: the mode first, then the grants, then asking.
    pub fn decide(&self, call: &ToolCall, reach: &Reach) -> Decision {
        if self.mode == PermissionMode::Plan
            && !PLAN_TOOLS.contains(&call.name.as_str())
            && call.name != crate::plan_tool::TOOL
        {
            return Decision::Refuse(format!(
                "Plan mode: `{}` isn't available. Only reading tools run; write what you would do with the plan tool, \
                 and the user carries it out after leaving plan mode.",
                call.name
            ));
        }
        match reach {
            Reach::Free => Decision::Run,
            Reach::Ask { grant: Some(grant), .. } if self.has(grant) => Decision::Run,
            Reach::Ask { grant: Some(Grant::ProjectEdits), .. } if self.mode == PermissionMode::AcceptEdits => {
                Decision::Run
            }
            // The sandbox is the boundary: anything with a grant is inside it. Secrets and unsandboxed
            // commands have no grant, so they still ask.
            Reach::Ask { grant: Some(_), sensitive: false, .. } if self.mode == PermissionMode::AutoSandbox => {
                Decision::Run
            }
            Reach::Ask { .. } => Decision::Ask,
        }
    }

    /// The tools to offer in the current mode: plan mode keeps reading tools and adds `plan`.
    pub fn filter_tools(&self, tools: Vec<ToolDefinition>) -> Vec<ToolDefinition> {
        if self.mode != PermissionMode::Plan {
            return tools;
        }
        let mut kept: Vec<ToolDefinition> =
            tools.into_iter().filter(|tool| PLAN_TOOLS.contains(&tool.name.as_str())).collect();
        kept.push(crate::plan_tool::tool_definition());
        kept
    }
}

impl Grant {
    /// The grant in words, for `/permissions`.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Grant::Exact(key) => describe_key(key),
            Grant::ProjectEdits => "edit and create files in this project".to_owned(),
            Grant::ProjectDeletes => "delete files in this project".to_owned(),
            Grant::Folder { dir, write } => {
                let shown = recode_tools::workspace::tilde(dir);
                if *write { format!("read and change files in {shown}") } else { format!("read files in {shown}") }
            }
        }
    }
}

/// An exact grant's key in words: `shell:net:cargo test` → "run `cargo test` with network".
fn describe_key(key: &str) -> String {
    let mut parts = key.splitn(3, ':');
    match (parts.next(), parts.next(), parts.next()) {
        (Some("shell"), Some("net"), Some(command)) => format!("run `{command}` with network"),
        (Some("shell"), Some(_), Some(command)) => format!("run `{command}`"),
        (Some("mcp"), Some(server), Some(tool)) => format!("call {tool} on {server}"),
        (Some("custom"), Some(name), _) => format!("run the custom tool {name}"),
        _ => key.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(name: &str, args: serde_json::Value) -> ToolCall {
        ToolCall { id: "c".into(), name: name.into(), arguments: args }
    }

    #[test]
    fn modes_decide_what_runs_without_asking() {
        let edit = Reach::Ask { grant: Some(Grant::ProjectEdits), label: None, outside: None, sensitive: false };
        let delete = Reach::Ask { grant: Some(Grant::ProjectDeletes), label: None, outside: None, sensitive: false };
        let secret = Reach::Ask { grant: None, label: None, outside: None, sensitive: true };
        let shell = Reach::Ask {
            grant: Some(Grant::Exact("shell:offline:ls".into())),
            label: None,
            outside: None,
            sensitive: false,
        };
        let unsandboxed = Reach::Ask { grant: None, label: None, outside: None, sensitive: false };
        let mut permissions = Permissions::default();
        let edit_call = call("edit", json!({"path": "a"}));
        assert_eq!(permissions.decide(&call("read", json!({})), &Reach::Free), Decision::Run);
        assert_eq!(permissions.decide(&edit_call, &edit), Decision::Ask);

        permissions.mode = PermissionMode::AcceptEdits;
        assert_eq!(permissions.decide(&edit_call, &edit), Decision::Run);
        assert_eq!(permissions.decide(&call("delete", json!({})), &delete), Decision::Ask, "deletes still ask");
        assert_eq!(permissions.decide(&call("shell", json!({})), &shell), Decision::Ask);

        permissions.mode = PermissionMode::AutoSandbox;
        assert_eq!(permissions.decide(&call("shell", json!({})), &shell), Decision::Run);
        assert_eq!(permissions.decide(&call("delete", json!({})), &delete), Decision::Run);
        assert_eq!(permissions.decide(&call("read", json!({})), &secret), Decision::Ask, "secrets always ask");
        assert_eq!(permissions.decide(&call("shell", json!({})), &unsandboxed), Decision::Ask, "no sandbox, no auto");

        permissions.mode = PermissionMode::Plan;
        assert!(matches!(permissions.decide(&edit_call, &edit), Decision::Refuse(text) if text.contains("Plan mode")));
        assert_eq!(permissions.decide(&call("grep", json!({})), &Reach::Free), Decision::Run);
        let offered = permissions.filter_tools(recode_tools::tool_definitions());
        let names: Vec<&str> = offered.iter().map(|t| t.name.as_str()).collect();
        assert!(
            names.contains(&"read")
                && names.contains(&"plan")
                && !names.contains(&"write")
                && !names.contains(&"shell"),
            "{names:?}"
        );
        permissions.mode = PermissionMode::Ask;
        assert_eq!(
            permissions.filter_tools(recode_tools::tool_definitions()).len(),
            recode_tools::tool_definitions().len()
        );
    }

    #[test]
    fn project_grants_are_remembered_described_and_revoked() {
        let mut permissions = Permissions::default();
        let edit = Reach::Ask { grant: Some(Grant::ProjectEdits), label: None, outside: None, sensitive: false };
        permissions.allow_for_project(Grant::ProjectEdits);
        assert_eq!(permissions.decide(&call("edit", json!({})), &edit), Decision::Run);
        assert_eq!(permissions.take_new_project_grants(), [Grant::ProjectEdits]);
        assert!(permissions.take_new_project_grants().is_empty(), "taken once");
        let folder = Grant::Folder { dir: "/tmp/notes".into(), write: false };
        permissions.load_project(vec![Grant::Exact("shell:net:cargo test".into()), folder.clone()]);
        assert!(
            permissions.has(&Grant::Folder { dir: "/tmp/notes/sub".into(), write: false }),
            "subfolders are covered"
        );
        assert!(!permissions.has(&Grant::Folder { dir: "/tmp/notes".into(), write: true }), "but not writing");
        assert_eq!(Grant::Exact("shell:net:cargo test".into()).describe(), "run `cargo test` with network");
        assert_eq!(Grant::Exact("mcp:rea:open_binary".into()).describe(), "call open_binary on rea");
        permissions.revoke_project(&folder);
        assert_eq!(permissions.project_grants().len(), 1);
        let json = serde_json::to_string(&Grant::Folder { dir: "/x".into(), write: true }).unwrap();
        assert_eq!(serde_json::from_str::<Grant>(&json).unwrap(), Grant::Folder { dir: "/x".into(), write: true });
        // A project grant also holds for the session it was given in.
        assert_eq!(permissions.session_grants(), ["edit and create files in this project"]);
        permissions.allow(Grant::ProjectDeletes);
        assert_eq!(
            permissions.session_grants(),
            ["edit and create files in this project", "delete files in this project"]
        );
    }
}
