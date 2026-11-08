//! How the interface talks to whatever drives it: the real session or the recording.

use crate::welcome::WelcomeInfo;
use recode_core::extensions::{ExtensionCommand, ExtensionsSnapshot};
use recode_core::ids::ApprovalId;
use recode_core::message::ImageData;
use recode_core::protocol::{ApprovalDecision, EventKind};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

/// Something for the interface to show.
/// A saved session, as the picker lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    /// "5 minutes ago".
    pub age: String,
    pub messages: usize,
}

/// The inference runtimes and containers found on this machine, for the Runtimes tab.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimesInfo {
    /// "podman 5.8.9 rootless".
    pub engines: Vec<String>,
    pub runtimes: Vec<RuntimeRow>,
    pub pods: Vec<PodRow>,
    pub containers: Vec<ContainerRow>,
    /// Addresses that answered but weren't recognised.
    pub unidentified: Vec<String>,
    pub warnings: Vec<String>,
    /// Steps that couldn't run.
    pub problems: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeRow {
    /// Stable within a report; what `UiCommand::StartRuntime` names.
    pub id: String,
    /// "Ollama", "llama.cpp", …
    pub name: String,
    /// Whether re-code has a tested adapter for it.
    pub supported: bool,
    pub state: RuntimeRowState,
    pub url: Option<String>,
    /// One line: "podman container `ollama` in pod `local-ai`".
    pub served_by: Option<String>,
    pub image: Option<String>,
    /// "GPU attached (/dev/kfd, /dev/dri)" or "no GPU devices".
    pub gpu: Option<String>,
    /// User-managed: re-code only starts it when asked, never changes it.
    pub yours: bool,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeRowState {
    Running {
        version: Option<String>,
    },
    /// `start_command` is `None` when re-code doesn't know a safe way to start it.
    Stopped {
        start_command: Option<String>,
        needs_root: bool,
    },
    NotAnswering {
        error: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PodRow {
    pub name: String,
    pub running: bool,
    pub ports: Vec<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerRow {
    pub name: String,
    pub image: String,
    pub status: String,
    pub running: bool,
    pub pod: Option<String>,
    pub ports: Vec<u16>,
    /// The runtime its image is known to run, if any.
    pub runs: Option<String>,
}

/// What the project folder's own configuration would load, and whether it's trusted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectTrust {
    pub folder: String,
    pub trusted: bool,
    /// Skills under `.re-code/skills`.
    pub skills: Vec<String>,
    /// Other configuration files under `.re-code/`.
    pub files: Vec<String>,
}

/// One background command, for `/tasks`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskInfo {
    pub id: String,
    pub command: String,
    /// "running", "finished", "exit N" or "killed".
    pub status: String,
    pub elapsed_secs: u64,
    pub last_line: String,
}

/// One instruction file, for `/memory`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstructionFileInfo {
    /// "yours", "project" or "folder".
    pub scope: String,
    pub path: String,
    /// Estimated tokens.
    pub tokens: u64,
    pub trusted: bool,
    pub problem: Option<String>,
    /// For a memory re-code saved: its scope ("project" or "user") and name, so it can be forgotten.
    pub memory: Option<(String, String)>,
}

/// Where a `# note` goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteScope {
    /// The project's `RE-CODE.md`, shared with the repository.
    ProjectShared,
    /// The project's `RE-CODE.local.md`, for this machine only.
    ProjectLocal,
    /// The user's own `RE-CODE.md` in re-code's config folder.
    User,
}

impl NoteScope {
    pub const ALL: [Self; 3] = [Self::ProjectShared, Self::ProjectLocal, Self::User];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::ProjectShared => "This project (RE-CODE.md, shared with the repository)",
            Self::ProjectLocal => "This project, this machine only (RE-CODE.local.md)",
            Self::User => "Every project (your own RE-CODE.md)",
        }
    }
}

/// Progress of a start the user approved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeStartEvent {
    Started { id: String, command: String },
    Waiting { id: String, seconds: u64 },
    Ready { id: String, name: String, version: Option<String>, verified: bool },
    Failed { id: String, error: String },
}

#[derive(Debug, Clone, PartialEq)]
pub enum UiEvent {
    Welcome(WelcomeInfo),
    /// What runs models on this machine, and how.
    Runtimes(RuntimesInfo),
    RuntimeStart(RuntimeStartEvent),
    ProjectTrust(ProjectTrust),
    /// Results of an MCP registry search.
    McpSearch {
        query: String,
        hits: Vec<recode_core::extensions::McpSearchHit>,
        error: Option<String>,
    },
    /// A skill was staged (or couldn't be), for the install window.
    Install(recode_core::extensions::InstallOutcome),
    /// A plugin was staged (or couldn't be), for the install window.
    PluginInstall(recode_core::extensions::PluginOutcome),
    /// What a marketplace or a plugin search offers.
    PluginListings {
        /// "hermes", a marketplace name, or "search: <words>".
        title: String,
        listings: Vec<recode_core::extensions::PluginListing>,
        error: Option<String>,
    },
    /// Results of a skill hub search.
    SkillSearch {
        query: String,
        hits: Vec<recode_core::extensions::SkillSearchHit>,
        note: Option<String>,
        error: Option<String>,
    },
    Session(EventKind),
    /// Text from the driver for the conversation, such as a note about what a session can do.
    Notice(String),
    /// Something long to read in a pager, such as the last request.
    Pager {
        title: String,
        text: String,
    },
    /// This session's compactions, oldest first, each with when it happened in words.
    Compactions(Vec<(String, recode_core::usage::CompactionReport)>),
    /// What `/init` would write, for the user to confirm.
    InitPreview {
        path: String,
        text: String,
    },
    /// The background commands, for `/tasks`.
    Tasks(Vec<TaskInfo>),
    /// The grants in force, for `/permissions`.
    Permissions {
        mode: recode_core::protocol::PermissionMode,
        /// What this session allowed, in words.
        session: Vec<String>,
        /// What the project remembers: (the grant as JSON, in words).
        project: Vec<(String, String)>,
    },
    /// The instruction files in force, for `/memory`.
    InstructionFiles {
        files: Vec<InstructionFileInfo>,
        /// Where the user's own file lives (it may not exist yet).
        user_path: String,
        /// Where the project's file would live, when there is a project.
        project_path: Option<String>,
    },
    Extensions(ExtensionsSnapshot),
    /// A short result to show on the home screen, and whether it's a problem.
    HomeMessage {
        text: String,
        is_error: bool,
    },
    /// Whether the active model can use tools, and a one-line summary for the panel.
    /// The project's files, for `@` mentions.
    ProjectFiles(Vec<String>),
    /// This folder's saved sessions, most recent first.
    Sessions(Vec<SessionSummary>),
    /// Fresh hardware figures for the welcome screen's meters.
    Hardware(Vec<crate::welcome::Meter>),
    /// The theme chosen last time.
    Theme {
        name: String,
        /// "dim", "normal" or "bright".
        variant: String,
        /// "#rrggbb", or empty for the theme's own accent.
        accent: String,
        solid: bool,
    },
    /// The user's own themes (from their theme files), and any files that couldn't be read.
    UserThemes {
        themes: Vec<crate::theme::Palette>,
        problems: Vec<String>,
    },
    /// Every remembered setting, at startup.
    Settings(std::collections::BTreeMap<String, String>),
    /// Earlier prompts across every project, oldest first, for Ctrl+R.
    HistoryAll(Vec<String>),
    /// The context window in force for the active model, and the user's setting for it.
    ContextWindow {
        window: recode_core::context::ContextWindow,
        setting: recode_core::context::WindowSetting,
        /// Bytes of key/value cache per token of context for this model, when known.
        kv_bytes_per_token: Option<u64>,
    },
    /// Earlier prompts in this project, oldest first.
    History(Vec<String>),
    /// A saved session was taken up: show the conversation, with this model if it's still available.
    Resumed {
        model: Option<String>,
    },
    ToolStatus {
        active: bool,
        summary: String,
    },
}

/// Something the user asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiCommand {
    Refresh,
    /// Start a stopped runtime by the id the Runtimes tab showed, after the user approved its command.
    StartRuntime(String),
    /// Trust the project folder's configuration as it is now, after the user saw what it holds.
    TrustProject,
    ChooseModel(String),
    Send {
        text: String,
        images: Vec<ImageData>,
    },
    Interrupt,
    ResolveApproval(ApprovalId, ApprovalDecision),
    /// Undo the file changes of the most recent turn that made any.
    Undo,
    /// Compact the conversation now, with an optional focus the summary keeps (remembered for later
    /// automatic compactions until changed).
    Compact {
        focus: Option<String>,
    },
    /// Show the next request as it would be sent, for `/context raw`.
    ShowLastRequest,
    /// List this session's compactions, answered with `UiEvent::Compactions`.
    ListCompactions,
    /// Wind the conversation back to before request `to_turn`, restoring files too when asked, or
    /// keeping a summary of what was removed.
    Rewind {
        to_turn: usize,
        files: bool,
        summarize: bool,
    },
    /// Prompts from every project, answered with `UiEvent::HistoryAll`.
    LoadHistory,
    /// Write the conversation (already rendered) to a file; `None` picks a dated name in the project.
    Export {
        path: Option<String>,
        text: String,
    },
    /// Run a command the user typed with `!`, in the sandbox, and show it like a tool call; or start it
    /// in the background (`!cmd &`).
    RunShell {
        command: String,
        network: bool,
        background: bool,
    },
    /// List the background commands, answered with `UiEvent::Tasks`.
    ListTasks,
    /// Stop a background command.
    KillTask(String),
    /// Show a background command's output in the pager.
    ShowTaskOutput(String),
    /// Switch how much the agent may do without asking.
    SetPermissionMode(recode_core::protocol::PermissionMode),
    /// List the grants in force, answered with `UiEvent::Permissions`.
    ListPermissions,
    /// Forget a project grant (its JSON as `UiEvent::Permissions` gave it).
    RevokePermission(String),
    /// Show the plan written in plan mode.
    ShowPlan,
    /// Prepare a first `RE-CODE.md` for the project, answered with `UiEvent::InitPreview`.
    Init,
    /// Write the file `/init` previewed.
    WriteInit,
    /// List the instruction files, answered with `UiEvent::InstructionFiles`.
    ListInstructions,
    /// Append a note the user typed with `#` to an instruction file.
    WriteNote {
        scope: NoteScope,
        text: String,
    },
    /// Remove a memory re-code saved (scope "project" or "user").
    ForgetMemory {
        scope: String,
        name: String,
    },
    /// Show the messages a compaction replaced.
    ShowCompaction(u64),
    /// Start afresh; the current conversation stays saved.
    NewConversation,
    /// Continue the most recent saved session in this folder.
    ContinueLast,
    /// Let the agent read and change this folder (and commands reach it) for the session.
    AddFolder(String),
    /// List this folder's saved sessions, answered with `UiEvent::Sessions`.
    ListSessions,
    /// Delete a saved session by id; the list is sent again afterwards.
    DeleteSession(String),
    /// Remember a theme for next time.
    SetTheme {
        name: String,
        variant: String,
        accent: String,
        solid: bool,
    },
    /// Remember a setting for next time (an empty value forgets it).
    SetSetting {
        key: String,
        value: String,
    },
    /// Remember how this model's context is budgeted, and apply it if the model is active.
    SetContextWindow {
        model: String,
        setting: recode_core::context::WindowSetting,
    },
    /// Continue a saved session, by id.
    ContinueSession(String),
    Extension(ExtensionCommand),
}

/// The interface's end of the connection.
pub struct Link {
    pub events: UnboundedReceiver<UiEvent>,
    pub commands: UnboundedSender<UiCommand>,
}
