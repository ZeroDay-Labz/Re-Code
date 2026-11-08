//! Interface state. Events change it, keys produce actions, and nothing here touches the terminal.

use crate::form::{ConfirmState, Confirmed, FormState};
use crate::input::Input;
use crate::install_view::InstallState;
use crate::sanitize::sanitize;
use crate::theme::ThemeSelection;
use crate::transcript::{Entry, RenderOptions, ToolEntry, ToolStatus, Transcript};
use crate::welcome::{HomeTab, WelcomeInfo, WelcomeModel, WelcomeState};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::style::Color;
use recode_core::context::{ContextWindow, Size, Trigger, WindowSetting};
use recode_core::extensions::{
    ExtensionCommand, ExtensionsSnapshot, MCP_PRESETS, McpTransport, SkillSource, WEB_SEARCH_PRESETS,
};
use recode_core::ids::ApprovalId;
use recode_core::message::ImageData;
use recode_core::message::ToolCall;
use recode_core::protocol::{AgentState, ApprovalDecision, EventKind, PermissionMode, TurnOutcome};
use recode_core::providers::{CloudProvider, PROVIDER_PRESETS, ProviderKind, ProviderPreset};
use recode_core::usage::TokenCount;
use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

/// What the interface wants done. The caller decides how: the demo and the engine differ here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Send {
        text: String,
        images: Vec<ImageData>,
    },
    Interrupt,
    ResolveApproval(ApprovalId, ApprovalDecision),
    /// Hand the mouse back to the terminal for native selection, or take it again.
    SetMouseCapture(bool),
    /// Use this model from now on.
    ChooseModel(String),
    /// Look again for runtimes and models.
    Refresh,
    /// Change the configured extensions.
    Extension(ExtensionCommand),
    /// Start the runtime with this id (the user approved its command in a Confirm overlay).
    StartRuntime(String),
    /// Trust the project folder's configuration (the user saw what it holds in a Confirm overlay).
    TrustProject,
    /// Open a file in the user's editor.
    EditFile(String),
    /// Copy text to the system clipboard.
    Copy(String),
    /// Read an image from the clipboard and attach it.
    PasteImage,
    /// Undo the file changes of the most recent turn that made any.
    Undo,
    /// Compact the conversation now, keeping `focus` in the summary when given.
    Compact {
        focus: Option<String>,
    },
    /// Show the next request as it would be sent (`/context raw`).
    ShowLastRequest,
    /// List this session's compactions.
    ListCompactions,
    /// Run a `!command` in the sandbox and show it like a tool call, or start it in the background.
    RunShell {
        command: String,
        network: bool,
        background: bool,
    },
    /// List the background commands.
    ListTasks,
    /// Stop a background command.
    KillTask(String),
    /// Show a background command's output.
    ShowTaskOutput(String),
    /// Go back to before request `to_turn`.
    Rewind {
        to_turn: usize,
        files: bool,
        summarize: bool,
    },
    /// Fetch prompts from every project, for the history search.
    LoadHistory,
    /// Delete a saved session (the user confirmed).
    DeleteSession(String),
    /// Write the whole conversation into the terminal's own scrollback, so native search and selection
    /// work on it, then come back.
    DumpScrollback(String),
    /// Write the rendered conversation to a file.
    Export {
        path: Option<String>,
        text: String,
    },
    /// Switch how much the agent may do without asking.
    SetPermissionMode(PermissionMode),
    /// List the grants in force.
    ListPermissions,
    /// Forget a project grant, by its JSON.
    RevokePermission(String),
    /// Show the plan written in plan mode.
    ShowPlan,
    /// Prepare a first `RE-CODE.md` for this project.
    Init,
    /// Write the `RE-CODE.md` that was previewed.
    WriteInit,
    /// List the instruction files in force.
    ListInstructions,
    /// Append a `#` note to an instruction file.
    WriteNote {
        scope: crate::link::NoteScope,
        text: String,
    },
    ForgetMemory {
        scope: String,
        name: String,
    },
    /// Show the messages a compaction replaced.
    ShowCompaction(u64),
    /// Remember and apply how this model's context is budgeted.
    SetContextWindow {
        model: String,
        setting: WindowSetting,
    },
    /// Start afresh; the current conversation stays saved.
    NewConversation,
    /// Continue the last saved session in this folder.
    ContinueLast,
    /// Let the agent read and change a folder for this session.
    AddFolder(String),
    /// Open a web page in the user's browser.
    OpenUrl(String),
    /// Remember a setting (an empty value forgets it).
    SetSetting {
        key: String,
        value: String,
    },
    /// Ask for this folder's saved sessions, to pick one.
    ListSessions,
    /// Remember this theme for next time.
    SetTheme(ThemeSelection),
    /// Continue a saved session, by id.
    ContinueSession(String),
    Quit,
}

/// Which screen is showing.
#[derive(Debug, Clone, PartialEq)]
pub enum Screen {
    Welcome(WelcomeState),
    Conversation,
}

/// A window over the conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Overlay {
    Help,
    /// What fills the context window, by part.
    Context,
    /// This session's compactions, newest first.
    Compactions {
        reports: Vec<(String, recode_core::usage::CompactionReport)>,
        selected: usize,
    },
    /// `/tasks`: the background commands.
    Tasks {
        tasks: Vec<crate::link::TaskInfo>,
        selected: usize,
    },
    /// Ctrl+X: the whole conversation as plain lines, with search; `[` hands it to the terminal.
    Transcript {
        offset: usize,
        /// The search being typed, when `/` was pressed.
        typing: Option<String>,
        /// The last search run.
        query: String,
        /// Lines matching the query.
        matches: Vec<usize>,
        current: usize,
    },
    /// Ctrl+R: search earlier prompts.
    HistorySearch {
        query: String,
        /// Across every project, not just this one.
        all_projects: bool,
        selected: usize,
    },
    /// Esc Esc: go back to an earlier request.
    Rewind {
        /// (turn number from 0, first line), newest first.
        turns: Vec<(usize, String)>,
        selected: usize,
        /// 0 conversation, 1 conversation and files, 2 summarize from here.
        choice: usize,
    },
    /// `/permissions`: the mode and the grants in force.
    Permissions {
        mode: PermissionMode,
        session: Vec<String>,
        project: Vec<(String, String)>,
        selected: usize,
    },
    /// `/memory`: the instruction files in force.
    Memory {
        files: Vec<crate::link::InstructionFileInfo>,
        user_path: String,
        project_path: Option<String>,
        selected: usize,
    },
    /// Where a `# note` should go.
    NoteTarget {
        text: String,
        selected: usize,
    },
    Palette {
        query: String,
        selected: usize,
    },
    Pager {
        title: String,
        text: String,
        is_diff: bool,
        /// Old and new side by side (diffs only, when the window is wide).
        side_by_side: bool,
        offset: usize,
    },
    /// Which code block of the last answer to copy.
    CodeBlocks {
        /// (language, code)
        blocks: Vec<(String, String)>,
        selected: usize,
    },
    Form(FormState),
    Confirm(ConfirmState),
    /// The one-click MCP preset picker.
    McpPresets {
        selected: usize,
    },
    /// `/web`: web search for the model, as an MCP server to set up.
    WebSetup {
        selected: usize,
    },
    /// Searching the MCP registry: type, Enter searches, Enter on a hit adds it.
    McpSearch {
        query: String,
        hits: Vec<recode_core::extensions::McpSearchHit>,
        selected: usize,
        searching: bool,
        error: Option<String>,
    },
    /// Installing a skill from a hub: ask, fetch, preview, decide.
    Install(InstallState),
    /// A marketplace's or a search's plugins: type to filter, Enter fetches one.
    PluginBrowse {
        title: String,
        listings: Vec<recode_core::extensions::PluginListing>,
        filter: String,
        selected: usize,
        loading: bool,
        error: Option<String>,
    },
    /// Searching the skill hubs: type, Enter searches, Enter on a hit fetches it.
    SkillSearch {
        query: String,
        hits: Vec<recode_core::extensions::SkillSearchHit>,
        selected: usize,
        searching: bool,
        error: Option<String>,
        note: Option<String>,
    },
    /// Choosing which of a server's tools the model may use.
    ToolFilter {
        server: String,
        tools: Vec<(String, bool)>,
        selected: usize,
    },
    /// The one-click provider preset picker.
    ProviderPresets {
        selected: usize,
    },
    /// The theme picker, previewing each theme as it's highlighted.
    Themes {
        selected: usize,
        /// What was in use when the picker opened, restored on Esc.
        original: ThemeSelection,
    },
    /// Getting a key for free models: pick a provider, then paste the key.
    FreeSetup(FreeStep),
    /// This folder's saved sessions, to pick one to continue.
    Sessions {
        sessions: Vec<crate::link::SessionSummary>,
        selected: usize,
    },
}

/// A command in the palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteCommand {
    CopyCode,
    SwitchModel,
    Extensions,
    TogglePanel,
    ToggleReasoning,
    ToggleSelectMode,
    LastOutput,
    Undo,
    Compact,
    Context,
    NewConversation,
    ContinueLast,
    Theme,
    FreeModels,
    TrustProject,
    InstallSkill,
    InstallPlugin,
    WebSearch,
    Help,
    Quit,
}

pub const PALETTE: &[(PaletteCommand, &str, &str)] = &[
    (PaletteCommand::CopyCode, "Copy the last code block", "Ctrl+Y"),
    (PaletteCommand::SwitchModel, "Switch model", "Ctrl+L"),
    (PaletteCommand::Extensions, "MCP servers, skills and tools", "Ctrl+E"),
    (PaletteCommand::TogglePanel, "Side panel: session, files, hidden", "Ctrl+B"),
    (PaletteCommand::ToggleReasoning, "Show or hide reasoning", "Ctrl+T"),
    (PaletteCommand::ToggleSelectMode, "Select text with the terminal", "F2"),
    (PaletteCommand::LastOutput, "Open the last tool output", "Ctrl+O"),
    (PaletteCommand::Undo, "Undo the last turn's file changes", "Ctrl+Z"),
    (PaletteCommand::NewConversation, "New conversation", ""),
    (PaletteCommand::ContinueLast, "Continue the last session here", ""),
    (PaletteCommand::Compact, "Compact the conversation now", "/compact"),
    (PaletteCommand::Context, "What fills the context window", "/context"),
    (PaletteCommand::Theme, "Change theme", ""),
    (PaletteCommand::FreeModels, "Get free models", "/free"),
    (PaletteCommand::TrustProject, "Trust this folder's configuration (project skills)", "/trust"),
    (PaletteCommand::InstallSkill, "Install a skill from a hub (Hermes, GitHub, skills.sh)", "/skill install"),
    (PaletteCommand::InstallPlugin, "Install a plugin (Hermes catalog, Claude Code marketplaces)", "/plugin install"),
    (PaletteCommand::WebSearch, "Give the model web search (SearXNG, DuckDuckGo, Brave…)", "/web"),
    (PaletteCommand::Help, "Keyboard shortcuts", "F1"),
    (PaletteCommand::Quit, "Quit", "Ctrl+C twice"),
];

/// "~41k" for an estimated token count.
fn approx_tokens(tokens: u64) -> String {
    if tokens >= 10_000 {
        format!("~{}k", tokens / 1000)
    } else if tokens >= 1000 {
        format!("~{}.{}k", tokens / 1000, tokens % 1000 / 100)
    } else {
        format!("~{tokens}")
    }
}

/// What a compaction did, in plain words. The sizes are estimates and say so.
fn compaction_notice(
    before: u64,
    after: u64,
    elided: usize,
    summarised: usize,
    problem: Option<String>,
    focus: Option<&str>,
) -> String {
    let focus_text = focus.map(|focus| format!(" (focus: {focus})")).unwrap_or_default();
    let mut parts = Vec::new();
    if elided > 0 {
        let plural = if elided == 1 { "" } else { "s" };
        parts.push(format!(
            "Shortened {elided} old tool result{plural} (the model can bring one back with read_result)"
        ));
    }
    if summarised > 0 {
        parts.push(format!("summarised {summarised} earlier messages, keeping your request and the latest steps"));
    }
    let mut text = if parts.is_empty() {
        format!(
            "Nothing to compact{focus_text}: the conversation is {} tokens (estimated), and the latest steps are \
             always kept whole.",
            approx_tokens(after)
        )
    } else {
        format!(
            "Compacted the conversation{focus_text}: {} → {} tokens (estimated). {}.",
            approx_tokens(before),
            approx_tokens(after),
            parts.join("; ")
        )
    };
    if let Some(problem) = problem {
        text.push(' ');
        text.push_str(&problem);
    }
    text
}

/// [`palette_matches`], with the commands used lately first when nothing is typed.
#[must_use]
pub fn palette_matches_recent(
    recent: &[PaletteCommand],
    query: &str,
) -> Vec<(PaletteCommand, &'static str, &'static str)> {
    let mut entries = palette_matches(query);
    if query.trim().is_empty() {
        for command in recent.iter().rev() {
            if let Some(position) = entries.iter().position(|entry| entry.0 == *command) {
                let entry = entries.remove(position);
                entries.insert(0, entry);
            }
        }
    }
    entries
}

/// Palette entries matching a query: every query character must appear, in order.
#[must_use]
pub fn palette_matches(query: &str) -> Vec<(PaletteCommand, &'static str, &'static str)> {
    let query: String = query.chars().filter(|c| !c.is_whitespace()).collect();
    let mut scored: Vec<(i64, usize, (PaletteCommand, &'static str, &'static str))> = PALETTE
        .iter()
        .enumerate()
        .filter_map(|(order, entry)| {
            let name: String = entry.1.chars().filter(|c| !c.is_whitespace()).collect();
            crate::complete::score(&query, &name).map(|score| (score, order, *entry))
        })
        .collect();
    // Best match first; with no query, the list keeps its usual order.
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    if query.is_empty() {
        scored.sort_by_key(|(_, order, _)| *order);
    }
    scored.into_iter().map(|(_, _, entry)| entry).collect()
}

/// Every fenced code block in markdown, as (language, code), without the fences.
#[must_use]
pub fn code_blocks(text: &str) -> Vec<(String, String)> {
    let mut blocks = Vec::new();
    let mut current: Option<(String, Vec<&str>)> = None;
    for line in text.lines() {
        let trimmed = line.trim_start();
        let fence = trimmed.starts_with("```") || trimmed.starts_with("~~~");
        match (&mut current, fence) {
            (None, true) => {
                let language = trimmed.trim_start_matches(['`', '~']).split_whitespace().next().unwrap_or_default();
                current = Some((language.to_owned(), Vec::new()));
            }
            (Some((_, lines)), false) => lines.push(line),
            (Some(_), true) => {
                if let Some((language, lines)) = current.take() {
                    blocks.push((language, lines.join("\n")));
                }
            }
            (None, false) => {}
        }
    }
    if let Some((language, lines)) = current {
        blocks.push((language, lines.join("\n")));
    }
    blocks.into_iter().filter(|(_, code)| !code.trim().is_empty()).collect()
}

/// Where each file of a diff starts in the pager's rendered lines, with its path.
fn diff_file_starts(diff: &str, width: usize, side_by_side: bool) -> Vec<(usize, String)> {
    let mut starts = Vec::new();
    let mut consumed = String::new();
    for line in diff.lines() {
        if let Some(path) = line.strip_prefix("+++ ") {
            let before = if side_by_side && width >= 100 {
                crate::transcript::render_diff_side_by_side(&consumed, width).len()
            } else {
                crate::transcript::render_diff(&consumed, width.max(1), usize::MAX).len()
            };
            // The header for this file is the line before `+++`, when there was one.
            starts.push((before.saturating_sub(1), path.trim().trim_start_matches("b/").to_owned()));
        }
        consumed.push_str(line);
        consumed.push('\n');
    }
    starts
}

/// Extracts the last fenced code block's contents from markdown, without the fences.
#[must_use]
pub fn last_code_block(text: &str) -> Option<String> {
    let mut blocks = Vec::new();
    let mut current: Option<Vec<&str>> = None;
    for line in text.lines() {
        let trimmed = line.trim_start();
        let fence = trimmed.starts_with("```") || trimmed.starts_with("~~~");
        match (&mut current, fence) {
            (None, true) => current = Some(Vec::new()),
            (Some(lines), false) => lines.push(line),
            (Some(_), true) => {
                if let Some(lines) = current.take() {
                    blocks.push(lines.join("\n"));
                }
            }
            (None, false) => {}
        }
    }
    // A still-open fence (a response mid-stream) counts too.
    if let Some(lines) = current {
        blocks.push(lines.join("\n"));
    }
    blocks.into_iter().rev().find(|block| !block.trim().is_empty())
}

/// A file changed during the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedFile {
    pub path: String,
    pub added: usize,
    pub removed: usize,
}

/// All interface state.
pub struct App {
    pub transcript: Transcript,
    pub input: Input,
    pub state: AgentState,
    state_since: Instant,
    pub now: Instant,
    pub project: String,
    pub title: String,
    pub model: String,
    pub provider: String,
    /// Set for recorded sessions, and always displayed.
    pub demo: bool,
    pub context: Option<(TokenCount, u64)>,
    /// The window the active model is budgeted against, and the user's setting for it.
    pub context_window: Option<ContextWindow>,
    pub context_setting: WindowSetting,
    /// Bytes of key/value cache per token for the active model, when the runtime says.
    pub kv_bytes_per_token: Option<u64>,
    /// How much the agent may do without asking (Shift+Tab cycles it).
    pub permission_mode: PermissionMode,
    /// Messages typed while the agent works, sent after its tool calls or as the next turn.
    pub queue: std::collections::VecDeque<(String, Vec<ImageData>)>,
    /// A selection outside the conversation text, over the last frame's cells.
    pub screen_selection: Option<ScreenSelection>,
    /// Where such a drag began, and the region it's clipped to.
    screen_anchor: Option<((u16, u16), ratatui::layout::Rect)>,
    /// The screen's regions in the last frame, for clipping a drag to the one it started in.
    pub region_rects: Vec<ratatui::layout::Rect>,
    /// The whole screen in the last frame.
    pub screen_rect: ratatui::layout::Rect,
    /// The last frame's cells, one string per cell (the second cell of a wide character is empty).
    pub last_frame: Vec<Vec<String>>,
    /// Prompts from every project, once asked for (Ctrl+R, Tab).
    pub history_all: Option<Vec<String>>,
    /// When Esc was last pressed on an empty, idle prompt: a second press within a moment rewinds.
    last_esc: Option<Instant>,
    /// Whether the terminal reports modified Enter (kitty keyboard protocol), so Ctrl+Enter can mean "send now".
    pub keyboard_enhanced: bool,
    /// What the last request held, by part.
    pub ledger: Option<recode_core::usage::Ledger>,
    /// The provider's figures for the last request.
    pub last_usage: Option<recode_core::usage::ReportedUsage>,
    /// Output speed from the last provider report.
    pub tokens_per_second: Option<f64>,
    pub overlay: Option<Overlay>,
    /// `None` means decide from the terminal's width.
    pub panel_preference: Option<bool>,
    pub options: RenderOptions,
    pub select_mode: bool,
    pub quit_armed: bool,
    /// Files changed during the session, totalled from `file_changes`.
    pub files: Vec<ChangedFile>,
    /// Each change with the turn that made it, so undoing a turn takes back only its share.
    file_changes: Vec<(usize, ChangedFile)>,
    /// Turns started so far, numbering them for `file_changes`.
    turns_started: usize,
    pub tools_run: usize,
    /// Set by each draw, and used to scroll by a page.
    pub view_size: (usize, usize),
    pub terminal_width: usize,
    pub input_width: usize,
    pub screen: Screen,
    /// What the driver last reported about this machine, kept for returning to the welcome screen.
    pub welcome_info: Option<WelcomeInfo>,
    /// The runtimes and containers found, for the Runtimes tab.
    pub runtimes: Option<crate::link::RuntimesInfo>,
    /// A runtime being started: its id and what's happening ("starting…", "waiting 10 s…").
    pub runtime_starting: Option<(String, String)>,
    /// The project folder's own configuration and whether it's trusted.
    pub project_trust: Option<crate::link::ProjectTrust>,
    pub reduced_motion: bool,
    /// Rows of the model list last drawn, used to page through it.
    pub model_list_rows: usize,
    pub extensions: Option<ExtensionsSnapshot>,
    pub home_message: Option<(String, bool)>,
    /// Where clickable things were drawn last frame.
    pub hits: Hits,
    pub created: Instant,
    /// Characters of model output per 100 ms, as (time slot, count).
    pub activity: VecDeque<(u64, u32)>,
    /// Images attached to the next message (from the clipboard).
    pub pending_images: Vec<ImageData>,
    /// Whether the active model can see images.
    pub model_vision: bool,
    pub tool_status: Option<(bool, String)>,
    /// The project's files, for `@` mentions and clickable paths.
    pub project_files: Vec<String>,
    /// The same, for fast lookups.
    pub project_file_set: std::collections::HashSet<String>,
    /// The highlighted suggestion in the completion popup.
    pub completion_selected: usize,
    /// The input as it was when the popup was dismissed with Esc; it stays away until the input changes.
    completion_dismissed: Option<String>,
    /// Earlier prompts in this project, oldest first.
    pub history: Vec<String>,
    /// While browsing history: the position, and what was typed before browsing began.
    history_position: Option<(usize, String)>,
    /// Text selected by dragging in the conversation.
    pub selection: Option<Selection>,
    /// Where the left button went down, on screen and in the conversation, until it's released.
    mouse_anchor: Option<((u16, u16), (usize, u16))>,
    /// While dragging past the top (negative) or bottom (positive) of the conversation: how far,
    /// which sets how fast it scrolls.
    drag_edge: i16,
    /// The last click, for double- and triple-clicks: when, where, and how many in a row.
    last_click: Option<(Instant, (u16, u16), u8)>,
    /// Where the conversation was drawn last (x, y, width, height), and its first line shown.
    pub transcript_rect: (u16, u16, u16, u16),
    pub transcript_top: usize,
    /// Messages shown in the corner for a few seconds, newest last.
    pub toasts: VecDeque<Toast>,
    /// Things an event asked of the driver, performed by the run loop after the event (an event
    /// handler returns nothing itself).
    pub pending_actions: Vec<Action>,
    /// Whether the terminal window has focus (for notifications).
    pub focused: bool,
    /// Settings remembered by the driver, as key → value.
    pub settings: BTreeMap<String, String>,
    /// Which side panel shows: the session, or the project's files.
    pub panel_tab: PanelTab,
    /// The side panel's width in columns, dragged or remembered.
    pub panel_width: u16,
    /// A drag of the panel's left edge is in progress.
    panel_drag: bool,
    /// The Files pane's folded tree.
    pub tree: crate::tree::Tree,
    /// Palette commands run lately, newest first, shown first in an empty palette.
    pub palette_recent: Vec<PaletteCommand>,
    /// This folder's saved sessions, most recent first, for the Home tab.
    pub recent_sessions: Vec<crate::link::SessionSummary>,
    /// The user asked for the session picker; the next list of sessions opens it.
    sessions_wanted: bool,
    /// The model's task list, from the `todo` tool.
    pub todos: Vec<recode_core::protocol::TodoItem>,
    /// Whether the side panel was drawn last frame (the status line shows tasks when it wasn't).
    pub panel_shown: bool,
    /// When the running turn began, how many tools it has run, and the tokens reported so far.
    turn_started: Option<Instant>,
    turn_tools: usize,
    turn_tokens: (Option<u64>, Option<u64>, Option<u64>),
    /// When each tool call started, by id, for its duration.
    tool_started: BTreeMap<String, Instant>,
    /// The theme in use: name, variant, accent, and whether it paints its own background.
    pub theme: ThemeSelection,
    /// `config.toml` names a theme, which wins over a saved choice.
    pub theme_locked: bool,
}

/// Text being selected with the mouse, as positions in the whole conversation: (line, column).
/// Being independent of the screen, it survives scrolling and can be longer than the view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub anchor: (usize, u16),
    pub head: (usize, u16),
}

/// Text being selected anywhere else on the screen (a panel, the prompt, a tab, an overlay), as screen
/// cells, clipped to the region the drag began in so it never bleeds into a neighbour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenSelection {
    pub anchor: (u16, u16),
    pub head: (u16, u16),
    pub clip: ratatui::layout::Rect,
}

impl ScreenSelection {
    /// Start and end in reading order: (row, column) pairs.
    #[must_use]
    pub fn ordered(&self) -> ((u16, u16), (u16, u16)) {
        let a = (self.anchor.1, self.anchor.0);
        let h = (self.head.1, self.head.0);
        if a <= h { (a, h) } else { (h, a) }
    }

    /// Whether a cell is selected, reading row by row within the clip, like a terminal.
    #[must_use]
    pub fn contains(&self, column: u16, row: u16) -> bool {
        let clip = self.clip;
        if column < clip.x || column >= clip.x + clip.width || row < clip.y || row >= clip.y + clip.height {
            return false;
        }
        let ((r0, c0), (r1, c1)) = self.ordered();
        if row < r0 || row > r1 {
            return false;
        }
        (row > r0 || column >= c0) && (row < r1 || column <= c1)
    }

    /// The selected text from the last frame's cells, one line per row, trailing spaces trimmed.
    #[must_use]
    pub fn text(&self, frame: &[Vec<String>]) -> String {
        let ((r0, c0), (r1, c1)) = self.ordered();
        let mut out = Vec::new();
        for row in r0..=r1 {
            let Some(cells) = frame.get(usize::from(row)) else { continue };
            let from = if row == r0 { c0 } else { self.clip.x };
            let to = if row == r1 { c1 } else { self.clip.x + self.clip.width.saturating_sub(1) };
            let line: String = (from..=to).filter_map(|x| cells.get(usize::from(x)).map(String::as_str)).collect();
            out.push(line.trim_end().to_owned());
        }
        out.join("\n").trim_matches('\n').to_owned()
    }
}

impl Selection {
    /// Start and end in reading order.
    #[must_use]
    pub fn ordered(&self) -> ((usize, u16), (usize, u16)) {
        if self.anchor <= self.head { (self.anchor, self.head) } else { (self.head, self.anchor) }
    }

    /// Whether a position is selected, reading line by line like a terminal does.
    #[must_use]
    pub fn contains(&self, line: usize, column: u16) -> bool {
        let ((l0, c0), (l1, c1)) = self.ordered();
        if line < l0 || line > l1 {
            return false;
        }
        (line > l0 || column >= c0) && (line < l1 || column <= c1)
    }
}

/// Where the free-models setup is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FreeStep {
    /// Choosing between the providers that offer free models.
    Pick { selected: usize },
    /// Pasting the key for one provider.
    Key {
        name: String,
        kind: ProviderKind,
        base_url: Option<String>,
        signup_url: Option<String>,
        /// Set when the provider is already configured (the key is replaced).
        original: Option<String>,
        key: String,
    },
}

impl FreeStep {
    /// The presets that offer free models.
    #[must_use]
    pub fn free_presets() -> Vec<&'static ProviderPreset> {
        PROVIDER_PRESETS.iter().filter(|preset| preset.free_models).collect()
    }

    fn key_for_preset(preset: &ProviderPreset, original: Option<String>) -> Self {
        Self::Key {
            name: preset.name.to_owned(),
            kind: preset.kind,
            base_url: (!preset.base_url.is_empty()).then(|| preset.base_url.to_owned()),
            signup_url: Some(preset.signup_url.to_owned()),
            original,
            key: String::new(),
        }
    }
}

/// A short message shown in the corner for a few seconds, for things that don't belong in the
/// conversation: confirmations, small mistakes, "not now".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub text: String,
    pub level: ToastLevel,
    pub at: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastLevel {
    Info,
    Success,
    Warning,
}

/// How long a toast stays.
pub const TOAST_SECONDS: u64 = 4;

/// What the side panel shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PanelTab {
    #[default]
    Session,
    Files,
}

/// Clickable places on screen.
#[derive(Debug, Default, Clone)]
pub struct Hits {
    /// (row, first column, end column, tab)
    pub tabs: Vec<(u16, u16, u16, HomeTab)>,
    /// (row, first column, end column, item index)
    pub rows: Vec<(u16, u16, u16, usize)>,
    /// Conversation lines: (screen row, transcript entry index), for clicking reasoning blocks.
    /// (row, first column, end column, transcript entry)
    pub lines: Vec<(u16, u16, u16, usize)>,
    /// The "↓ N more lines" badge: (row, first column, end column).
    pub badge: Option<(u16, u16, u16)>,
    /// Project paths mentioned in the conversation: (row, first column, end column, path).
    pub paths: Vec<(u16, u16, u16, String)>,
    /// Rows of the Files pane: (row, first column, end column, index into the tree's rows).
    pub tree_rows: Vec<(u16, u16, u16, usize)>,
    /// Where the side panel was drawn, for resizing and scrolling it.
    pub panel: Option<ratatui::layout::Rect>,
    /// Changed-file rows in the side panel: (row, first column, end column, path).
    pub panel_files: Vec<(u16, u16, u16, String)>,
}

const ACTIVITY_SLOT_MS: u128 = 100;
const ACTIVITY_SLOTS: usize = 400;

impl App {
    #[must_use]
    pub fn new(now: Instant, project: &str, model: &str, provider: &str, demo: bool) -> Self {
        Self {
            transcript: Transcript::default(),
            input: Input::default(),
            state: AgentState::Idle,
            state_since: now,
            now,
            project: project.to_owned(),
            title: String::new(),
            model: model.to_owned(),
            provider: provider.to_owned(),
            demo,
            context: None,
            context_window: None,
            context_setting: WindowSetting::default(),
            kv_bytes_per_token: None,
            permission_mode: PermissionMode::default(),
            queue: std::collections::VecDeque::new(),
            screen_selection: None,
            screen_anchor: None,
            region_rects: Vec::new(),
            screen_rect: ratatui::layout::Rect::default(),
            last_frame: Vec::new(),
            history_all: None,
            last_esc: None,
            keyboard_enhanced: false,
            ledger: None,
            last_usage: None,
            tokens_per_second: None,
            overlay: None,
            panel_preference: None,
            options: RenderOptions::default(),
            select_mode: false,
            quit_armed: false,
            files: Vec::new(),
            file_changes: Vec::new(),
            turns_started: 0,
            tools_run: 0,
            view_size: (80, 20),
            terminal_width: 80,
            input_width: 76,
            screen: Screen::Conversation,
            welcome_info: None,
            runtimes: None,
            runtime_starting: None,
            project_trust: None,
            reduced_motion: false,
            model_list_rows: 8,
            extensions: None,
            home_message: None,
            hits: Hits::default(),
            created: now,
            activity: VecDeque::new(),
            pending_images: Vec::new(),
            model_vision: false,
            tool_status: None,
            project_files: Vec::new(),
            project_file_set: std::collections::HashSet::new(),
            completion_selected: 0,
            completion_dismissed: None,
            history: Vec::new(),
            history_position: None,
            selection: None,
            mouse_anchor: None,
            drag_edge: 0,
            last_click: None,
            transcript_rect: (0, 0, 0, 0),
            transcript_top: 0,
            toasts: VecDeque::new(),
            pending_actions: Vec::new(),
            focused: true,
            settings: BTreeMap::new(),
            panel_tab: PanelTab::Session,
            panel_width: crate::view::PANEL_WIDTH,
            panel_drag: false,
            tree: crate::tree::Tree::default(),
            palette_recent: Vec::new(),
            recent_sessions: Vec::new(),
            sessions_wanted: false,
            todos: Vec::new(),
            panel_shown: false,
            turn_started: None,
            turn_tools: 0,
            turn_tokens: (None, None, None),
            tool_started: BTreeMap::new(),
            theme: ThemeSelection::default(),
            theme_locked: false,
        }
    }

    /// True while something is happening that the user could interrupt.
    #[must_use]
    pub fn is_busy(&self) -> bool {
        self.state != AgentState::Idle
    }

    /// True while the screen changes with time. Otherwise nothing is redrawn, and an idle session uses no CPU.
    #[must_use]
    pub fn needs_ticks(&self) -> bool {
        match &self.screen {
            Screen::Welcome(welcome) => {
                let probing = self.extensions.as_ref().is_some_and(|extensions| !extensions.probing.is_empty());
                !self.reduced_motion && (self.welcome_info.is_none() || probing || welcome.intro_running(self.now))
            }
            Screen::Conversation => {
                (self.is_busy() && self.state != AgentState::AwaitingApproval)
                    || self.toasts_live()
                    || self.edge_scrolling()
            }
        }
    }

    /// The palette's entries for a query: with nothing typed, the commands used lately come first.
    #[must_use]
    pub fn palette_entries(&self, query: &str) -> Vec<(PaletteCommand, &'static str, &'static str)> {
        palette_matches_recent(&self.palette_recent, query)
    }

    /// How long the running turn has taken so far.
    #[must_use]
    pub fn turn_elapsed(&self) -> Option<std::time::Duration> {
        self.turn_started.map(|started| self.now.saturating_duration_since(started))
    }

    /// Shows a toast. At most three are kept; older ones go first.
    pub fn toast(&mut self, text: impl Into<String>, level: ToastLevel) {
        self.toasts.push_back(Toast { text: sanitize(&text.into()), level, at: self.now });
        while self.toasts.len() > 3 {
            self.toasts.pop_front();
        }
    }

    /// The toasts still worth showing, dropping expired ones.
    pub fn live_toasts(&mut self) -> Vec<Toast> {
        let limit = std::time::Duration::from_secs(TOAST_SECONDS);
        let now = self.now;
        self.toasts.retain(|toast| now.saturating_duration_since(toast.at) < limit);
        self.toasts.iter().cloned().collect()
    }

    fn toasts_live(&self) -> bool {
        let limit = std::time::Duration::from_secs(TOAST_SECONDS);
        self.toasts.iter().any(|toast| self.now.saturating_duration_since(toast.at) < limit)
    }

    /// The most recent toast's text, for tests and the status line.
    #[must_use]
    pub fn last_toast(&self) -> Option<&str> {
        self.toasts.back().map(|toast| toast.text.as_str())
    }

    /// Whether a selection is being dragged past the edge, so the view keeps scrolling.
    #[must_use]
    pub fn edge_scrolling(&self) -> bool {
        self.mouse_anchor.is_some() && self.drag_edge != 0
    }

    /// A screen cell as a position in the conversation, clamped into the conversation's area, and how
    /// far outside its top or bottom edge the cell is.
    fn content_point(&self, column: u16, row: u16) -> ((usize, u16), i16) {
        let (x, y, width, height) = self.transcript_rect;
        let column = column.clamp(x, x + width.saturating_sub(1)) - x;
        let bottom = y + height.saturating_sub(1);
        let edge = if row < y {
            -i16::try_from(y - row).unwrap_or(i16::MAX)
        } else if row > bottom {
            i16::try_from(row - bottom).unwrap_or(i16::MAX)
        } else {
            0
        };
        let line = self.transcript_top + usize::from(row.clamp(y, bottom) - y);
        ((line, column), edge)
    }

    /// The left button went down: the start of a click or of a drag. Ctrl+click jumps to the latest.
    pub fn mouse_down(&mut self, column: u16, row: u16, ctrl: bool) -> Vec<Action> {
        self.selection = None;
        self.screen_selection = None;
        self.screen_anchor = None;
        self.drag_edge = 0;
        if self.screen != Screen::Conversation || self.overlay.is_some() {
            // Anywhere else, a drag selects what's on screen; a click still acts.
            self.screen_anchor = Some(((column, row), self.screen_rect));
            return self.click(column, row);
        }
        let on_badge = self.hits.badge.is_some_and(|(y, from, to)| y == row && (from..to).contains(&column));
        if ctrl || on_badge {
            self.transcript.follow();
            return Vec::new();
        }
        // The panel's left edge can be dragged to resize it.
        if let Some(panel) = self.hits.panel
            && column == panel.x
            && row >= panel.y
            && row < panel.y + panel.height
        {
            self.panel_drag = true;
            return Vec::new();
        }
        let (x, y, width, height) = self.transcript_rect;
        if width > 0 && height > 0 && column >= x && column < x + width && row >= y && row < y + height {
            self.mouse_anchor = Some(((column, row), self.content_point(column, row).0));
        } else {
            self.mouse_anchor = None;
            // Outside the conversation text: a drag selects within the region it started in.
            let clip = self
                .region_rects
                .iter()
                .copied()
                .find(|rect| {
                    column >= rect.x && column < rect.x + rect.width && row >= rect.y && row < rect.y + rect.height
                })
                .unwrap_or(self.screen_rect);
            self.screen_anchor = Some(((column, row), clip));
            return self.click(column, row);
        }
        Vec::new()
    }

    /// Dragging with the left button selects text. Past the top or bottom of the conversation it
    /// scrolls, faster the further out the mouse is, and keeps scrolling while held there.
    pub fn mouse_drag(&mut self, column: u16, row: u16) {
        if self.panel_drag {
            let right = u16::try_from(self.terminal_width).unwrap_or(u16::MAX);
            self.panel_width =
                right.saturating_sub(column).clamp(crate::view::PANEL_MIN_WIDTH, crate::view::PANEL_MAX_WIDTH);
            return;
        }
        if let Some((anchor, clip)) = self.screen_anchor {
            let head = (
                column.clamp(clip.x, clip.x + clip.width.saturating_sub(1)),
                row.clamp(clip.y, clip.y + clip.height.saturating_sub(1)),
            );
            self.screen_selection = Some(ScreenSelection { anchor, head, clip });
            return;
        }
        let Some((_, anchor)) = self.mouse_anchor else { return };
        let (head, edge) = self.content_point(column, row);
        self.drag_edge = edge.clamp(-4, 4);
        self.selection = Some(Selection { anchor, head });
        if self.drag_edge != 0 {
            self.edge_scroll();
        }
    }

    /// One step of scrolling while a selection is dragged past an edge; called on every tick too.
    pub fn edge_scroll(&mut self) {
        if !self.edge_scrolling() {
            return;
        }
        let (width, height) = self.view_size;
        self.transcript.scroll_by(isize::from(self.drag_edge), width, height, self.options);
        self.transcript_top = self.transcript.top(width, height, self.options);
        if let Some(selection) = &mut self.selection {
            // The end of the selection follows the edge that's scrolling.
            let line =
                if self.drag_edge < 0 { self.transcript_top } else { self.transcript_top + height.saturating_sub(1) };
            selection.head.0 = line;
        }
    }

    /// The left button came up: copies a selection, or counts as a click if there was no drag.
    pub fn mouse_up(&mut self, _column: u16, _row: u16) -> Vec<Action> {
        if std::mem::take(&mut self.panel_drag) {
            return vec![Action::SetSetting { key: "panel_width".to_owned(), value: self.panel_width.to_string() }];
        }
        let anchor = self.mouse_anchor.take();
        self.screen_anchor = None;
        self.drag_edge = 0;
        if let Some(selection) = self.selection
            && selection.anchor != selection.head
        {
            return self.copy_selection();
        }
        if let Some(selection) = self.screen_selection
            && selection.anchor != selection.head
        {
            return self.copy_screen_selection();
        }
        self.selection = None;
        self.screen_selection = None;
        // A press outside the conversation already acted as a click; releasing does nothing more.
        let Some(((x, y), point)) = anchor else { return Vec::new() };
        // Double-click selects a word, triple-click a whole message; a single click opens and closes.
        let count = match self.last_click {
            Some((at, place, count))
                if place == (x, y)
                    && self.now.saturating_duration_since(at) < std::time::Duration::from_millis(450) =>
            {
                count % 3 + 1
            }
            _ => 1,
        };
        self.last_click = Some((self.now, (x, y), count));
        match count {
            2 => self.select_word(point),
            3 => self.select_message(x, y),
            _ => self.click(x, y),
        }
    }

    /// Selects and copies the word at a position.
    fn select_word(&mut self, (line, column): (usize, u16)) -> Vec<Action> {
        use unicode_segmentation::UnicodeSegmentation;
        let width = self.view_size.0;
        let Some(text) = self.transcript.lines_between(width, self.options, line, line + 1).pop() else {
            return Vec::new();
        };
        let cells: Vec<String> = text
            .spans
            .iter()
            .flat_map(|span| span.content.graphemes(true).map(str::to_owned).collect::<Vec<_>>())
            .collect();
        let word = |cell: &String| !cell.trim().is_empty();
        let at = usize::from(column).min(cells.len().saturating_sub(1));
        if cells.get(at).is_none_or(|cell| !word(cell)) {
            return Vec::new();
        }
        let start = (0..=at).rev().take_while(|&index| word(&cells[index])).last().unwrap_or(at);
        let end = (at..cells.len()).take_while(|&index| word(&cells[index])).last().unwrap_or(at);
        let to_column = |index: usize| u16::try_from(index).unwrap_or(u16::MAX);
        self.selection = Some(Selection { anchor: (line, to_column(start)), head: (line, to_column(end)) });
        self.copy_selection()
    }

    /// Selects and copies the whole message (or tool, or note) at a screen row, however long it is.
    fn select_message(&mut self, column: u16, row: u16) -> Vec<Action> {
        let entry = self.hits.lines.iter().find(|(y, from, to, _)| *y == row && (*from..*to).contains(&column));
        let Some(&(_, _, _, entry)) = entry else { return Vec::new() };
        let width = self.view_size.0;
        let Some((start, end)) = self.transcript.entry_lines(entry, width, self.options) else { return Vec::new() };
        if end <= start {
            return Vec::new();
        }
        self.selection = Some(Selection { anchor: (start, 0), head: (end - 1, u16::MAX) });
        self.copy_selection()
    }

    /// Copies the selected text, confirming it in the status line. The selection stays visible.
    fn copy_selection(&mut self) -> Vec<Action> {
        let text = self.selected_text();
        if text.is_empty() {
            return Vec::new();
        }
        let count = text.chars().count();
        let lines = text.lines().count();
        let what = if lines > 1 { format!("{lines} lines") } else { format!("{count} characters") };
        self.toast(format!("Copied {what}"), ToastLevel::Success);
        vec![Action::Copy(text)]
    }

    /// Copies a selection made outside the conversation text, from the last frame. The selection stays.
    fn copy_screen_selection(&mut self) -> Vec<Action> {
        let Some(selection) = self.screen_selection else { return Vec::new() };
        let text = selection.text(&self.last_frame);
        if text.is_empty() {
            return Vec::new();
        }
        let lines = text.lines().count();
        let what = if lines > 1 { format!("{lines} lines") } else { format!("{} characters", text.chars().count()) };
        self.toast(format!("Copied {what}"), ToastLevel::Success);
        vec![Action::Copy(text)]
    }

    /// Whether a drag is under way, so the status line can say how to select natively instead.
    #[must_use]
    pub fn dragging(&self) -> bool {
        self.mouse_anchor.is_some() || self.screen_anchor.is_some()
    }

    /// Adds to the newest toast, for the copy path the terminal side chose.
    pub fn amend_last_toast(&mut self, suffix: &str) {
        if let Some(last) = self.toasts.back_mut() {
            last.text.push_str(suffix);
        }
    }

    /// The selected text as the conversation shows it, including any part scrolled out of view, with
    /// trailing spaces trimmed from each line.
    #[must_use]
    pub fn selected_text(&mut self) -> String {
        use unicode_segmentation::UnicodeSegmentation;
        use unicode_width::UnicodeWidthStr;
        let Some(selection) = self.selection else { return String::new() };
        let ((l0, c0), (l1, c1)) = selection.ordered();
        let width = self.view_size.0;
        let lines = self.transcript.lines_between(width, self.options, l0, l1 + 1);
        let mut out = Vec::new();
        for (offset, line) in lines.iter().enumerate() {
            // One entry per screen column, so columns line up with what was selected on screen.
            let mut cells: Vec<&str> = Vec::new();
            for span in &line.spans {
                for grapheme in span.content.graphemes(true) {
                    cells.push(grapheme);
                    // A wide character takes two columns; the second holds nothing.
                    cells.extend(std::iter::repeat_n("", grapheme.width().saturating_sub(1)));
                }
            }
            let index = l0 + offset;
            let from = if index == l0 { usize::from(c0) } else { 0 };
            let to = if index == l1 { usize::from(c1) + 1 } else { cells.len() };
            let to = to.min(cells.len());
            let text: String = cells.get(from.min(to)..to).map(<[&str]>::concat).unwrap_or_default();
            out.push(text.trim_end().to_owned());
        }
        out.join("\n").trim_matches('\n').to_owned()
    }

    fn activity_slot(&self) -> u64 {
        u64::try_from(self.now.saturating_duration_since(self.created).as_millis() / ACTIVITY_SLOT_MS).unwrap_or(0)
    }

    /// Records model output for the live graph.
    fn record_activity(&mut self, characters: usize) {
        let slot = self.activity_slot();
        let amount = u32::try_from(characters).unwrap_or(u32::MAX);
        match self.activity.back_mut() {
            Some((last, count)) if *last == slot => *count = count.saturating_add(amount),
            _ => self.activity.push_back((slot, amount)),
        }
        if self.activity.len() > ACTIVITY_SLOTS {
            self.activity.pop_front();
        }
    }

    /// The most recent `slots` time slots, oldest first. Silent periods are real zeros.
    #[must_use]
    pub fn activity_window(&self, slots: usize) -> Vec<u32> {
        let now = self.activity_slot();
        let mut out = vec![0; slots];
        for (slot, count) in &self.activity {
            let age = usize::try_from(now.saturating_sub(*slot)).unwrap_or(usize::MAX);
            if age < slots {
                out[slots - 1 - age] = *count;
            }
        }
        out
    }

    /// Opens the welcome screen.
    /// Empties the conversation view, for a new conversation or before showing a saved one.
    pub fn clear_conversation(&mut self) {
        self.queue.clear();
        self.transcript = Transcript::default();
        self.context = None;
        self.files.clear();
        self.file_changes.clear();
        self.turns_started = 0;
        self.tools_run = 0;
        self.state = AgentState::Idle;
    }

    /// Switches theme, re-rendering what's shown in the new colours. An unknown name changes nothing.
    pub fn use_theme(&mut self, selection: ThemeSelection) {
        if selection == self.theme || crate::theme::resolve(&selection).is_none() {
            return;
        }
        self.theme = selection;
        crate::theme::activate(&self.theme);
        self.transcript.invalidate();
    }

    /// The theme saved last time, applied unless `config.toml` chooses one.
    pub fn saved_theme(&mut self, selection: ThemeSelection) {
        if self.theme_locked {
            return;
        }
        if let Some(name) = crate::theme::find_theme(&selection.name) {
            self.use_theme(ThemeSelection { name, ..selection });
        }
    }

    /// Takes the latest list of saved sessions: the Home tab shows it, and the picker opens if it was asked for.
    pub fn sessions_listed(&mut self, sessions: Vec<crate::link::SessionSummary>) {
        self.recent_sessions.clone_from(&sessions);
        if std::mem::take(&mut self.sessions_wanted) {
            self.show_sessions(sessions);
        }
    }

    /// Shows the session picker, or says there's nothing to pick.
    pub fn show_sessions(&mut self, sessions: Vec<crate::link::SessionSummary>) {
        if sessions.is_empty() {
            self.toast("There are no saved sessions in this folder yet.", ToastLevel::Info);
        } else {
            self.overlay = Some(Overlay::Sessions { sessions, selected: 0 });
        }
    }

    /// Live hardware figures for the welcome screen's meters.
    pub fn set_hardware(&mut self, meters: Vec<crate::welcome::Meter>) {
        if let Some(info) = &mut self.welcome_info {
            info.hardware = meters;
        }
    }

    /// A saved session was taken up. With its model still available the conversation opens;
    /// otherwise the model list does, with the conversation waiting behind it.
    pub fn resumed(&mut self, model: Option<String>) {
        self.clear_conversation();
        match model {
            Some(model) => {
                if let Some(info) = &self.welcome_info {
                    self.provider = provider_of(info, &model);
                    self.model_vision = info.models.iter().any(|m| m.name == model && m.vision);
                }
                self.model = model;
                self.screen = Screen::Conversation;
            }
            None => {
                self.open_welcome();
                self.home_message =
                    Some(("The last session's model isn't available. Pick one to carry on.".to_owned(), true));
            }
        }
    }

    pub fn open_welcome(&mut self) {
        let mut welcome = WelcomeState::new(self.now);
        // Returning to a project with saved sessions lands on Home; a new one goes straight to the models.
        if !self.recent_sessions.is_empty() {
            welcome.tab = HomeTab::Home;
        }
        self.screen = Screen::Welcome(welcome);
        self.select_current_model();
    }

    /// Accepts fresh information about models and runtimes.
    pub fn set_welcome_info(&mut self, info: WelcomeInfo) {
        self.welcome_info = Some(info);
        self.select_current_model();
    }

    fn select_current_model(&mut self) {
        let wanted = self.welcome_info.as_ref().and_then(|info| {
            let current = (!self.model.is_empty()).then_some(self.model.as_str());
            let chosen = current.or(info.last_model.as_deref())?;
            info.models.iter().position(|model| model.name == chosen)
        });
        if let Screen::Welcome(welcome) = &mut self.screen {
            welcome.selected = wanted.unwrap_or(0);
            welcome.filter.clear();
        }
    }

    #[must_use]
    pub fn state_elapsed(&self) -> Duration {
        self.now.saturating_duration_since(self.state_since)
    }

    /// The approval waiting for an answer, if any.
    #[must_use]
    pub fn pending_approval(&self) -> Option<(&ToolEntry, ApprovalId)> {
        self.transcript.iter().rev().find_map(|entry| match entry {
            Entry::Tool(tool) => match &tool.status {
                ToolStatus::AwaitingApproval { approval, .. } => Some((tool, *approval)),
                _ => None,
            },
            _ => None,
        })
    }

    fn set_state(&mut self, state: AgentState) {
        if self.state != state {
            self.state = state;
            self.state_since = self.now;
        }
    }

    /// Applies an event. All text is sanitised here, the one place it enters the interface.
    pub fn apply(&mut self, kind: EventKind) {
        match kind {
            EventKind::TurnStarted { text, .. } => {
                if self.queue.front().is_some_and(|(queued, _)| text.ends_with(queued.as_str())) {
                    self.queue.pop_front();
                }
                self.turns_started += 1;
                self.turn_started = Some(self.now);
                self.turn_tools = 0;
                self.turn_tokens = (None, None, None);
                let text = sanitize(&text);
                if self.title.is_empty() {
                    self.title = text.lines().next().unwrap_or_default().to_owned();
                }
                self.transcript.push(Entry::User(text));
                self.transcript.follow();
            }
            EventKind::StateChanged { state } => self.set_state(state),
            EventKind::TextDelta { text } => {
                self.record_activity(text.chars().count());
                self.append_text(&sanitize(&text), false);
            }
            EventKind::ReasoningDelta { text } => {
                self.record_activity(text.chars().count());
                self.append_text(&sanitize(&text), true);
            }
            EventKind::ToolCallRequested { call } => self.tool_requested(&call),
            EventKind::ToolOutput { call_id, text } => {
                let text = sanitize(&text);
                self.update_tool(&call_id, |tool| tool.output.push_str(&text));
            }
            EventKind::ToolFinished { call_id, success, summary } => self.tool_finished(&call_id, success, &summary),
            EventKind::ApprovalRequired { approval, call_id, description, diff, always } => {
                let description = sanitize(&description);
                let diff = diff.map(|diff| sanitize(&diff));
                let always = always.map(|always| sanitize(&always));
                self.update_tool(&call_id, |tool| {
                    tool.diff.clone_from(&diff);
                    tool.status = ToolStatus::AwaitingApproval { approval, description, diff, always };
                });
            }
            EventKind::ApprovalResolved { approval, decision } => self.approval_resolved(approval, decision),
            EventKind::UsageReported { usage } => {
                self.last_usage = Some(usage.clone());
                // Summed over the turn: a turn with tool calls makes several requests.
                let add = |total: &mut Option<u64>, more: Option<u64>| {
                    if let Some(more) = more {
                        *total = Some(total.unwrap_or(0) + more);
                    }
                };
                add(&mut self.turn_tokens.0, usage.prompt_tokens);
                add(&mut self.turn_tokens.1, usage.cached_prompt_tokens);
                add(&mut self.turn_tokens.2, usage.output_tokens);
                if let (Some(tokens), Some(millis)) = (usage.output_tokens, usage.output_ms)
                    && millis > 0
                {
                    #[allow(clippy::cast_precision_loss)]
                    let speed = tokens as f64 * 1000.0 / millis as f64;
                    self.tokens_per_second = Some(speed);
                }
            }
            EventKind::ContextMeasured { used, limit } => self.context = Some((used, limit)),
            EventKind::ContextLedger { ledger } => self.ledger = Some(ledger),
            EventKind::PermissionModeChanged { mode } => self.permission_mode = mode,
            EventKind::HookRan { name, event, ok, summary } => {
                let mark = if ok { "⚙" } else { "⚠" };
                self.transcript.push(Entry::Notice(sanitize(&format!("{mark} hook {name} · {event} · {summary}"))));
            }
            EventKind::Rewound { to_turn, files, summary } => {
                self.transcript.truncate_to_turn(to_turn);
                self.context = None;
                let mut text = format!("Went back to before request {}.", to_turn + 1);
                if !files.is_empty() {
                    text.push_str(&format!(
                        " Put back {} file{}: {}.",
                        files.len(),
                        if files.len() == 1 { "" } else { "s" },
                        files.join(", ")
                    ));
                }
                if summary.is_some() {
                    text.push_str(" A summary of what was removed stays in the conversation.");
                }
                self.transcript.push(Entry::Notice(text));
                if let Some(summary) = summary {
                    self.transcript.push(Entry::Assistant(sanitize(&summary)));
                }
            }
            EventKind::QueuedDelivered { text } => {
                let text = sanitize(&text);
                let shown = text.rsplit("]\n\n").next().unwrap_or(&text).to_owned();
                if let Some(at) = self.queue.iter().position(|(queued, _)| text.ends_with(queued.as_str())) {
                    self.queue.remove(at);
                }
                self.transcript.push(Entry::User(shown));
            }
            EventKind::TurnFinished { outcome, .. } => self.turn_finished(outcome),
            EventKind::Undone { summary, paths } => {
                self.undone(&paths);
                self.transcript.push(Entry::Notice(sanitize(&summary)));
            }
            EventKind::TodoUpdated { items } => {
                self.todos = items
                    .into_iter()
                    .map(|item| recode_core::protocol::TodoItem { text: sanitize(&item.text), status: item.status })
                    .collect();
            }
            EventKind::Compacted { before_tokens, after_tokens, elided, summarised, problem, focus } => {
                self.transcript.push(Entry::Notice(compaction_notice(
                    before_tokens,
                    after_tokens,
                    elided,
                    summarised,
                    problem,
                    focus.as_deref(),
                )));
            }
        }
    }

    fn append_text(&mut self, text: &str, reasoning: bool) {
        let same_kind = matches!(
            (self.transcript.last(), reasoning),
            (Some(Entry::Reasoning(_)), true) | (Some(Entry::Assistant(_)), false)
        );
        if !same_kind {
            let entry = if reasoning { Entry::Reasoning(String::new()) } else { Entry::Assistant(String::new()) };
            self.transcript.push(entry);
        }
        self.transcript.with_last(|entry| {
            if let Entry::Reasoning(existing) | Entry::Assistant(existing) = entry {
                existing.push_str(text);
            }
        });
    }

    fn tool_requested(&mut self, call: &ToolCall) {
        let field = |name: &str| call.arguments.get(name).and_then(serde_json::Value::as_str);
        let target = field("path")
            .or_else(|| field("pattern"))
            .or_else(|| field("command"))
            .or_else(|| field("name"))
            .unwrap_or_default();
        self.tools_run += 1;
        self.turn_tools += 1;
        self.transcript.push(Entry::Tool(ToolEntry {
            id: call.id.clone(),
            name: sanitize(&call.name),
            target: sanitize(target).replace('\n', " "),
            status: ToolStatus::Running,
            output: String::new(),
            diff: None,
            elapsed: None,
        }));
        self.tool_started.insert(call.id.clone(), self.now);
    }

    fn update_tool(&mut self, id: &str, change: impl FnOnce(&mut ToolEntry)) {
        let mut change = Some(change);
        self.transcript.update_all(|entry| match entry {
            Entry::Tool(tool) if tool.id == id => {
                if let Some(change) = change.take() {
                    change(tool);
                }
                true
            }
            _ => false,
        });
    }

    fn tool_finished(&mut self, id: &str, success: bool, summary: &str) {
        let summary = sanitize(summary);
        let elapsed = self.tool_started.remove(id).map(|started| self.now.saturating_duration_since(started));
        let mut changed = None;
        self.update_tool(id, |tool| {
            tool.elapsed = elapsed;
            if tool.status == ToolStatus::Denied {
                return;
            }
            if success
                && matches!(tool.name.as_str(), "edit" | "write" | "delete")
                && let Some(diff) = &tool.diff
            {
                let count = |prefix: char, header: &str| {
                    diff.lines().filter(|line| line.starts_with(prefix) && !line.starts_with(header)).count()
                };
                changed = Some(ChangedFile {
                    path: tool.target.clone(),
                    added: count('+', "+++"),
                    removed: count('-', "---"),
                });
            }
            tool.status = if success { ToolStatus::Succeeded { summary } } else { ToolStatus::Failed { summary } };
        });
        if let Some(file) = changed {
            self.file_changes.push((self.turns_started, file));
            self.total_files();
        }
    }

    fn total_files(&mut self) {
        let mut files: Vec<ChangedFile> = Vec::new();
        for (_, change) in &self.file_changes {
            match files.iter_mut().find(|existing| existing.path == change.path) {
                Some(existing) => {
                    existing.added += change.added;
                    existing.removed += change.removed;
                }
                None => files.push(change.clone()),
            }
        }
        self.files = files;
    }

    /// Takes back the undone turn's changes to these files: for each, the most recent turn that changed it.
    fn undone(&mut self, paths: &[String]) {
        for path in paths {
            let latest = self.file_changes.iter().filter(|(_, c)| &c.path == path).map(|(turn, _)| *turn).max();
            if let Some(turn) = latest {
                self.file_changes.retain(|(t, c)| !(*t == turn && &c.path == path));
            }
        }
        self.total_files();
    }

    fn approval_resolved(&mut self, resolved: ApprovalId, decision: ApprovalDecision) {
        self.transcript.update_all(|entry| match entry {
            Entry::Tool(tool) => match &tool.status {
                ToolStatus::AwaitingApproval { approval, .. } if *approval == resolved => {
                    tool.status =
                        if decision == ApprovalDecision::Deny { ToolStatus::Denied } else { ToolStatus::Running };
                    true
                }
                _ => false,
            },
            _ => false,
        });
    }

    fn turn_finished(&mut self, outcome: TurnOutcome) {
        // Whatever hadn't finished by now never will, and must not keep claiming that it's running.
        self.transcript.update_all(|entry| match entry {
            Entry::Tool(tool) if matches!(tool.status, ToolStatus::Running | ToolStatus::AwaitingApproval { .. }) => {
                tool.status = ToolStatus::Interrupted;
                true
            }
            _ => false,
        });
        let (prompt_tokens, cached_tokens, output_tokens) = self.turn_tokens;
        self.transcript.push(Entry::TurnEnd(crate::transcript::TurnSummary {
            outcome,
            elapsed: self.turn_started.take().map(|started| self.now.saturating_duration_since(started)),
            tools: self.turn_tools,
            prompt_tokens,
            cached_tokens,
            output_tokens,
        }));
        self.set_state(AgentState::Idle);
    }

    /// Handles a key press, returning anything the caller needs to do.
    pub fn handle_key(&mut self, key: KeyEvent) -> Vec<Action> {
        if key.kind == KeyEventKind::Release {
            return Vec::new();
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        // F2 hands the mouse to the terminal (and back) from any screen or window.
        if key.code == KeyCode::F(2) {
            return self.run_command(PaletteCommand::ToggleSelectMode);
        }
        // With text selected, Ctrl+C copies it (as terminals do) and Esc lets it go.
        if self.selection.is_some() || self.screen_selection.is_some() {
            if ctrl && key.code == KeyCode::Char('c') {
                let copied =
                    if self.selection.is_some() { self.copy_selection() } else { self.copy_screen_selection() };
                self.selection = None;
                self.screen_selection = None;
                return copied;
            }
            self.selection = None;
            self.screen_selection = None;
            if key.code == KeyCode::Esc {
                return Vec::new();
            }
        }
        if !(ctrl && key.code == KeyCode::Char('c')) {
            self.quit_armed = false;
        }
        if self.overlay.is_some() {
            return self.overlay_key(key);
        }
        if matches!(self.screen, Screen::Welcome(_)) {
            return self.welcome_key(key);
        }
        if self.input.is_empty()
            && !ctrl
            && let Some((tool, approval)) = self.pending_approval()
        {
            let (title, diff) = (format!("{} {}", tool.name, tool.target), tool.diff.clone());
            match key.code {
                KeyCode::Char('y') => return vec![Action::ResolveApproval(approval, ApprovalDecision::AllowOnce)],
                // Only when "always" is on offer: secrets and commands outside the sandbox are asked each time.
                KeyCode::Char('a') if tool.always().is_some() => {
                    return vec![Action::ResolveApproval(approval, ApprovalDecision::AllowForSession)];
                }
                KeyCode::Char('a') => return Vec::new(),
                // Remember it for this project, across sessions: offered exactly when "always" is.
                KeyCode::Char('p') if tool.always().is_some() => {
                    return vec![Action::ResolveApproval(approval, ApprovalDecision::AllowForProject)];
                }
                KeyCode::Char('p') => return Vec::new(),
                KeyCode::Char('n') => return vec![Action::ResolveApproval(approval, ApprovalDecision::Deny)],
                KeyCode::Char('d') => {
                    if let Some(diff) = diff {
                        self.overlay =
                            Some(Overlay::Pager { title, text: diff, is_diff: true, side_by_side: false, offset: 0 });
                    }
                    return Vec::new();
                }
                _ => {}
            }
        }
        if !ctrl && let Some(actions) = self.completion_key(key) {
            return actions;
        }
        // Ctrl+Enter while the model works: queue the draft and stop the turn, so it goes out now.
        if ctrl && key.code == KeyCode::Enter && self.is_busy() && !self.demo {
            if !self.input.text().trim().is_empty() {
                let images = std::mem::take(&mut self.pending_images);
                let text = self.input.take();
                if self.history.last() != Some(&text) {
                    self.history.push(text.clone());
                }
                self.history_position = None;
                self.queue.push_back((text, images));
            }
            return if self.queue.is_empty() { Vec::new() } else { vec![Action::Interrupt] };
        }
        if ctrl {
            return self.control_key(key.code);
        }
        self.plain_key(key)
    }

    /// The suggestions for what's being typed, when the popup should show.
    #[must_use]
    pub fn completion(&self) -> Option<crate::complete::Completion> {
        if self.screen != Screen::Conversation || self.overlay.is_some() || self.pending_approval().is_some() {
            return None;
        }
        if self.completion_dismissed.as_deref() == Some(self.input.text()) {
            return None;
        }
        crate::complete::complete(self.input.text(), self.input.cursor(), &self.project_files, &self.skill_commands())
    }

    /// Keys for the open completion popup. `None` lets the key through to the input.
    fn completion_key(&mut self, key: KeyEvent) -> Option<Vec<Action>> {
        let completion = self.completion()?;
        let count = completion.items.len();
        let selected = self.completion_selected.min(count - 1);
        match key.code {
            KeyCode::Up => self.completion_selected = (selected + count - 1) % count,
            KeyCode::Down => self.completion_selected = (selected + 1) % count,
            KeyCode::Esc => self.completion_dismissed = Some(self.input.text().to_owned()),
            KeyCode::Tab | KeyCode::Enter if !key.modifiers.intersects(KeyModifiers::ALT | KeyModifiers::SHIFT) => {
                let chosen = &completion.items[selected];
                self.input.replace_before_cursor(completion.start, &chosen.replacement);
                self.completion_selected = 0;
                // Enter on a command that needs nothing more runs it straight away.
                if key.code == KeyCode::Enter && chosen.runs {
                    let text = self.input.take();
                    return Some(self.run_slash(text.trim()));
                }
            }
            _ => return None,
        }
        Some(Vec::new())
    }

    /// Runs a `/command`, returning what it does. Unknown commands are explained, not sent to the model.
    fn run_slash(&mut self, text: &str) -> Vec<Action> {
        let mut words = text.trim_start_matches('/').splitn(2, char::is_whitespace);
        let name = words.next().unwrap_or_default();
        let argument = words.next().unwrap_or_default().trim().to_owned();
        let tab = |app: &mut App, tab: HomeTab| {
            if !app.demo && !app.is_busy() {
                app.open_welcome();
                if let Screen::Welcome(welcome) = &mut app.screen {
                    welcome.tab = tab;
                }
            }
            Vec::new()
        };
        match name {
            "help" => self.run_command(PaletteCommand::Help),
            "model" => self.run_command(PaletteCommand::SwitchModel),
            "mcp" => tab(self, HomeTab::Mcp),
            "tools" => tab(self, HomeTab::Tools),
            "skills" => tab(self, HomeTab::Skills),
            "plugins" => tab(self, HomeTab::Plugins),
            "web" | "search" => self.run_command(PaletteCommand::WebSearch),
            "import" => match argument.trim() {
                "opencode" => vec![Action::Extension(ExtensionCommand::ImportOpenCode)],
                "claude" | "claude-code" => vec![Action::Extension(ExtensionCommand::ImportClaudeMcp)],
                _ => {
                    self.toast(
                        "/import opencode brings OpenCode's servers and providers; /import claude its MCP servers.",
                        ToastLevel::Info,
                    );
                    Vec::new()
                }
            },
            "plugin" => {
                let mut words = argument.splitn(2, char::is_whitespace);
                let verb = words.next().unwrap_or_default();
                let rest = words.next().map(|rest| rest.trim().to_owned()).filter(|rest| !rest.is_empty());
                match verb {
                    "" => tab(self, HomeTab::Plugins),
                    "install" | "add" | "i" => self.open_plugin_install(rest),
                    "search" | "find" | "s" => self.open_plugin_search(rest),
                    "update" | "u" => self.update_plugin(rest.as_deref().unwrap_or_default()),
                    "browse" | "list" | "b" => self.browse_plugins(rest.as_deref().unwrap_or("hermes")),
                    "marketplace" | "market" | "m" => {
                        let mut words = rest.as_deref().unwrap_or_default().splitn(2, char::is_whitespace);
                        let action = words.next().unwrap_or_default();
                        let what = words.next().map(|w| w.trim().to_owned()).filter(|w| !w.is_empty());
                        match (action, what) {
                            ("add", Some(what)) => vec![Action::Extension(ExtensionCommand::AddMarketplace(what))],
                            ("add", None) | ("", None) => self.open_marketplace_add(),
                            ("remove" | "rm", Some(what)) => {
                                vec![Action::Extension(ExtensionCommand::RemoveMarketplace(what))]
                            }
                            (other, what) => {
                                // `/plugin marketplace owner/repo` adds it.
                                let text = what.map(|w| format!("{other} {w}")).unwrap_or_else(|| other.to_owned());
                                vec![Action::Extension(ExtensionCommand::AddMarketplace(text))]
                            }
                        }
                    }
                    other => self.open_plugin_install(Some(format!(
                        "{other}{}",
                        rest.map(|r| format!(" {r}")).unwrap_or_default()
                    ))),
                }
            }
            "providers" => tab(self, HomeTab::Providers),
            "undo" => self.run_command(PaletteCommand::Undo),
            "compact" => {
                let actions = self.run_command(PaletteCommand::Compact);
                if argument.is_empty() {
                    actions
                } else {
                    actions
                        .into_iter()
                        .map(|action| match action {
                            Action::Compact { .. } => Action::Compact { focus: Some(argument.clone()) },
                            other => other,
                        })
                        .collect()
                }
            }
            "context" if argument == "raw" => {
                if self.demo {
                    self.toast("The demo has no live request to show.", ToastLevel::Info);
                    return Vec::new();
                }
                vec![Action::ShowLastRequest]
            }
            "context" => self.run_command(PaletteCommand::Context),
            "autocompact" => {
                let Some(window) = self.context_window.clone() else {
                    self.toast("Pick a model first; the window is set per model.", ToastLevel::Warning);
                    return Vec::new();
                };
                match recode_core::context::parse_autocompact(&argument) {
                    Err(problem) => {
                        self.toast(problem, ToastLevel::Warning);
                        Vec::new()
                    }
                    Ok(trigger) => {
                        let mut setting = self.context_setting;
                        let mut capped = None;
                        match (trigger, window.window) {
                            // Without a known window, the size given becomes the window itself.
                            (Trigger::Tokens(tokens), None) => {
                                setting.size = Size::Tokens(tokens);
                                setting.trigger = Trigger::Auto;
                            }
                            (Trigger::Tokens(tokens), Some(size)) if tokens > size => {
                                setting.trigger = Trigger::Tokens(size);
                                capped = Some(size);
                            }
                            (trigger, _) => setting.trigger = trigger,
                        }
                        let actions = self.set_context_setting(setting);
                        if let Some(window) = &self.context_window {
                            let mut text = format!("{}: {}", self.model, window.describe(&setting));
                            if let Some(size) = capped {
                                text.push_str(&format!(
                                    " (capped at the model's {} window)",
                                    recode_core::context::tokens(size)
                                ));
                            }
                            self.toast(text, ToastLevel::Success);
                        }
                        actions
                    }
                }
            }
            "rewind" => self.open_rewind(),
            "tasks" => {
                if self.demo {
                    self.toast("The demo recording has no background commands.", ToastLevel::Info);
                    return Vec::new();
                }
                vec![Action::ListTasks]
            }
            "transcript" => self.open_transcript_mode(),
            "export" => {
                if self.demo {
                    self.toast("The demo recording isn't exported.", ToastLevel::Info);
                    return Vec::new();
                }
                let text =
                    crate::export::markdown(&self.transcript, &self.title, &self.model, self.options.show_reasoning);
                if argument == "clipboard" {
                    self.toast("Copied the conversation as Markdown.", ToastLevel::Success);
                    return vec![Action::Copy(text)];
                }
                let path = (!argument.is_empty()).then(|| argument.clone());
                vec![Action::Export { path, text }]
            }
            "permissions" => {
                if self.demo {
                    self.toast("That isn't available in the demo recording.", ToastLevel::Info);
                    return Vec::new();
                }
                vec![Action::ListPermissions]
            }
            "plan" => {
                if self.demo {
                    self.toast("That isn't available in the demo recording.", ToastLevel::Info);
                    return Vec::new();
                }
                vec![Action::ShowPlan]
            }
            "init" | "memory" => {
                if self.demo {
                    self.toast("That isn't available in the demo recording.", ToastLevel::Info);
                    return Vec::new();
                }
                vec![if name == "init" { Action::Init } else { Action::ListInstructions }]
            }
            "compactions" => {
                if self.demo {
                    self.toast("The demo recording has no compactions to show.", ToastLevel::Info);
                    return Vec::new();
                }
                vec![Action::ListCompactions]
            }
            "new" | "clear" => self.run_command(PaletteCommand::NewConversation),
            "continue" | "resume" => self.run_command(PaletteCommand::ContinueLast),
            "copy" => self.run_command(PaletteCommand::CopyCode),
            "reasoning" => self.run_command(PaletteCommand::ToggleReasoning),
            "theme" | "themes" => self.run_command(PaletteCommand::Theme),
            "free" => self.run_command(PaletteCommand::FreeModels),
            "trust" => self.run_command(PaletteCommand::TrustProject),
            "skill" => {
                let mut words = argument.splitn(2, char::is_whitespace);
                let verb = words.next().unwrap_or_default();
                let rest = words.next().map(|rest| rest.trim().to_owned()).filter(|rest| !rest.is_empty());
                match verb {
                    "" | "install" | "add" | "i" => self.open_install(rest),
                    "search" | "find" | "s" => self.open_skill_search(rest),
                    "update" | "u" => self.update_skill(rest.as_deref().unwrap_or_default()),
                    other => {
                        self.open_install(Some(format!("{other}{}", rest.map(|r| format!(" {r}")).unwrap_or_default())))
                    }
                }
            }
            "files" | "tree" => {
                self.panel_preference = Some(true);
                self.panel_tab = PanelTab::Files;
                Vec::new()
            }
            "diff" | "diffs" => {
                let text = self.session_diff();
                if text.trim().is_empty() {
                    self.toast("No files have been changed in this session.", ToastLevel::Info);
                } else {
                    let files = text.lines().filter(|line| line.starts_with("+++ ")).count();
                    let title = format!("Changes this session · {files} file{}", if files == 1 { "" } else { "s" });
                    self.overlay = Some(Overlay::Pager { title, text, is_diff: true, side_by_side: false, offset: 0 });
                }
                Vec::new()
            }
            "panel" => self.run_command(PaletteCommand::TogglePanel),
            "quit" | "exit" => self.run_command(PaletteCommand::Quit),
            "add-dir" if argument.is_empty() => {
                self.toast("Give a folder: /add-dir ~/notes", ToastLevel::Warning);
                Vec::new()
            }
            "add-dir" => vec![Action::AddFolder(argument)],
            other => {
                self.toast(
                    format!(
                        "There's no /{other} command. Type / to see them, or // to send a message starting with /."
                    ),
                    ToastLevel::Warning,
                );
                Vec::new()
            }
        }
    }

    /// Steps through earlier prompts. Returns false when there's nowhere to go.
    fn history_step(&mut self, back: bool) -> bool {
        if self.history.is_empty() {
            return false;
        }
        let position = match (&self.history_position, back) {
            (None, true) => {
                self.history_position = Some((self.history.len(), self.input.text().to_owned()));
                self.history.len() - 1
            }
            (None, false) => return false,
            (Some((index, _)), true) => index.saturating_sub(1),
            (Some((index, _)), false) => index + 1,
        };
        if position >= self.history.len() {
            // Past the newest: back to what was being typed.
            if let Some((_, draft)) = self.history_position.take() {
                self.input.set(&draft);
            }
            return true;
        }
        if let Some((index, _)) = &mut self.history_position {
            *index = position;
        }
        let text = self.history[position].clone();
        self.input.set(&text);
        true
    }

    fn welcome_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            if self.quit_armed {
                return vec![Action::Quit];
            }
            self.quit_armed = true;
            return Vec::new();
        }
        if ctrl && key.code == KeyCode::Char('r') {
            self.welcome_info = None;
            self.home_message = None;
            return vec![Action::Refresh];
        }
        if key.code == KeyCode::F(1) {
            self.overlay = Some(Overlay::Help);
            return Vec::new();
        }
        let tab_move = match key.code {
            KeyCode::Tab => Some(1),
            KeyCode::BackTab => Some(-1),
            _ => None,
        };
        let Screen::Welcome(welcome) = &mut self.screen else { return Vec::new() };
        if let Some(delta) = tab_move {
            welcome.tab = welcome.tab.moved(delta);
            self.home_message = None;
            return Vec::new();
        }
        match welcome.tab {
            HomeTab::Home => self.home_key(key, ctrl),
            HomeTab::Settings => self.settings_key(key, ctrl),
            HomeTab::Models => self.models_key(key, ctrl),
            HomeTab::Runtimes => self.runtimes_key(key, ctrl),
            _ => self.extensions_key(key, ctrl),
        }
    }

    fn home_key(&mut self, key: KeyEvent, ctrl: bool) -> Vec<Action> {
        if ctrl {
            return Vec::new();
        }
        let rows = crate::home::dashboard_rows(self);
        let has_session = !self.model.is_empty();
        let Screen::Welcome(welcome) = &mut self.screen else { return Vec::new() };
        match key.code {
            KeyCode::Up => welcome.move_extension(-1, rows.len()),
            KeyCode::Down => welcome.move_extension(1, rows.len()),
            KeyCode::Esc if has_session => self.screen = Screen::Conversation,
            KeyCode::Enter => {
                let chosen = rows.get(welcome.extension_index(rows.len())).cloned();
                return self.open_dash_item(chosen);
            }
            KeyCode::Char('d') => {
                if let Some(crate::home::DashItem::Resume(id, label)) =
                    rows.get(welcome.extension_index(rows.len())).cloned()
                {
                    self.confirm_delete_session(id, label);
                }
            }
            _ => {}
        }
        Vec::new()
    }

    /// Asks before a saved chat is deleted, from the Home list or the session picker.
    fn confirm_delete_session(&mut self, id: String, label: String) {
        if self.demo {
            self.toast("That isn't available in the demo recording.", ToastLevel::Info);
            return;
        }
        self.overlay = Some(Overlay::Confirm(ConfirmState {
            title: "Delete this chat?".to_owned(),
            body: vec![
                label,
                "Its messages, saved snapshot and compaction history are removed. If it's the conversation open now, a new one starts.".to_owned(),
            ],
            command: Confirmed::DeleteSession(id),
        }));
    }

    fn open_dash_item(&mut self, item: Option<crate::home::DashItem>) -> Vec<Action> {
        use crate::home::DashItem;
        match item {
            Some(DashItem::Resume(id, _)) => vec![Action::ContinueSession(id)],
            Some(DashItem::Model(name)) => {
                let Some(index) = self.welcome_info.as_ref().and_then(|i| i.models.iter().position(|m| m.name == name))
                else {
                    return Vec::new();
                };
                if let Screen::Welcome(welcome) = &mut self.screen {
                    welcome.selected = index;
                    welcome.tab = HomeTab::Models;
                }
                self.choose_selected_model()
            }
            Some(DashItem::GetFree) => {
                self.open_free_setup();
                Vec::new()
            }
            Some(DashItem::Theme) => self.run_command(PaletteCommand::Theme),
            Some(DashItem::WebSearch) => self.run_command(PaletteCommand::WebSearch),
            Some(DashItem::Settings) => {
                if let Screen::Welcome(welcome) = &mut self.screen {
                    welcome.tab = HomeTab::Settings;
                }
                Vec::new()
            }
            None => Vec::new(),
        }
    }

    fn settings_key(&mut self, key: KeyEvent, ctrl: bool) -> Vec<Action> {
        use crate::home::{CONTEXT_CHOICES, NOTIFICATION_CHOICES, SettingKind};
        if ctrl {
            return Vec::new();
        }
        let rows = crate::home::settings_rows(self);
        let has_session = !self.model.is_empty();
        let Screen::Welcome(welcome) = &mut self.screen else { return Vec::new() };
        let index = welcome.extension_index(rows.len());
        // `a` puts a context row back to automatic.
        let reset = key.code == KeyCode::Char('a');
        let step: i32 = match key.code {
            KeyCode::Up => {
                welcome.move_extension(-1, rows.len());
                return Vec::new();
            }
            KeyCode::Down => {
                welcome.move_extension(1, rows.len());
                return Vec::new();
            }
            KeyCode::Esc if has_session => {
                self.screen = Screen::Conversation;
                return Vec::new();
            }
            KeyCode::Left => -1,
            KeyCode::Right | KeyCode::Enter => 1,
            KeyCode::Char('d' | 'a') => 0,
            _ => return Vec::new(),
        };
        let Some(setting) = rows.get(index) else { return Vec::new() };
        let cycle = |choices: &[&str], current: &str, step: i32| -> String {
            let at = choices.iter().position(|c| *c == current).unwrap_or(0);
            let count = i32::try_from(choices.len()).unwrap_or(1);
            let next = (i32::try_from(at).unwrap_or(0) + step).rem_euclid(count);
            choices[usize::try_from(next).unwrap_or(0)].to_owned()
        };
        match &setting.kind {
            SettingKind::Theme if step != 0 => self.run_command(PaletteCommand::Theme),
            SettingKind::Variant | SettingKind::Accent | SettingKind::Background if self.theme_locked => {
                self.toast("The theme is set in config.toml, so it can't be changed here.", ToastLevel::Info);
                Vec::new()
            }
            SettingKind::Variant if step != 0 => {
                let variant = if step < 0 { self.theme.variant.previous() } else { self.theme.variant.next() };
                self.set_theme(ThemeSelection { variant, ..self.theme.clone() })
            }
            SettingKind::Accent if step != 0 => {
                let palette = crate::theme::resolve(&ThemeSelection::named(&self.theme.name));
                let choices: Vec<Color> =
                    palette.map(|p| vec![p.colors[2], p.colors[3], p.colors[4], p.colors[5]]).unwrap_or_default();
                let accent = match self.theme.accent {
                    None => choices.first().copied(),
                    Some(accent) => {
                        choices.iter().position(|c| *c == accent).and_then(|at| choices.get(at + 1).copied())
                    }
                };
                self.set_theme(ThemeSelection { accent, ..self.theme.clone() })
            }
            SettingKind::Background if step != 0 => {
                self.set_theme(ThemeSelection { solid: !self.theme.solid, ..self.theme.clone() })
            }
            SettingKind::ContextSize if (step != 0 || reset) && self.context_window.is_some() && has_session => {
                let window = self.context_window.clone().unwrap_or_default();
                let mut setting = self.context_setting;
                if reset {
                    setting.size = Size::Auto;
                } else {
                    let limit = window.model_limit.unwrap_or(u64::MAX);
                    let mut choices: Vec<u64> =
                        CONTEXT_CHOICES.iter().map(|c| u64::from(*c)).filter(|c| *c <= limit).collect();
                    if let Some(limit) = window.model_limit
                        && !choices.contains(&limit)
                    {
                        choices.push(limit);
                    }
                    let current = window.window.unwrap_or(16384);
                    let at = choices.iter().position(|c| *c >= current).unwrap_or(choices.len().saturating_sub(1));
                    let next = at.saturating_add_signed(isize::try_from(step).unwrap_or(0)).min(choices.len() - 1);
                    setting.size = Size::Tokens(choices[next]);
                }
                self.set_context_setting(setting)
            }
            SettingKind::ContextSize if step != 0 => {
                let current: u32 = self.settings.get("num_ctx").and_then(|v| v.parse().ok()).unwrap_or(16384);
                let at = CONTEXT_CHOICES.iter().position(|c| *c == current).unwrap_or(2);
                let next = at.saturating_add_signed(isize::try_from(step).unwrap_or(0)).min(CONTEXT_CHOICES.len() - 1);
                self.set_setting("num_ctx", CONTEXT_CHOICES[next].to_string())
            }
            SettingKind::CompactionTrigger if step != 0 || reset => {
                let window = self.context_window.clone().unwrap_or_default();
                let Some(size) = window.window else {
                    self.toast("Set a context size first; compaction is measured against it.", ToastLevel::Info);
                    return Vec::new();
                };
                let mut setting = self.context_setting;
                // auto, 50 % … 90 % of the window, off.
                let percents = [50u64, 60, 70, 80, 90];
                let choices: Vec<Trigger> = std::iter::once(Trigger::Auto)
                    .chain(percents.iter().map(|p| Trigger::Tokens(size * p / 100)))
                    .chain(std::iter::once(Trigger::Off))
                    .collect();
                let at = match setting.trigger {
                    Trigger::Auto => 0,
                    Trigger::Off => choices.len() - 1,
                    Trigger::Tokens(tokens) => choices
                        .iter()
                        .position(|c| matches!(c, Trigger::Tokens(t) if *t >= tokens))
                        .unwrap_or(choices.len() - 1),
                };
                let next = if reset {
                    0
                } else {
                    at.saturating_add_signed(isize::try_from(step).unwrap_or(0)).min(choices.len() - 1)
                };
                setting.trigger = choices[next];
                self.set_context_setting(setting)
            }
            SettingKind::Reserve if step != 0 || reset => {
                let mut setting = self.context_setting;
                let choices = [
                    Size::Auto,
                    Size::Tokens(1024),
                    Size::Tokens(2048),
                    Size::Tokens(4096),
                    Size::Tokens(8192),
                    Size::Tokens(16384),
                ];
                let at = choices.iter().position(|c| *c == setting.reserve).unwrap_or(0);
                let next = if reset {
                    0
                } else {
                    at.saturating_add_signed(isize::try_from(step).unwrap_or(0)).min(choices.len() - 1)
                };
                setting.reserve = choices[next];
                self.set_context_setting(setting)
            }
            SettingKind::IdleCompaction if step != 0 => {
                let current = self.settings.get("idle_compaction").cloned().unwrap_or_else(|| "off".to_owned());
                self.set_setting("idle_compaction", cycle(&crate::home::IDLE_CHOICES, &current, step))
            }
            SettingKind::Notifications if step != 0 => {
                let current = self.settings.get("notifications").cloned().unwrap_or_else(|| "both".to_owned());
                self.set_setting("notifications", cycle(&NOTIFICATION_CHOICES, &current, step))
            }
            SettingKind::Memory if step != 0 => {
                let current = self.settings.get("memory").cloned().unwrap_or_else(|| "on".to_owned());
                self.set_setting("memory", cycle(&crate::home::MEMORY_CHOICES, &current, step))
            }
            SettingKind::LineNumbers if step != 0 => {
                let on = self.settings.get("line_numbers").is_some_and(|v| v == "on");
                self.set_setting("line_numbers", if on { "off".to_owned() } else { "on".to_owned() })
            }
            SettingKind::ApiKey(name, stored) => {
                if step == 0 {
                    if !*stored {
                        return Vec::new();
                    }
                    let Some(provider) =
                        self.extensions.as_ref().and_then(|e| e.providers.iter().find(|p| &p.name == name).cloned())
                    else {
                        return Vec::new();
                    };
                    self.toast(format!("Removed the key for {name}."), ToastLevel::Info);
                    return vec![Action::Extension(ExtensionCommand::SaveProvider {
                        original: Some(name.clone()),
                        provider,
                        key: Some(String::new()),
                    })];
                }
                self.open_key_step(name);
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// Applies and remembers a theme change made outside the picker.
    fn set_theme(&mut self, selection: ThemeSelection) -> Vec<Action> {
        self.use_theme(selection.clone());
        vec![Action::SetTheme(selection)]
    }

    /// The driver's word on the background commands: opens or refreshes the list.
    pub fn tasks_listed(&mut self, tasks: Vec<crate::link::TaskInfo>) {
        let selected = match &self.overlay {
            Some(Overlay::Tasks { selected, .. }) => (*selected).min(tasks.len().saturating_sub(1)),
            _ => 0,
        };
        if matches!(self.overlay, Some(Overlay::Tasks { .. }) | None) || self.overlay.is_none() {
            self.overlay = Some(Overlay::Tasks { tasks, selected });
        }
    }

    /// Transcript mode: the whole conversation, searchable, from the bottom.
    fn open_transcript_mode(&mut self) -> Vec<Action> {
        if self.transcript.is_empty() {
            self.toast("Nothing in the conversation yet.", ToastLevel::Info);
            return Vec::new();
        }
        self.overlay = Some(Overlay::Transcript {
            offset: usize::MAX,
            typing: None,
            query: String::new(),
            matches: Vec::new(),
            current: 0,
        });
        Vec::new()
    }

    /// The rewind picker: every request so far, newest first.
    fn open_rewind(&mut self) -> Vec<Action> {
        if self.demo {
            self.toast("Rewinding isn't available in the demo recording.", ToastLevel::Info);
            return Vec::new();
        }
        if self.is_busy() {
            self.toast("Rewinding is available once this turn finishes (Esc stops it).", ToastLevel::Warning);
            return Vec::new();
        }
        let mut turns = self.transcript.requests();
        if turns.is_empty() {
            self.toast("Nothing to go back to yet.", ToastLevel::Info);
            return Vec::new();
        }
        turns.reverse();
        self.overlay = Some(Overlay::Rewind { turns, selected: 0, choice: 0 });
        Vec::new()
    }

    /// Prompts matching the search, best first (newest first among equals).
    pub fn history_matches(&self, query: &str, all_projects: bool) -> Vec<String> {
        let source: &[String] = if all_projects { self.history_all.as_deref().unwrap_or(&[]) } else { &self.history };
        let mut scored: Vec<(i64, usize, &String)> = source
            .iter()
            .enumerate()
            .filter_map(|(index, prompt)| crate::complete::score(query, prompt).map(|score| (score, index, prompt)))
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
        let mut seen = std::collections::BTreeSet::new();
        scored
            .into_iter()
            .filter(|(_, _, prompt)| seen.insert((*prompt).clone()))
            .map(|(_, _, prompt)| prompt.clone())
            .take(50)
            .collect()
    }

    /// The driver's answer to a request for every project's prompts.
    pub fn history_all_loaded(&mut self, all: Vec<String>) {
        self.history_all = Some(all);
    }

    /// Shift+Tab: the next permission mode, applied at once and confirmed by the driver.
    fn cycle_permission_mode(&mut self) -> Vec<Action> {
        if self.demo {
            self.toast("Permission modes aren't available in the demo recording.", ToastLevel::Info);
            return Vec::new();
        }
        let mode = self.permission_mode.next();
        self.permission_mode = mode;
        self.toast(format!("{}: {}", mode.label(), mode.describe()), ToastLevel::Info);
        vec![Action::SetPermissionMode(mode)]
    }

    /// The driver's answer to `/permissions`.
    pub fn permissions_listed(&mut self, mode: PermissionMode, session: Vec<String>, project: Vec<(String, String)>) {
        self.permission_mode = mode;
        self.overlay = Some(Overlay::Permissions { mode, session, project, selected: 0 });
    }

    /// What `/init` would write: shown for confirmation, the first lines in full.
    pub fn init_preview(&mut self, path: String, text: String) {
        let lines: Vec<&str> = text.lines().collect();
        let mut body = vec![format!("{path} doesn't exist yet. /init would write this, from what the folder shows:")];
        body.push(String::new());
        for line in lines.iter().take(14) {
            body.push(format!("  {line}"));
        }
        if lines.len() > 14 {
            body.push(format!("  … {} more lines", lines.len() - 14));
        }
        body.push(String::new());
        body.push("You can edit it afterwards with /memory. Write it?".to_owned());
        let name = std::path::Path::new(&path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or(path);
        self.overlay = Some(Overlay::Confirm(ConfirmState {
            title: format!("Write {name}?"),
            body,
            command: Confirmed::WriteInit,
        }));
    }

    /// The driver's answer to `/memory`.
    pub fn instructions_listed(
        &mut self,
        files: Vec<crate::link::InstructionFileInfo>,
        user_path: String,
        project_path: Option<String>,
    ) {
        self.overlay = Some(Overlay::Memory { files, user_path, project_path, selected: 0 });
    }

    /// The driver's answer to `/compactions`: shown newest first.
    pub fn compactions_listed(&mut self, mut reports: Vec<(String, recode_core::usage::CompactionReport)>) {
        reports.reverse();
        self.overlay = Some(Overlay::Compactions { reports, selected: 0 });
    }

    /// Opens long text from the driver in the pager.
    pub fn open_pager(&mut self, title: String, text: String) {
        self.overlay =
            Some(Overlay::Pager { title, text: sanitize(&text), is_diff: false, side_by_side: false, offset: 0 });
    }

    /// The driver's word on the active model's context window.
    pub fn set_context_window(
        &mut self,
        window: ContextWindow,
        setting: WindowSetting,
        kv_bytes_per_token: Option<u64>,
    ) {
        self.context_window = Some(window);
        self.context_setting = setting;
        self.kv_bytes_per_token = kv_bytes_per_token;
    }

    /// Applies a context setting for the active model at once (the driver confirms with its own figures).
    fn set_context_setting(&mut self, setting: WindowSetting) -> Vec<Action> {
        if let Some(window) = &self.context_window {
            let default: u32 = self.settings.get("num_ctx").and_then(|v| v.parse().ok()).unwrap_or(16384);
            let is_local = window.local_num_ctx.is_some();
            self.context_window = Some(ContextWindow::resolve(&setting, window.model_limit, is_local, default));
        }
        self.context_setting = setting;
        vec![Action::SetContextWindow { model: self.model.clone(), setting }]
    }

    /// Applies and remembers a setting.
    fn set_setting(&mut self, key: &str, value: String) -> Vec<Action> {
        self.settings.insert(key.to_owned(), value.clone());
        vec![Action::SetSetting { key: key.to_owned(), value }]
    }

    fn models_key(&mut self, key: KeyEvent, ctrl: bool) -> Vec<Action> {
        let page = self.model_list_rows.max(1);
        let has_session = !self.model.is_empty();
        let Screen::Welcome(welcome) = &mut self.screen else { return Vec::new() };
        let matches = self.welcome_info.as_ref().map_or_else(Vec::new, |info| welcome.matches(info));
        match key.code {
            KeyCode::Up => welcome.move_selection(&matches, -1),
            KeyCode::Down => welcome.move_selection(&matches, 1),
            KeyCode::PageUp => welcome.move_selection(&matches, -isize::try_from(page).unwrap_or(1)),
            KeyCode::PageDown => welcome.move_selection(&matches, isize::try_from(page).unwrap_or(1)),
            KeyCode::Esc if !welcome.filter.is_empty() => welcome.set_filter(String::new(), self.welcome_info.as_ref()),
            KeyCode::Esc if has_session => self.screen = Screen::Conversation,
            KeyCode::Backspace => {
                let mut filter = welcome.filter.clone();
                filter.pop();
                welcome.set_filter(filter, self.welcome_info.as_ref());
            }
            // Narrowing the list: free only, tool-capable only, vision only.
            KeyCode::Char('f') if ctrl => welcome.toggle(|only| &mut only.free, self.welcome_info.as_ref()),
            KeyCode::Char('t') if ctrl => welcome.toggle(|only| &mut only.tools, self.welcome_info.as_ref()),
            KeyCode::Char('v') if ctrl => welcome.toggle(|only| &mut only.vision, self.welcome_info.as_ref()),
            KeyCode::Char('p') if ctrl => welcome.by_provider = !welcome.by_provider,
            KeyCode::Char('k') if !ctrl && welcome.filter.is_empty() => return self.key_for_selected(),
            KeyCode::Char(c) if !ctrl => {
                let filter = format!("{}{c}", welcome.filter);
                welcome.set_filter(filter, self.welcome_info.as_ref());
            }
            // A model whose provider has no key can't start: Enter goes to the key instead.
            KeyCode::Enter if self.selected_model().is_some_and(|model| model.needs_key) => {
                return self.key_for_selected();
            }
            KeyCode::Enter => return self.choose_selected_model(),
            _ => {}
        }
        Vec::new()
    }

    /// The highlighted model on the welcome screen.
    fn selected_model(&self) -> Option<&WelcomeModel> {
        let Screen::Welcome(welcome) = &self.screen else { return None };
        let info = self.welcome_info.as_ref()?;
        let matches = welcome.matches(info);
        let chosen = matches.contains(&welcome.selected).then_some(welcome.selected).or(matches.first().copied())?;
        info.models.get(chosen)
    }

    /// Opens the key step for the highlighted model's provider.
    fn key_for_selected(&mut self) -> Vec<Action> {
        let Some(provider) = self.selected_model().and_then(|model| model.provider_label.clone()) else {
            self.toast("That model runs here, so it needs no key.", ToastLevel::Info);
            return Vec::new();
        };
        self.open_key_step(&provider);
        Vec::new()
    }

    /// Opens the free-models setup at the key step for a provider, preset or configured.
    pub fn open_key_step(&mut self, provider: &str) {
        let configured = self
            .extensions
            .as_ref()
            .and_then(|extensions| extensions.providers.iter().find(|p| p.name == provider).cloned());
        let step = match (ProviderPreset::named(provider), configured) {
            (Some(preset), configured) => FreeStep::key_for_preset(preset, configured.map(|p| p.name)),
            (None, Some(configured)) => FreeStep::Key {
                name: configured.name.clone(),
                kind: configured.kind,
                base_url: configured.base_url.clone(),
                signup_url: None,
                original: Some(configured.name),
                key: String::new(),
            },
            (None, None) => {
                self.toast(format!("{provider} isn't a known provider."), ToastLevel::Warning);
                return;
            }
        };
        self.overlay = Some(Overlay::FreeSetup(step));
    }

    /// Opens the free-models setup at the provider choice.
    pub fn open_free_setup(&mut self) {
        self.overlay = Some(Overlay::FreeSetup(FreeStep::Pick { selected: 0 }));
    }

    fn free_setup_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let Some(Overlay::FreeSetup(step)) = &mut self.overlay else { return Vec::new() };
        match step {
            FreeStep::Pick { selected } => {
                let presets = FreeStep::free_presets();
                match key.code {
                    KeyCode::Esc => self.overlay = None,
                    KeyCode::Up => *selected = selected.saturating_sub(1),
                    KeyCode::Down => *selected = (*selected + 1).min(presets.len().saturating_sub(1)),
                    KeyCode::Enter => {
                        let Some(preset) = presets.get(*selected).copied() else { return Vec::new() };
                        let name = preset.name.to_owned();
                        let url = preset.signup_url.to_owned();
                        self.open_key_step(&name);
                        return vec![Action::OpenUrl(url)];
                    }
                    _ => {}
                }
            }
            FreeStep::Key { name, kind, base_url, signup_url, original, key: typed } => match key.code {
                KeyCode::Esc => self.overlay = Some(Overlay::FreeSetup(FreeStep::Pick { selected: 0 })),
                KeyCode::Char('o') if ctrl => {
                    if let Some(url) = signup_url.clone() {
                        return vec![Action::OpenUrl(url)];
                    }
                }
                KeyCode::Backspace => {
                    typed.pop();
                }
                KeyCode::Char(c) if !ctrl && !c.is_whitespace() => typed.push(c),
                KeyCode::Enter if !typed.trim().is_empty() => {
                    let provider =
                        CloudProvider { name: name.clone(), kind: *kind, base_url: base_url.clone(), enabled: true };
                    let command = ExtensionCommand::SaveProvider {
                        original: original.clone(),
                        provider,
                        key: Some(typed.trim().to_owned()),
                    };
                    let name = name.clone();
                    self.overlay = None;
                    self.toast(format!("Checking {name}…"), ToastLevel::Info);
                    return vec![Action::Extension(command)];
                }
                _ => {}
            },
        }
        Vec::new()
    }

    fn choose_selected_model(&mut self) -> Vec<Action> {
        let Screen::Welcome(welcome) = &self.screen else { return Vec::new() };
        let Some(info) = &self.welcome_info else { return Vec::new() };
        let matches = welcome.matches(info);
        let chosen = matches.contains(&welcome.selected).then_some(welcome.selected).or(matches.first().copied());
        let Some(name) = chosen.and_then(|index| info.models.get(index)).map(|model| model.name.clone()) else {
            return Vec::new();
        };
        self.provider = provider_of(info, &name);
        if self.model != name {
            self.tokens_per_second = None;
        }
        self.model_vision = chosen.and_then(|index| info.models.get(index)).is_some_and(|model| model.vision);
        // A fresh model can't see images queued for the previous one only if it lacks vision.
        if !self.model_vision {
            self.pending_images.clear();
        }
        self.model.clone_from(&name);
        self.screen = Screen::Conversation;
        vec![Action::ChooseModel(name)]
    }

    fn extensions_key(&mut self, key: KeyEvent, ctrl: bool) -> Vec<Action> {
        if ctrl {
            return Vec::new();
        }
        let has_session = !self.model.is_empty();
        let count = self.extension_count();
        let Screen::Welcome(welcome) = &mut self.screen else { return Vec::new() };
        let tab = welcome.tab;
        match key.code {
            KeyCode::Up => welcome.move_extension(-1, count),
            KeyCode::Down => welcome.move_extension(1, count),
            KeyCode::PageUp => welcome.move_extension(-5, count),
            KeyCode::PageDown => welcome.move_extension(5, count),
            KeyCode::Esc => {
                if has_session {
                    self.screen = Screen::Conversation;
                } else {
                    welcome.tab = HomeTab::Models;
                }
            }
            KeyCode::Char('a') => self.overlay = Some(Overlay::Form(FormState::new_for(tab))),
            KeyCode::Char('e') | KeyCode::Enter => return self.edit_selected(),
            KeyCode::Char('d') => self.confirm_delete(),
            KeyCode::Char(' ') => return self.toggle_selected(),
            KeyCode::Char('t') if tab == HomeTab::Mcp || tab == HomeTab::Providers => return self.test_selected(),
            KeyCode::Char('i') if tab == HomeTab::Mcp => {
                return vec![Action::Extension(ExtensionCommand::ImportClaudeMcp)];
            }
            KeyCode::Char('o') if tab == HomeTab::Mcp || tab == HomeTab::Providers => {
                return vec![Action::Extension(ExtensionCommand::ImportOpenCode)];
            }
            KeyCode::Char('i') if tab == HomeTab::Skills => return self.open_install(None),
            KeyCode::Char('s') if tab == HomeTab::Skills => return self.open_skill_search(None),
            KeyCode::Char('u') if tab == HomeTab::Skills => return self.update_selected_skill(),
            KeyCode::Char('i') if tab == HomeTab::Plugins => return self.open_plugin_install(None),
            KeyCode::Char('m') if tab == HomeTab::Plugins => return self.open_marketplace_add(),
            KeyCode::Char('b') if tab == HomeTab::Plugins => return self.browse_selected_marketplace(),
            KeyCode::Char('s') if tab == HomeTab::Plugins => return self.open_plugin_search(None),
            KeyCode::Char('u') if tab == HomeTab::Plugins => return self.update_selected_plugin(),
            KeyCode::Char('x') if tab == HomeTab::Mcp => return self.toggle_exposed(),
            KeyCode::Char('p') if tab == HomeTab::Mcp => self.overlay = Some(Overlay::McpPresets { selected: 0 }),
            KeyCode::Char('s') if tab == HomeTab::Mcp => {
                self.overlay = Some(Overlay::McpSearch {
                    query: String::new(),
                    hits: Vec::new(),
                    selected: 0,
                    searching: false,
                    error: None,
                });
            }
            KeyCode::Char('f') if tab == HomeTab::Mcp => self.open_tool_filter(),
            KeyCode::Char('p') if tab == HomeTab::Providers => {
                self.overlay = Some(Overlay::ProviderPresets { selected: 0 })
            }
            _ => {}
        }
        Vec::new()
    }

    fn extension_count(&self) -> usize {
        let Screen::Welcome(welcome) = &self.screen else { return 0 };
        if welcome.tab == HomeTab::Runtimes {
            return self.runtimes.as_ref().map_or(0, |info| crate::runtimes_view::rows(info).len());
        }
        let Some(extensions) = &self.extensions else { return 0 };
        match welcome.tab {
            HomeTab::Models | HomeTab::Runtimes => 0,
            HomeTab::Home => crate::home::dashboard_rows(self).len(),
            HomeTab::Settings => crate::home::settings_rows(self).len(),
            HomeTab::Providers => extensions.providers.len(),
            HomeTab::Mcp => extensions.servers.len(),
            HomeTab::Skills => extensions.skills.len(),
            HomeTab::Plugins => crate::extensions_view::plugin_rows(extensions).len(),
            HomeTab::Tools => extensions.tools.len(),
        }
    }

    fn selected_extension(&self) -> Option<(HomeTab, usize, &ExtensionsSnapshot)> {
        let Screen::Welcome(welcome) = &self.screen else { return None };
        let extensions = self.extensions.as_ref()?;
        let index = welcome.extension_index(self.extension_count());
        Some((welcome.tab, index, extensions))
    }

    /// `u` on the Skills tab: check the selected installed skill for a newer version.
    fn update_selected_skill(&mut self) -> Vec<Action> {
        let Some((tab, index, extensions)) = self.selected_extension() else { return Vec::new() };
        if tab != HomeTab::Skills {
            return Vec::new();
        }
        match extensions.skills.get(index) {
            Some(skill) if skill.installed_from.is_some() => {
                let name = skill.name.clone();
                self.update_skill(&name)
            }
            Some(skill) => {
                let text = format!("{} wasn't installed from a hub, so there's nothing to update it from.", skill.name);
                self.home_message = Some((text, true));
                Vec::new()
            }
            None => Vec::new(),
        }
    }

    fn edit_selected(&mut self) -> Vec<Action> {
        let Some((tab, index, extensions)) = self.selected_extension() else { return Vec::new() };
        match tab {
            HomeTab::Mcp => {
                if let Some(server) = extensions.servers.get(index) {
                    self.overlay = Some(Overlay::Form(FormState::edit_server(server)));
                }
            }
            HomeTab::Providers => {
                if let Some(provider) = extensions.providers.get(index) {
                    self.overlay = Some(Overlay::Form(FormState::edit_provider(provider)));
                }
            }
            HomeTab::Tools => {
                if let Some(tool) = extensions.tools.get(index) {
                    self.overlay = Some(Overlay::Form(FormState::edit_tool(tool)));
                }
            }
            HomeTab::Skills => {
                if let Some(skill) = extensions.skills.get(index) {
                    if skill.source == SkillSource::User {
                        return vec![Action::EditFile(skill.path.clone())];
                    }
                    if skill.source == SkillSource::Project && !skill.trusted {
                        return self.trust_project();
                    }
                    let text = format!("{} isn't one of re-code's own skills, so it's read-only here.", skill.name);
                    self.home_message = Some((text, true));
                }
            }
            HomeTab::Plugins => {
                use crate::extensions_view::PluginRow;
                match crate::extensions_view::plugin_rows(extensions).get(index) {
                    Some(PluginRow::Claude(claude)) => {
                        if let Some(info) = extensions.claude_plugins.get(*claude) {
                            let path = info.install_path.clone();
                            return self.open_plugin_install(Some(path));
                        }
                    }
                    Some(PluginRow::Marketplace(_)) => return self.browse_selected_marketplace(),
                    Some(PluginRow::Plugin(plugin)) => {
                        if let Some(plugin) = extensions.plugins.get(*plugin) {
                            let text = format!(
                                "{} is a folder of files; its skills show on the Skills tab and its servers on the MCP tab.",
                                plugin.name
                            );
                            self.home_message = Some((text, false));
                        }
                    }
                    None => {}
                }
            }
            HomeTab::Models | HomeTab::Home | HomeTab::Settings | HomeTab::Runtimes => {}
        }
        Vec::new()
    }

    /// `u` on the Plugins tab: check the selected plugin for a newer version.
    fn update_selected_plugin(&mut self) -> Vec<Action> {
        use crate::extensions_view::PluginRow;
        let Some((tab, index, extensions)) = self.selected_extension() else { return Vec::new() };
        if tab != HomeTab::Plugins {
            return Vec::new();
        }
        match crate::extensions_view::plugin_rows(extensions).get(index) {
            Some(PluginRow::Plugin(plugin)) => {
                let name = extensions.plugins.get(*plugin).map(|p| p.name.clone()).unwrap_or_default();
                self.update_plugin(&name)
            }
            Some(PluginRow::Marketplace(market)) => {
                // Re-reading a marketplace is adding it again at its newest commit.
                let repo = extensions.marketplaces.get(*market).map(|m| m.repo.clone()).unwrap_or_default();
                vec![Action::Extension(ExtensionCommand::AddMarketplace(repo))]
            }
            _ => {
                self.home_message = Some(("Only installed plugins and marketplaces can be updated.".to_owned(), true));
                Vec::new()
            }
        }
    }

    /// `b` on the Plugins tab: browse the selected marketplace, or the Hermes catalog.
    fn browse_selected_marketplace(&mut self) -> Vec<Action> {
        use crate::extensions_view::PluginRow;
        let name = match self.selected_extension() {
            Some((HomeTab::Plugins, index, extensions)) => {
                match crate::extensions_view::plugin_rows(extensions).get(index) {
                    Some(PluginRow::Marketplace(market)) => {
                        extensions.marketplaces.get(*market).map(|m| m.name.clone()).unwrap_or_default()
                    }
                    _ => "hermes".to_owned(),
                }
            }
            _ => "hermes".to_owned(),
        };
        self.browse_plugins(&name)
    }

    /// Keys on the Runtimes tab: choose a row, Enter offers to start a stopped runtime.
    fn runtimes_key(&mut self, key: KeyEvent, ctrl: bool) -> Vec<Action> {
        let count = self.extension_count();
        let Screen::Welcome(welcome) = &mut self.screen else { return Vec::new() };
        match key.code {
            KeyCode::Up | KeyCode::Char('k') if !ctrl => welcome.move_extension(-1, count),
            KeyCode::Down | KeyCode::Char('j') if !ctrl => welcome.move_extension(1, count),
            KeyCode::Enter => return self.runtimes_enter(),
            _ => {}
        }
        Vec::new()
    }

    /// Enter on a runtime row: a stopped one with a known start command asks before running it;
    /// anything else explains itself in the home message.
    fn runtimes_enter(&mut self) -> Vec<Action> {
        let count = self.extension_count();
        let Screen::Welcome(welcome) = &self.screen else { return Vec::new() };
        let index = welcome.extension_index(count);
        let Some(info) = &self.runtimes else { return Vec::new() };
        let rows = crate::runtimes_view::rows(info);
        let Some(row) = rows.get(index) else { return Vec::new() };
        let message = match row {
            crate::runtimes_view::RuntimesRow::Runtime(runtime) => match &runtime.state {
                crate::link::RuntimeRowState::Stopped { start_command: Some(command), needs_root } => {
                    if self.runtime_starting.is_some() {
                        return Vec::new();
                    }
                    let mut body = vec!["This runs exactly this command on your computer:".to_owned(), command.clone()];
                    if *needs_root {
                        body.push("It needs your password (sudo), which the terminal will ask for.".to_owned());
                    }
                    body.extend(runtime.notes.iter().cloned());
                    if runtime.yours {
                        body.push("This runtime is yours: re-code starts it now because you asked, and never stops or changes it.".to_owned());
                    }
                    self.overlay = Some(Overlay::Confirm(ConfirmState {
                        title: format!("Start {}?", runtime.name),
                        body,
                        command: Confirmed::StartRuntime(runtime.id.clone()),
                    }));
                    return Vec::new();
                }
                crate::link::RuntimeRowState::Stopped { start_command: None, .. } => (
                    format!(
                        "re-code doesn't know a safe way to start {}; start it yourself and press Ctrl+R.",
                        runtime.name
                    ),
                    true,
                ),
                crate::link::RuntimeRowState::NotAnswering { error } => (error.clone(), true),
                crate::link::RuntimeRowState::Running { .. } => (
                    format!(
                        "{} is already running{}.",
                        runtime.name,
                        runtime.url.as_deref().map(|u| format!(" at {u}")).unwrap_or_default()
                    ),
                    false,
                ),
            },
            crate::runtimes_view::RuntimesRow::Container(container) => (
                match &container.runs {
                    Some(runs) => {
                        format!("{} runs {runs}; its runtime entry above is where it starts.", container.name)
                    }
                    None => format!(
                        "{} isn't an inference runtime; re-code lists it but doesn't manage it.",
                        container.name
                    ),
                },
                false,
            ),
        };
        self.home_message = Some(message);
        Vec::new()
    }

    /// Whether a `/…` line names a usable skill rather than a command.
    fn invokes_skill(&self, text: &str) -> bool {
        let name = text.trim_start_matches('/').split(char::is_whitespace).next().unwrap_or_default();
        !name.is_empty() && self.skill_commands().iter().any(|(skill, _)| skill == name)
    }

    /// Usable skills as `/name` commands: (name, description).
    fn skill_commands(&self) -> Vec<(String, String)> {
        self.extensions
            .as_ref()
            .map(|e| {
                e.skills
                    .iter()
                    .filter(|s| s.enabled && s.problem.is_none())
                    .map(|s| (s.name.clone(), s.description.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// `f` on an MCP server: choose which of its tools the model may use. Needs a successful test
    /// first, so the tool names are known.
    fn open_tool_filter(&mut self) {
        let Some((HomeTab::Mcp, index, extensions)) = self.selected_extension() else { return };
        let Some(server) = extensions.servers.get(index) else { return };
        if server.external.is_some() {
            self.home_message = Some((
                format!(
                    "{} is read from another tool's configuration. Press i to copy it into re-code, then filter the copy.",
                    server.name
                ),
                true,
            ));
            return;
        }
        let Some(recode_core::extensions::McpProbe::Connected { tools, .. }) = extensions.probes.get(&server.name)
        else {
            self.home_message =
                Some((format!("Test {} first (t) so re-code knows which tools it offers.", server.name), true));
            return;
        };
        let tools: Vec<(String, bool)> =
            tools.iter().map(|tool| (tool.name.clone(), server.allows_tool(&tool.name))).collect();
        self.overlay = Some(Overlay::ToolFilter { server: server.name.clone(), tools, selected: 0 });
    }

    /// The driver answered a registry search.
    pub fn mcp_search_result(
        &mut self,
        query: &str,
        hits: Vec<recode_core::extensions::McpSearchHit>,
        error: Option<String>,
    ) {
        if let Some(Overlay::McpSearch { query: current, hits: shown, selected, searching, error: shown_error }) =
            &mut self.overlay
            && current.trim() == query
        {
            *searching = false;
            *selected = 0;
            *shown_error = if hits.is_empty() && error.is_none() {
                Some(format!("Nothing in the registry matches \"{query}\"."))
            } else {
                error
            };
            *shown = hits;
        }
    }

    /// `/web`: offers web-search MCP servers to set up.
    pub fn open_web_setup(&mut self) {
        if self.demo {
            self.toast("That isn't available in the demo recording.", ToastLevel::Info);
            return;
        }
        self.overlay = Some(Overlay::WebSetup { selected: 0 });
    }

    /// `/trust`: shows what the project folder's configuration would load, and asks.
    fn trust_project(&mut self) -> Vec<Action> {
        if self.demo {
            self.toast("That isn't available in the demo recording.", ToastLevel::Info);
            return Vec::new();
        }
        let Some(trust) = self.project_trust.clone() else {
            self.toast("Still looking at this folder's configuration; try again in a moment.", ToastLevel::Info);
            return Vec::new();
        };
        if trust.trusted {
            self.toast("This folder's configuration is already trusted.", ToastLevel::Info);
            return Vec::new();
        }
        if trust.skills.is_empty() && trust.files.is_empty() {
            self.toast("This folder has no .re-code configuration to trust.", ToastLevel::Info);
            return Vec::new();
        }
        let mut body = vec![format!("{} carries configuration that re-code would start using:", trust.folder)];
        for skill in &trust.skills {
            body.push(format!("  skill  {skill}  (.re-code/skills/{skill}/SKILL.md)"));
        }
        for file in &trust.files {
            if let Some(root_file) = file.strip_prefix("./") {
                body.push(format!("  file   {root_file}  (instructions, sent with every request)"));
            } else {
                body.push(format!("  file   .re-code/{file}"));
            }
        }
        body.push(
            "A repository can carry anything, so read these first. Trust is pinned to their content: a change asks again."
                .to_owned(),
        );
        self.overlay = Some(Overlay::Confirm(ConfirmState {
            title: "Trust this folder's configuration?".to_owned(),
            body,
            command: Confirmed::TrustProject,
        }));
        Vec::new()
    }

    /// Progress of a start the user approved.
    pub fn runtime_start_event(&mut self, event: crate::link::RuntimeStartEvent) {
        use crate::link::RuntimeStartEvent as E;
        match event {
            E::Started { id, command } => {
                self.runtime_starting = Some((id, "starting…".to_owned()));
                self.home_message = Some((format!("Running: {command}"), false));
            }
            E::Waiting { id, seconds } => {
                self.runtime_starting = Some((id, format!("waiting {seconds} s…")));
            }
            E::Ready { id, name, version, verified } => {
                if self.runtime_starting.as_ref().is_some_and(|(starting, _)| *starting == id) {
                    self.runtime_starting = None;
                }
                let version = version.map(|v| format!(" {v}")).unwrap_or_default();
                let text = if verified {
                    format!("{name}{version} is up.")
                } else {
                    format!("{name} was started. re-code couldn't check that it answers; Ctrl+R looks again.")
                };
                self.toast(text.clone(), ToastLevel::Info);
                self.home_message = Some((text, false));
            }
            E::Failed { id, error } => {
                if self.runtime_starting.as_ref().is_some_and(|(starting, _)| *starting == id) {
                    self.runtime_starting = None;
                }
                self.toast("The runtime didn't start.".to_owned(), ToastLevel::Warning);
                self.home_message = Some((format!("Couldn't start it: {error}"), true));
            }
        }
    }

    fn confirm_delete(&mut self) {
        let Some((tab, index, extensions)) = self.selected_extension() else { return };
        let (what, command) = match tab {
            HomeTab::Providers => match extensions.providers.get(index) {
                Some(provider) => (provider.name.clone(), ExtensionCommand::DeleteProvider(provider.name.clone())),
                None => return,
            },
            HomeTab::Mcp => match extensions.servers.get(index) {
                Some(server) => (server.name.clone(), ExtensionCommand::DeleteServer(server.name.clone())),
                None => return,
            },
            HomeTab::Tools => match extensions.tools.get(index) {
                Some(tool) => (tool.name.clone(), ExtensionCommand::DeleteTool(tool.name.clone())),
                None => return,
            },
            HomeTab::Skills => match extensions.skills.get(index) {
                Some(skill) if skill.source == SkillSource::User => {
                    (skill.name.clone(), ExtensionCommand::DeleteSkill(skill.name.clone()))
                }
                Some(skill) => {
                    let text =
                        format!("{} isn't one of re-code's own skills, so it can't be deleted here.", skill.name);
                    self.home_message = Some((text, true));
                    return;
                }
                None => return,
            },
            HomeTab::Plugins => {
                use crate::extensions_view::PluginRow;
                match crate::extensions_view::plugin_rows(extensions).get(index) {
                    Some(PluginRow::Plugin(plugin)) => match extensions.plugins.get(*plugin) {
                        Some(plugin) => (plugin.name.clone(), ExtensionCommand::DeletePlugin(plugin.name.clone())),
                        None => return,
                    },
                    Some(PluginRow::Marketplace(market)) => match extensions.marketplaces.get(*market) {
                        Some(market) => (market.name.clone(), ExtensionCommand::RemoveMarketplace(market.name.clone())),
                        None => return,
                    },
                    Some(PluginRow::Claude(_)) => {
                        self.home_message = Some(("That's Claude Code's; remove it there.".to_owned(), true));
                        return;
                    }
                    None => return,
                }
            }
            HomeTab::Models | HomeTab::Home | HomeTab::Settings | HomeTab::Runtimes => return,
        };
        let body = match (tab, &command) {
            (HomeTab::Skills, _) => {
                vec!["This permanently deletes the skill's directory and everything in it.".to_owned()]
            }
            (_, ExtensionCommand::DeletePlugin(_)) => {
                vec!["This removes the plugin's folder with its skills and MCP servers.".to_owned()]
            }
            (_, ExtensionCommand::RemoveMarketplace(_)) => {
                vec!["re-code stops following it. Plugins already installed from it stay.".to_owned()]
            }
            _ => vec!["This removes it from your configuration.".to_owned()],
        };
        self.overlay =
            Some(Overlay::Confirm(ConfirmState { title: format!("Delete {what}?"), body, command: command.into() }));
    }

    fn toggle_selected(&mut self) -> Vec<Action> {
        let Some((tab, index, extensions)) = self.selected_extension() else { return Vec::new() };
        let command = match tab {
            HomeTab::Providers => extensions
                .providers
                .get(index)
                .map(|provider| ExtensionCommand::SetProviderEnabled(provider.name.clone(), !provider.enabled)),
            HomeTab::Mcp => extensions
                .servers
                .get(index)
                .map(|server| ExtensionCommand::SetServerEnabled(server.name.clone(), !server.enabled)),
            HomeTab::Tools => extensions
                .tools
                .get(index)
                .map(|tool| ExtensionCommand::SetToolEnabled(tool.name.clone(), !tool.enabled)),
            HomeTab::Plugins => {
                use crate::extensions_view::PluginRow;
                match crate::extensions_view::plugin_rows(extensions).get(index) {
                    Some(PluginRow::Plugin(plugin)) => extensions
                        .plugins
                        .get(*plugin)
                        .map(|plugin| ExtensionCommand::SetPluginEnabled(plugin.name.clone(), !plugin.enabled)),
                    _ => None,
                }
            }
            HomeTab::Skills => match extensions.skills.get(index) {
                Some(skill) if skill.scan.blocks() => {
                    let text = format!(
                        "{} was flagged as dangerous by the scan, so it can't be enabled: {}",
                        skill.name,
                        skill.scan.findings().first().map(|f| f.reason.as_str()).unwrap_or_default()
                    );
                    self.home_message = Some((text, true));
                    None
                }
                Some(skill) if skill.source == SkillSource::Project && !skill.trusted => {
                    // Switching on a project skill means trusting the folder: show what that loads and ask.
                    return self.trust_project();
                }
                Some(skill) => Some(ExtensionCommand::SetSkillEnabled(skill.name.clone(), !skill.enabled)),
                None => None,
            },
            HomeTab::Models | HomeTab::Home | HomeTab::Settings | HomeTab::Runtimes => None,
        };
        command.map(Action::Extension).into_iter().collect()
    }

    fn toggle_exposed(&mut self) -> Vec<Action> {
        let Some((tab, index, extensions)) = self.selected_extension() else { return Vec::new() };
        if tab != HomeTab::Mcp {
            return Vec::new();
        }
        extensions
            .servers
            .get(index)
            .map(|server| ExtensionCommand::SetServerExposed(server.name.clone(), !server.expose_to_agent))
            .map(Action::Extension)
            .into_iter()
            .collect()
    }

    fn test_selected(&mut self) -> Vec<Action> {
        let Some((tab, index, extensions)) = self.selected_extension() else { return Vec::new() };
        if tab == HomeTab::Providers {
            return match extensions.providers.get(index) {
                // A cloud test is just an API call, so it needs no confirmation.
                Some(provider) => vec![Action::Extension(ExtensionCommand::TestProvider(provider.name.clone()))],
                None => Vec::new(),
            };
        }
        let Some(server) = extensions.servers.get(index) else { return Vec::new() };
        let command = ExtensionCommand::TestServer(server.name.clone());
        if matches!(server.transport, McpTransport::Stdio { .. }) {
            // A local server is a program, so nothing starts without seeing exactly what it runs.
            let where_it_runs = match server.sandbox {
                recode_core::extensions::McpSandbox::Bubblewrap => {
                    "This runs the following program in the bubblewrap sandbox (project writable, home emptied, secrets hidden; on the host only if bubblewrap is missing):"
                }
                recode_core::extensions::McpSandbox::None => {
                    "This runs the following program on your computer, outside any sandbox:"
                }
            };
            self.overlay = Some(Overlay::Confirm(ConfirmState {
                title: format!("Test {}?", server.name),
                body: vec![where_it_runs.to_owned(), server.connection_text()],
                command: command.into(),
            }));
            return Vec::new();
        }
        vec![Action::Extension(command)]
    }

    /// Handles a left click. Home-screen tabs and list rows are clickable, and clicking the selected row opens it.
    pub fn click(&mut self, column: u16, row: u16) -> Vec<Action> {
        if self.overlay.is_some() {
            return Vec::new();
        }
        // In the conversation, clicking a reasoning block or a finished tool expands or collapses it.
        // Only the transcript's own columns count, so a click in the side panel does nothing here.
        if self.screen == Screen::Conversation {
            // A file in the Files pane opens; a folder opens or closes.
            if let Some((_, _, _, index)) =
                self.hits.tree_rows.iter().find(|(y, from, to, _)| *y == row && (*from..*to).contains(&column)).copied()
            {
                let rows = self.tree.rows(&self.project_files);
                if let Some(entry) = rows.get(index) {
                    if entry.is_dir {
                        self.tree.toggle(&entry.path);
                        return Vec::new();
                    }
                    return vec![Action::EditFile(entry.path.clone())];
                }
            }
            // A project path mentioned in the conversation opens in the editor.
            if let Some((_, _, _, path)) =
                self.hits.paths.iter().find(|(y, from, to, _)| *y == row && (*from..*to).contains(&column))
            {
                return vec![Action::EditFile(path.clone())];
            }
            // A changed file in the side panel opens in the editor.
            if let Some((_, _, _, path)) =
                self.hits.panel_files.iter().find(|(y, from, to, _)| *y == row && (*from..*to).contains(&column))
            {
                return vec![Action::EditFile(path.clone())];
            }
            let hit = self.hits.lines.iter().find(|(y, from, to, _)| *y == row && (*from..*to).contains(&column));
            if let Some((_, _, _, entry)) = hit.copied() {
                self.transcript.toggle(entry);
            }
            return Vec::new();
        }
        let tab = self.hits.tabs.iter().find(|(y, from, to, _)| *y == row && (*from..*to).contains(&column));
        if let (Some((_, _, _, tab)), Screen::Welcome(welcome)) = (tab, &mut self.screen) {
            welcome.tab = *tab;
            return Vec::new();
        }
        let hit = self.hits.rows.iter().find(|(y, from, to, _)| *y == row && (*from..*to).contains(&column));
        let Some((_, _, _, index)) = hit.copied() else { return Vec::new() };
        let Screen::Welcome(welcome) = &mut self.screen else { return Vec::new() };
        if welcome.tab == HomeTab::Home {
            welcome.set_extension_index(index);
            let rows = crate::home::dashboard_rows(self);
            return self.open_dash_item(rows.get(index).cloned());
        }
        if welcome.tab == HomeTab::Settings {
            welcome.set_extension_index(index);
            return Vec::new();
        }
        if welcome.tab == HomeTab::Runtimes {
            let count = self.extension_count();
            let Screen::Welcome(welcome) = &mut self.screen else { return Vec::new() };
            let same = welcome.extension_index(count) == index;
            welcome.set_extension_index(index);
            return if same { self.runtimes_enter() } else { Vec::new() };
        }
        if welcome.tab == HomeTab::Models {
            if welcome.selected == index {
                return self.choose_selected_model();
            }
            welcome.selected = index;
        } else {
            let count = self.extension_count();
            let Screen::Welcome(welcome) = &mut self.screen else { return Vec::new() };
            if welcome.extension_index(count) == index {
                return self.edit_selected();
            }
            welcome.set_extension_index(index);
        }
        Vec::new()
    }

    fn control_key(&mut self, code: KeyCode) -> Vec<Action> {
        match code {
            KeyCode::Char('c') => {
                if !self.input.is_empty() {
                    self.input.take();
                } else if self.quit_armed {
                    return vec![Action::Quit];
                } else {
                    self.quit_armed = true;
                }
            }
            KeyCode::Char('k') => self.overlay = Some(Overlay::Palette { query: String::new(), selected: 0 }),
            KeyCode::Char('r') => {
                self.overlay = Some(Overlay::HistorySearch { query: String::new(), all_projects: false, selected: 0 });
            }
            KeyCode::Char('x') => return self.open_transcript_mode(),
            KeyCode::Char('l') => return self.run_command(PaletteCommand::SwitchModel),
            KeyCode::Char('b') => return self.run_command(PaletteCommand::TogglePanel),
            KeyCode::Char('t') => return self.run_command(PaletteCommand::ToggleReasoning),
            KeyCode::Char('o') => return self.run_command(PaletteCommand::LastOutput),
            KeyCode::Char('z') => return self.run_command(PaletteCommand::Undo),
            KeyCode::Left => self.input.word_left(),
            KeyCode::Right => self.input.word_right(),
            KeyCode::Delete => self.input.delete_to_line_end(),
            // Back to the latest output from anywhere, whatever is typed.
            KeyCode::End => self.transcript.follow(),
            KeyCode::Home => {
                let (width, height) = self.view_size;
                self.transcript.scroll_to_top(width, height, self.options);
            }
            KeyCode::Char('y') => return self.run_command(PaletteCommand::CopyCode),
            KeyCode::Char('v') => return vec![Action::PasteImage],
            KeyCode::Char('j') => self.input.insert("\n"),
            KeyCode::Char('u') => self.input.delete_to_line_start(),
            KeyCode::Char('w') => self.input.delete_word(),
            // Ctrl+Backspace: as Backspace with Ctrl (keyboard protocol) or as ^H (VTE terminals).
            KeyCode::Backspace | KeyCode::Char('h') => self.input.delete_word_back(),
            KeyCode::Char('a') => self.input.line_start(),
            KeyCode::Char('e') => return self.run_command(PaletteCommand::Extensions),
            _ => {}
        }
        Vec::new()
    }

    fn plain_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let (width, height) = self.view_size;
        let page = isize::try_from(height.saturating_sub(2).max(1)).unwrap_or(isize::MAX);
        match key.code {
            KeyCode::Esc if self.is_busy() => return vec![Action::Interrupt],
            KeyCode::Esc if self.input.is_empty() => {
                // Esc twice on an empty prompt opens the rewind picker, as in Claude Code.
                let again = self.last_esc.is_some_and(|at| self.now.duration_since(at) < Duration::from_millis(600));
                self.last_esc = Some(self.now);
                if again {
                    self.last_esc = None;
                    return self.open_rewind();
                }
            }
            KeyCode::BackTab => return self.cycle_permission_mode(),
            KeyCode::Enter => {
                let wants_newline = key.modifiers.intersects(KeyModifiers::ALT | KeyModifiers::SHIFT);
                if wants_newline {
                    self.input.insert("\n");
                } else if !self.input.text().trim().is_empty() {
                    let text = self.input.text().trim().to_owned();
                    // `# remember this` is a note for an instruction file, never a message.
                    if let Some(note) = text.strip_prefix("# ").filter(|note| !note.trim().is_empty()) {
                        if self.demo {
                            self.toast("Notes aren't written in the demo recording.", ToastLevel::Info);
                            return Vec::new();
                        }
                        let note = note.trim().to_owned();
                        self.input.take();
                        self.history_position = None;
                        self.overlay = Some(Overlay::NoteTarget { text: note, selected: 0 });
                        return Vec::new();
                    }
                    // `/skill-name request` goes to the model as a message; the driver applies the skill.
                    if text.starts_with('/') && !text.starts_with("//") && !self.invokes_skill(&text) {
                        self.input.take();
                        self.history_position = None;
                        return self.run_slash(&text);
                    }
                    // `!command` runs in the sandbox without the model; `!!command` with network.
                    if let Some(rest) = text.strip_prefix('!').filter(|rest| !rest.trim().is_empty()) {
                        if self.demo {
                            self.toast("Commands don't run in the demo recording.", ToastLevel::Info);
                            return Vec::new();
                        }
                        if self.is_busy() {
                            self.toast(
                                "A command can run once this turn finishes (Esc stops it).",
                                ToastLevel::Warning,
                            );
                            return Vec::new();
                        }
                        let (network, command) = match rest.strip_prefix('!') {
                            Some(with_network) => (true, with_network.trim().to_owned()),
                            None => (false, rest.trim().to_owned()),
                        };
                        // A trailing `&` means in the background, as in a shell.
                        let (command, background) = match command.strip_suffix('&') {
                            Some(rest) if !rest.trim().is_empty() => (rest.trim().to_owned(), true),
                            _ => (command, false),
                        };
                        if command.is_empty() {
                            return Vec::new();
                        }
                        self.input.take();
                        if self.history.last() != Some(&text) {
                            self.history.push(text.clone());
                        }
                        self.history_position = None;
                        return vec![Action::RunShell { command, network, background }];
                    }
                    if self.is_busy() && !self.demo {
                        // Typed while the model works: queued, and sent after its tool calls or as the next turn.
                        let images = std::mem::take(&mut self.pending_images);
                        let mut text = self.input.take();
                        if text.starts_with("//") || text.starts_with("\\#") || text.starts_with("\\!") {
                            text.remove(0);
                        }
                        if self.history.last() != Some(&text) {
                            self.history.push(text.clone());
                        }
                        self.history_position = None;
                        self.queue.push_back((text, images));
                        if key.modifiers.contains(KeyModifiers::CONTROL) {
                            return vec![Action::Interrupt];
                        }
                        return Vec::new();
                    }
                    if !self.is_busy() {
                        let images = std::mem::take(&mut self.pending_images);
                        let mut text = self.input.take();
                        if text.starts_with("//") || text.starts_with("\\#") || text.starts_with("\\!") {
                            text.remove(0);
                        }
                        if self.history.last() != Some(&text) {
                            self.history.push(text.clone());
                        }
                        self.history_position = None;
                        return vec![Action::Send { text, images }];
                    }
                }
            }
            KeyCode::Char(c) => self.input.insert(c.encode_utf8(&mut [0; 4])),
            KeyCode::Backspace if self.input.is_empty() && !self.pending_images.is_empty() => {
                self.pending_images.pop();
            }
            // Backspace on an empty prompt takes the last queued message back for editing.
            KeyCode::Backspace if self.input.is_empty() && !self.queue.is_empty() => {
                if let Some((text, images)) = self.queue.pop_back() {
                    self.input.insert(&text);
                    self.pending_images = images;
                }
            }
            KeyCode::Backspace if key.modifiers.contains(KeyModifiers::ALT) => self.input.delete_word_back(),
            KeyCode::Backspace => self.input.backspace(),
            KeyCode::Delete => self.input.delete(),
            KeyCode::Left if key.modifiers.contains(KeyModifiers::ALT) => self.input.word_left(),
            KeyCode::Right if key.modifiers.contains(KeyModifiers::ALT) => self.input.word_right(),
            KeyCode::Left => self.input.left(),
            KeyCode::Right => self.input.right(),
            // Up on an empty prompt (or while already browsing) walks back through earlier prompts.
            KeyCode::Up if self.input.is_empty() || self.history_position.is_some() => {
                if !self.history_step(true) {
                    self.scroll(-1);
                }
            }
            KeyCode::Down if self.history_position.is_some() => {
                self.history_step(false);
            }
            KeyCode::Up => {
                if !self.input.move_vertical(self.input_width, false) {
                    self.scroll(-1);
                }
            }
            KeyCode::Down => {
                if !self.input.move_vertical(self.input_width, true) {
                    self.scroll(1);
                }
            }
            KeyCode::PageUp => self.scroll(-page),
            KeyCode::PageDown => self.scroll(page),
            KeyCode::Home if self.input.is_empty() => self.transcript.scroll_to_top(width, height, self.options),
            KeyCode::End if self.input.is_empty() => self.transcript.follow(),
            KeyCode::Home => self.input.line_start(),
            KeyCode::End => self.input.line_end(),
            KeyCode::F(1) => self.overlay = Some(Overlay::Help),
            _ => {}
        }
        Vec::new()
    }

    fn overlay_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            self.overlay = None;
            return Vec::new();
        }
        if matches!(self.overlay, Some(Overlay::FreeSetup(_))) {
            return self.free_setup_key(key);
        }
        if matches!(self.overlay, Some(Overlay::Install(_))) {
            return self.install_key(key);
        }
        if matches!(self.overlay, Some(Overlay::SkillSearch { .. })) {
            return self.skill_search_key(key);
        }
        if matches!(self.overlay, Some(Overlay::PluginBrowse { .. })) {
            return self.plugin_browse_key(key);
        }
        let Some(overlay) = &mut self.overlay else { return Vec::new() };
        match overlay {
            Overlay::Themes { selected, original } => {
                let original = original.clone();
                let names = crate::theme::names();
                let count = names.len().max(1);
                let index = match key.code {
                    KeyCode::Up => (*selected + count - 1) % count,
                    KeyCode::Down => (*selected + 1) % count,
                    _ => (*selected).min(count - 1),
                };
                *selected = index;
                let current = self.theme.clone();
                let mut next = ThemeSelection { name: names[index].clone(), ..current.clone() };
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => {
                        self.overlay = None;
                        self.use_theme(original);
                        return Vec::new();
                    }
                    KeyCode::Left => next.variant = current.variant.previous(),
                    KeyCode::Right => next.variant = current.variant.next(),
                    KeyCode::Char('b') => next.solid = !current.solid,
                    // The theme's own accent, then its other bright colours, in turn.
                    KeyCode::Char('a') => {
                        let palette = crate::theme::resolve(&ThemeSelection::named(&next.name));
                        let choices: Vec<Color> = palette
                            .map(|p| vec![p.colors[2], p.colors[3], p.colors[4], p.colors[5]])
                            .unwrap_or_default();
                        next.accent = match current.accent {
                            None => choices.first().copied(),
                            Some(accent) => {
                                let at = choices.iter().position(|c| *c == accent);
                                at.and_then(|at| choices.get(at + 1).copied())
                            }
                        };
                    }
                    KeyCode::Enter => {
                        self.overlay = None;
                        let name = next.name.clone();
                        self.use_theme(next.clone());
                        self.toast(format!("Theme: {name}"), ToastLevel::Success);
                        return vec![Action::SetTheme(next)];
                    }
                    _ => {}
                }
                // Moving through the list previews each theme on the whole screen.
                self.use_theme(next);
            }
            // Handled above, before the overlay is borrowed.
            Overlay::FreeSetup(_) => {}
            Overlay::Sessions { sessions, selected } => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => self.overlay = None,
                KeyCode::Up => *selected = selected.saturating_sub(1),
                KeyCode::Down => *selected = (*selected + 1).min(sessions.len().saturating_sub(1)),
                KeyCode::Char('d') => {
                    if let Some(session) = sessions.get(*selected).cloned() {
                        let label = format!("{} · {} · {} messages", session.title, session.age, session.messages);
                        // The picker comes back with the list once the deletion is done.
                        self.sessions_wanted = true;
                        self.confirm_delete_session(session.id, label);
                    }
                }
                KeyCode::Enter => {
                    let id = sessions.get(*selected).map(|session| session.id.clone());
                    self.overlay = None;
                    if let Some(id) = id {
                        return vec![Action::ContinueSession(id)];
                    }
                }
                _ => {}
            },
            Overlay::McpSearch { query, hits, selected, searching, error } => match key.code {
                KeyCode::Esc => self.overlay = None,
                KeyCode::Up => *selected = selected.saturating_sub(1),
                KeyCode::Down => *selected = (*selected + 1).min(hits.len().saturating_sub(1)),
                KeyCode::Backspace => {
                    query.pop();
                    hits.clear();
                    *error = None;
                }
                KeyCode::Char(c) if !ctrl => {
                    query.push(c);
                    hits.clear();
                    *error = None;
                }
                KeyCode::Enter => {
                    if let Some(hit) = hits.get((*selected).min(hits.len().saturating_sub(1))) {
                        match &hit.server {
                            Some(server) => {
                                let server = server.clone();
                                let name = server.name.clone();
                                self.overlay = None;
                                self.home_message =
                                    Some((format!("Added {name} from the registry. Press t to test it."), false));
                                return vec![Action::Extension(ExtensionCommand::SaveServer {
                                    original: None,
                                    server,
                                })];
                            }
                            None => {
                                let why =
                                    hit.unsupported.clone().unwrap_or_else(|| "no package re-code can run".to_owned());
                                *error = Some(format!("{} can't be added: {why}", hit.name));
                            }
                        }
                    } else if !query.trim().is_empty() && !*searching {
                        *searching = true;
                        *error = None;
                        return vec![Action::Extension(ExtensionCommand::SearchMcp(query.trim().to_owned()))];
                    }
                }
                _ => {}
            },
            // Handled above, before the borrow.
            Overlay::Install(_) | Overlay::SkillSearch { .. } | Overlay::PluginBrowse { .. } => {}
            Overlay::ToolFilter { server, tools, selected } => match key.code {
                KeyCode::Esc => self.overlay = None,
                KeyCode::Up => *selected = selected.saturating_sub(1),
                KeyCode::Down => *selected = (*selected + 1).min(tools.len().saturating_sub(1)),
                KeyCode::Char(' ') => {
                    if let Some(tool) = tools.get_mut(*selected) {
                        tool.1 = !tool.1;
                    }
                }
                KeyCode::Char('a') => tools.iter_mut().for_each(|tool| tool.1 = true),
                KeyCode::Char('n') => tools.iter_mut().for_each(|tool| tool.1 = false),
                KeyCode::Enter => {
                    let server = server.clone();
                    let filter = if tools.iter().all(|tool| tool.1) {
                        None
                    } else {
                        Some(recode_core::extensions::ToolFilter {
                            include: tools.iter().filter(|tool| tool.1).map(|tool| tool.0.clone()).collect(),
                            exclude: Vec::new(),
                        })
                    };
                    self.overlay = None;
                    return vec![Action::Extension(ExtensionCommand::SetToolFilter { server, tools: filter })];
                }
                _ => {}
            },
            Overlay::McpPresets { selected } => match key.code {
                KeyCode::Esc => self.overlay = None,
                KeyCode::Up => *selected = selected.saturating_sub(1),
                KeyCode::Down => *selected = (*selected + 1).min(MCP_PRESETS.len().saturating_sub(1)),
                KeyCode::Enter => {
                    let preset = MCP_PRESETS.get((*selected).min(MCP_PRESETS.len().saturating_sub(1)));
                    self.overlay = preset.map(|preset| Overlay::Form(FormState::from_mcp_preset(preset)));
                }
                _ => {}
            },
            Overlay::WebSetup { selected } => match key.code {
                KeyCode::Esc => self.overlay = None,
                KeyCode::Up => *selected = selected.saturating_sub(1),
                KeyCode::Down => *selected = (*selected + 1).min(WEB_SEARCH_PRESETS.len().saturating_sub(1)),
                KeyCode::Enter => {
                    let preset = WEB_SEARCH_PRESETS.get((*selected).min(WEB_SEARCH_PRESETS.len().saturating_sub(1)));
                    self.overlay = preset.map(|preset| Overlay::Form(FormState::from_mcp_preset(preset)));
                    self.home_message = Some((
                        "Fill in the address or key, Enter saves it, then t tests it. The model gets the server's search tools in its next turn."
                            .to_owned(),
                        false,
                    ));
                }
                _ => {}
            },
            Overlay::ProviderPresets { selected } => match key.code {
                KeyCode::Esc => self.overlay = None,
                KeyCode::Up => *selected = selected.saturating_sub(1),
                KeyCode::Down => *selected = (*selected + 1).min(PROVIDER_PRESETS.len().saturating_sub(1)),
                KeyCode::Enter => {
                    let preset = PROVIDER_PRESETS.get((*selected).min(PROVIDER_PRESETS.len().saturating_sub(1)));
                    self.overlay = preset.map(|preset| Overlay::Form(FormState::from_provider_preset(preset)));
                }
                _ => {}
            },
            Overlay::Confirm(confirm) => match key.code {
                KeyCode::Char('y') | KeyCode::Enter => {
                    let command = confirm.command.clone();
                    self.overlay = None;
                    return match command {
                        Confirmed::Extension(command) => vec![Action::Extension(command)],
                        Confirmed::StartRuntime(id) => {
                            self.runtime_starting = Some((id.clone(), "starting…".to_owned()));
                            vec![Action::StartRuntime(id)]
                        }
                        Confirmed::TrustProject => vec![Action::TrustProject],
                        Confirmed::WriteInit => vec![Action::WriteInit],
                        Confirmed::ForgetMemory { scope, name } => vec![Action::ForgetMemory { scope, name }],
                        Confirmed::DeleteSession(id) => vec![Action::DeleteSession(id)],
                    };
                }
                KeyCode::Char('n') | KeyCode::Esc => self.overlay = None,
                _ => {}
            },
            Overlay::Form(form) => match form.handle_key(key) {
                crate::form::FormOutcome::Stay => {}
                crate::form::FormOutcome::Cancel => self.overlay = None,
                crate::form::FormOutcome::Submit(command) => {
                    self.overlay = None;
                    return vec![Action::Extension(*command)];
                }
            },
            Overlay::Help => {
                if matches!(key.code, KeyCode::Esc | KeyCode::Char('q') | KeyCode::F(1) | KeyCode::Enter) {
                    self.overlay = None;
                }
            }
            Overlay::Tasks { tasks, selected } => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => self.overlay = None,
                KeyCode::Up => *selected = selected.saturating_sub(1),
                KeyCode::Down => *selected = (*selected + 1).min(tasks.len().saturating_sub(1)),
                KeyCode::Enter => {
                    if let Some(task) = tasks.get(*selected) {
                        let id = task.id.clone();
                        self.overlay = None;
                        return vec![Action::ShowTaskOutput(id)];
                    }
                }
                KeyCode::Char('k') => {
                    if let Some(task) = tasks.get(*selected) {
                        return vec![Action::KillTask(task.id.clone())];
                    }
                }
                _ => {}
            },
            Overlay::Transcript { offset, typing, query, matches, current } => {
                let page = self.view_size.1.max(1);
                if let Some(text) = typing {
                    match key.code {
                        KeyCode::Esc => *typing = None,
                        KeyCode::Backspace => {
                            text.pop();
                        }
                        KeyCode::Enter => {
                            let wanted = text.to_lowercase();
                            *query = std::mem::take(text);
                            *typing = None;
                            let width = self.terminal_width.saturating_sub(4);
                            let lines = self.transcript.plain_lines(width, self.options);
                            *matches = lines
                                .iter()
                                .enumerate()
                                .filter(|(_, line)| !wanted.is_empty() && line.to_lowercase().contains(&wanted))
                                .map(|(index, _)| index)
                                .collect();
                            *current = matches.iter().position(|line| *line >= *offset).unwrap_or(0);
                            if let Some(line) = matches.get(*current) {
                                *offset = *line;
                            }
                        }
                        KeyCode::Char(c) if !ctrl => text.push(c),
                        _ => {}
                    }
                    return Vec::new();
                }
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => self.overlay = None,
                    KeyCode::Char('/') => *typing = Some(String::new()),
                    KeyCode::Char('n') if !matches.is_empty() => {
                        *current = (*current + 1) % matches.len();
                        *offset = matches[*current];
                    }
                    KeyCode::Char('N') if !matches.is_empty() => {
                        *current = (*current + matches.len() - 1) % matches.len();
                        *offset = matches[*current];
                    }
                    KeyCode::Up | KeyCode::Char('k') => *offset = offset.saturating_sub(1),
                    KeyCode::Down | KeyCode::Char('j') => *offset = offset.saturating_add(1),
                    KeyCode::PageUp | KeyCode::Char('b') => *offset = offset.saturating_sub(page),
                    KeyCode::PageDown | KeyCode::Char(' ') | KeyCode::Char('f') => {
                        *offset = offset.saturating_add(page)
                    }
                    KeyCode::Home | KeyCode::Char('g') => *offset = 0,
                    KeyCode::End | KeyCode::Char('G') => *offset = usize::MAX,
                    KeyCode::Char('[') => {
                        let width = self.terminal_width.saturating_sub(4).max(20);
                        let text = self.transcript.plain_lines(width, self.options).join("\n");
                        return vec![Action::DumpScrollback(text)];
                    }
                    KeyCode::Char('v') => {
                        let width = self.terminal_width.saturating_sub(4).max(20);
                        let text = self.transcript.plain_lines(width, self.options).join("\n");
                        let path = std::env::temp_dir().join(format!("re-code-transcript-{}.txt", std::process::id()));
                        match std::fs::write(&path, text) {
                            Ok(()) => {
                                self.overlay = None;
                                return vec![Action::EditFile(path.display().to_string())];
                            }
                            Err(error) => {
                                self.toast(format!("Couldn't write the transcript: {error}"), ToastLevel::Warning)
                            }
                        }
                    }
                    _ => {}
                }
            }
            Overlay::HistorySearch { query, all_projects, selected } => match key.code {
                KeyCode::Esc => self.overlay = None,
                KeyCode::Up => *selected = selected.saturating_sub(1),
                KeyCode::Down => *selected += 1,
                KeyCode::Tab => {
                    *all_projects = !*all_projects;
                    *selected = 0;
                    if *all_projects && self.history_all.is_none() {
                        return vec![Action::LoadHistory];
                    }
                }
                KeyCode::Backspace => {
                    query.pop();
                    *selected = 0;
                }
                KeyCode::Enter => {
                    let (query, all_projects, selected) = (query.clone(), *all_projects, *selected);
                    let matches = self.history_matches(&query, all_projects);
                    if let Some(prompt) = matches.get(selected.min(matches.len().saturating_sub(1))) {
                        let prompt = prompt.clone();
                        self.overlay = None;
                        self.input.set(&prompt);
                    }
                }
                KeyCode::Char(c) if !ctrl => {
                    query.push(c);
                    *selected = 0;
                }
                _ => {}
            },
            Overlay::Rewind { turns, selected, choice } => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => self.overlay = None,
                KeyCode::Up => *selected = selected.saturating_sub(1),
                KeyCode::Down => *selected = (*selected + 1).min(turns.len().saturating_sub(1)),
                KeyCode::Left => *choice = choice.saturating_sub(1),
                KeyCode::Right | KeyCode::Tab => *choice = (*choice + 1) % 3,
                KeyCode::Enter => {
                    if let Some(to_turn) = turns.get(*selected).map(|(turn, _)| *turn) {
                        let choice = *choice;
                        self.overlay = None;
                        return vec![Action::Rewind { to_turn, files: choice == 1, summarize: choice == 2 }];
                    }
                }
                _ => {}
            },
            Overlay::Permissions { project, selected, .. } => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => self.overlay = None,
                KeyCode::Up => *selected = selected.saturating_sub(1),
                KeyCode::Down => *selected = (*selected + 1).min(project.len().saturating_sub(1)),
                KeyCode::Char('d') => {
                    if let Some((json, label)) = project.get(*selected).cloned() {
                        project.remove(*selected);
                        *selected = (*selected).min(project.len().saturating_sub(1));
                        self.toast(format!("Forgot: {label}"), ToastLevel::Info);
                        return vec![Action::RevokePermission(json)];
                    }
                }
                KeyCode::BackTab => {
                    let actions = self.cycle_permission_mode();
                    if let Some(Overlay::Permissions { mode, .. }) = &mut self.overlay {
                        *mode = self.permission_mode;
                    }
                    return actions;
                }
                _ => {}
            },
            Overlay::Memory { files, user_path, project_path, selected } => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => self.overlay = None,
                KeyCode::Up => *selected = selected.saturating_sub(1),
                KeyCode::Down => *selected = (*selected + 1).min(files.len().saturating_sub(1)),
                KeyCode::Enter => {
                    let path = files.get(*selected).map(|file| file.path.clone());
                    if let Some(path) = path {
                        self.overlay = None;
                        return vec![Action::EditFile(path)];
                    }
                }
                KeyCode::Char('u') => {
                    let path = user_path.clone();
                    self.overlay = None;
                    return vec![Action::EditFile(path)];
                }
                KeyCode::Char('d') => {
                    if let Some((scope, name)) = files.get(*selected).and_then(|file| file.memory.clone()) {
                        self.overlay = Some(Overlay::Confirm(ConfirmState {
                            title: format!("Forget \"{name}\"?"),
                            body: vec![
                                "re-code stops bringing this up in later sessions; the file is deleted.".to_owned(),
                            ],
                            command: Confirmed::ForgetMemory { scope, name },
                        }));
                    }
                }
                KeyCode::Char('n') => {
                    let Some(project) = project_path.clone() else {
                        self.toast("There's no project folder here to write a RE-CODE.md in.", ToastLevel::Info);
                        return Vec::new();
                    };
                    if files.iter().any(|file| file.path == project) {
                        self.overlay = None;
                        return vec![Action::EditFile(project)];
                    }
                    self.overlay = None;
                    return vec![Action::Init];
                }
                _ => {}
            },
            Overlay::NoteTarget { text, selected } => match key.code {
                KeyCode::Esc => {
                    let text = format!("# {text}");
                    self.overlay = None;
                    self.input.insert(&text);
                }
                KeyCode::Up => *selected = selected.saturating_sub(1),
                KeyCode::Down => *selected = (*selected + 1).min(crate::link::NoteScope::ALL.len() - 1),
                KeyCode::Enter => {
                    let scope = crate::link::NoteScope::ALL[(*selected).min(crate::link::NoteScope::ALL.len() - 1)];
                    let text = text.clone();
                    self.overlay = None;
                    return vec![Action::WriteNote { scope, text }];
                }
                _ => {}
            },
            Overlay::Compactions { reports, selected } => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => self.overlay = None,
                KeyCode::Up => *selected = selected.saturating_sub(1),
                KeyCode::Down => *selected = (*selected + 1).min(reports.len().saturating_sub(1)),
                KeyCode::Enter => {
                    let seq = reports.get(*selected).map(|(_, report)| report.seq);
                    if let Some(seq) = seq {
                        self.overlay = None;
                        return vec![Action::ShowCompaction(seq)];
                    }
                }
                _ => {}
            },
            Overlay::Context => match key.code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => self.overlay = None,
                KeyCode::Char('c') if !self.is_busy() && !self.demo => {
                    self.overlay = None;
                    return self.run_command(PaletteCommand::Compact);
                }
                KeyCode::Char('r') if !self.demo => {
                    self.overlay = None;
                    return vec![Action::ShowLastRequest];
                }
                _ => {}
            },
            Overlay::Pager { text, is_diff, side_by_side, offset, .. } => {
                let page = self.view_size.1.saturating_sub(2).max(1);
                let width = self.terminal_width.saturating_sub(4);
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => self.overlay = None,
                    KeyCode::Up | KeyCode::Char('k') => *offset = offset.saturating_sub(1),
                    KeyCode::Down | KeyCode::Char('j') => *offset = offset.saturating_add(1),
                    KeyCode::PageUp => *offset = offset.saturating_sub(page),
                    KeyCode::PageDown | KeyCode::Char(' ') => *offset = offset.saturating_add(page),
                    KeyCode::Home => *offset = 0,
                    // Drawing clamps the offset to the last page.
                    KeyCode::End => *offset = usize::MAX,
                    KeyCode::Char('s') if *is_diff => *side_by_side = !*side_by_side,
                    // Jump between the files of a diff, and open the current one in the editor.
                    KeyCode::Char('n') if *is_diff => {
                        let starts = diff_file_starts(text, width, *side_by_side);
                        if let Some(next) = starts.iter().find(|(line, _)| *line > *offset) {
                            *offset = next.0;
                        }
                    }
                    KeyCode::Char('p') if *is_diff => {
                        let starts = diff_file_starts(text, width, *side_by_side);
                        if let Some(previous) = starts.iter().rev().find(|(line, _)| *line < *offset) {
                            *offset = previous.0;
                        }
                    }
                    KeyCode::Char('e') if *is_diff => {
                        let starts = diff_file_starts(text, width, *side_by_side);
                        let current = starts.iter().rev().find(|(line, _)| *line <= *offset).or(starts.first());
                        if let Some((_, path)) = current {
                            let path = path.clone();
                            self.overlay = None;
                            return vec![Action::EditFile(path)];
                        }
                    }
                    _ => {}
                }
            }
            Overlay::CodeBlocks { blocks, selected } => {
                let count = blocks.len().max(1);
                let chosen = match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => {
                        self.overlay = None;
                        return Vec::new();
                    }
                    KeyCode::Up => {
                        *selected = (*selected + count - 1) % count;
                        return Vec::new();
                    }
                    KeyCode::Down => {
                        *selected = (*selected + 1) % count;
                        return Vec::new();
                    }
                    KeyCode::Enter => blocks.get(*selected).cloned(),
                    KeyCode::Char(digit @ '1'..='9') => blocks.get(usize::from(digit as u8 - b'1')).cloned(),
                    _ => return Vec::new(),
                };
                if let Some((language, code)) = chosen {
                    self.overlay = None;
                    let what = if language.is_empty() { "code block".to_owned() } else { format!("{language} block") };
                    self.toast(
                        format!("Copied the {what} ({} lines)", code.lines().count().max(1)),
                        ToastLevel::Success,
                    );
                    return vec![Action::Copy(code)];
                }
            }
            Overlay::Palette { query, selected } => match key.code {
                KeyCode::Esc => self.overlay = None,
                KeyCode::Up => *selected = selected.saturating_sub(1),
                KeyCode::Down => *selected = selected.saturating_add(1),
                KeyCode::Backspace => {
                    query.pop();
                    *selected = 0;
                }
                KeyCode::Char(c) if !ctrl => {
                    query.push(c);
                    *selected = 0;
                }
                KeyCode::Enter => {
                    let (query, selected) = (query.clone(), *selected);
                    let matches = palette_matches_recent(&self.palette_recent, &query);
                    let choice = matches.get(selected.min(matches.len().saturating_sub(1))).map(|item| item.0);
                    self.overlay = None;
                    if let Some(command) = choice {
                        self.palette_recent.retain(|recent| *recent != command);
                        self.palette_recent.insert(0, command);
                        self.palette_recent.truncate(5);
                        return self.run_command(command);
                    }
                }
                _ => {}
            },
        }
        Vec::new()
    }

    /// Copies the last code block from the conversation, or the whole last answer if it has none.
    fn copy_last_code(&mut self) -> Vec<Action> {
        let last = self.transcript.iter().rev().find_map(|entry| match entry {
            Entry::Assistant(text) => Some(text.clone()),
            _ => None,
        });
        let Some(text) = last else {
            self.toast("Nothing to copy yet.", ToastLevel::Info);
            return Vec::new();
        };
        let blocks = code_blocks(&text);
        match blocks.len() {
            // Several blocks: ask which, rather than guess the last.
            2.. => {
                self.overlay = Some(Overlay::CodeBlocks { blocks, selected: 0 });
                Vec::new()
            }
            1 => {
                let (_, code) = blocks.into_iter().next().unwrap_or_default();
                let lines = code.lines().count().max(1);
                self.toast(format!("Copied the last code block ({lines} lines)"), ToastLevel::Success);
                vec![Action::Copy(code)]
            }
            _ => {
                let lines = text.lines().count().max(1);
                self.toast(format!("Copied the last answer ({lines} lines)"), ToastLevel::Success);
                vec![Action::Copy(text)]
            }
        }
    }

    /// Every diff the session's tools made, as one document for the pager.
    fn session_diff(&self) -> String {
        self.transcript
            .iter()
            .filter_map(|entry| match entry {
                Entry::Tool(tool) => tool.diff.clone(),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn run_command(&mut self, command: PaletteCommand) -> Vec<Action> {
        match command {
            PaletteCommand::CopyCode => return self.copy_last_code(),
            PaletteCommand::SwitchModel => {
                // A recording has no models to choose from, and a running turn has to finish or be stopped first.
                if !self.demo && !self.is_busy() {
                    self.open_welcome();
                }
            }
            PaletteCommand::Extensions => {
                if !self.demo && !self.is_busy() {
                    self.open_welcome();
                    if let Screen::Welcome(welcome) = &mut self.screen {
                        welcome.tab = HomeTab::Mcp;
                    }
                }
            }
            PaletteCommand::TogglePanel => {
                // Session → Files → hidden → Session.
                let showing = self.panel_preference.unwrap_or(crate::view::panel_default(self.terminal_width));
                match (showing, self.panel_tab) {
                    (true, PanelTab::Session) => self.panel_tab = PanelTab::Files,
                    (true, PanelTab::Files) => self.panel_preference = Some(false),
                    (false, _) => {
                        self.panel_preference = Some(true);
                        self.panel_tab = PanelTab::Session;
                    }
                }
            }
            PaletteCommand::ToggleReasoning => self.options.show_reasoning = !self.options.show_reasoning,
            PaletteCommand::ToggleSelectMode => {
                self.select_mode = !self.select_mode;
                return vec![Action::SetMouseCapture(!self.select_mode)];
            }
            PaletteCommand::LastOutput => {
                let last = self.transcript.iter().rev().find_map(|entry| match entry {
                    Entry::Tool(tool) if !tool.output.is_empty() || tool.diff.is_some() => Some(tool),
                    _ => None,
                });
                if let Some(tool) = last {
                    let is_diff = tool.output.is_empty();
                    let text = if is_diff { tool.diff.clone().unwrap_or_default() } else { tool.output.clone() };
                    let title = format!("{} {}", tool.name, tool.target);
                    self.overlay = Some(Overlay::Pager { title, text, is_diff, side_by_side: false, offset: 0 });
                }
            }
            // Files are still changing while a turn runs, so undo waits until it has finished.
            PaletteCommand::Undo => {
                if self.is_busy() {
                    self.toast("Undo is available once this turn finishes (Esc stops it).", ToastLevel::Warning);
                } else {
                    return vec![Action::Undo];
                }
            }
            PaletteCommand::Compact | PaletteCommand::NewConversation | PaletteCommand::ContinueLast => {
                if self.is_busy() {
                    self.toast("That's available once this turn finishes (Esc stops it).", ToastLevel::Warning);
                } else if self.demo {
                    self.toast("That isn't available in the demo recording.", ToastLevel::Info);
                } else {
                    return vec![match command {
                        PaletteCommand::Compact => Action::Compact { focus: None },
                        PaletteCommand::NewConversation => {
                            self.clear_conversation();
                            Action::NewConversation
                        }
                        _ => {
                            self.sessions_wanted = true;
                            Action::ListSessions
                        }
                    }];
                }
            }
            PaletteCommand::TrustProject => return self.trust_project(),
            PaletteCommand::InstallSkill => return self.open_install(None),
            PaletteCommand::InstallPlugin => return self.open_plugin_install(None),
            PaletteCommand::WebSearch => self.open_web_setup(),
            PaletteCommand::FreeModels => {
                if self.demo {
                    self.toast("That isn't available in the demo recording.", ToastLevel::Info);
                } else {
                    self.open_free_setup();
                }
            }
            PaletteCommand::Theme => {
                let selected = crate::theme::names().iter().position(|name| *name == self.theme.name).unwrap_or(0);
                self.overlay = Some(Overlay::Themes { selected, original: self.theme.clone() });
            }
            PaletteCommand::Help => self.overlay = Some(Overlay::Help),
            PaletteCommand::Context => {
                if self.ledger.is_none() && self.context.is_none() {
                    self.toast(
                        "Nothing has been sent yet, so there's nothing in the context to show.",
                        ToastLevel::Info,
                    );
                } else {
                    self.overlay = Some(Overlay::Context);
                }
            }
            PaletteCommand::Quit => return vec![Action::Quit],
        }
        Vec::new()
    }

    /// Handles a mouse wheel or a keyboard scroll.
    /// The wheel over the Files pane scrolls the tree; anywhere else, the conversation.
    pub fn wheel(&mut self, column: u16, row: u16, lines: isize) {
        let over_panel = self.hits.panel.is_some_and(|panel| {
            column >= panel.x && column < panel.x + panel.width && row >= panel.y && row < panel.y + panel.height
        });
        if over_panel && self.panel_tab == PanelTab::Files && self.screen == Screen::Conversation {
            let count = self.tree.rows(&self.project_files).len();
            self.tree.scroll = self.tree.scroll.saturating_add_signed(lines).min(count.saturating_sub(1));
            return;
        }
        self.scroll(lines);
    }

    pub fn scroll(&mut self, lines: isize) {
        let (width, height) = self.view_size;
        if self.overlay.is_none()
            && let Screen::Welcome(welcome) = &mut self.screen
        {
            if welcome.tab == HomeTab::Models {
                if let Some(info) = &self.welcome_info {
                    let matches = welcome.matches(info);
                    welcome.move_selection(&matches, lines.signum());
                }
            } else {
                let count = self.extension_count();
                if let Screen::Welcome(welcome) = &mut self.screen {
                    welcome.move_extension(lines.signum(), count);
                }
            }
            return;
        }
        match &mut self.overlay {
            Some(Overlay::Pager { offset, .. }) => *offset = offset.saturating_add_signed(lines),
            Some(_) => {}
            None => self.transcript.scroll_by(lines, width, height, self.options),
        }
    }

    /// Attaches a clipboard image, or explains why it can't.
    pub fn attach_image(&mut self, image: Option<ImageData>) {
        match image {
            None => self.toast("The clipboard is empty.", ToastLevel::Info),
            Some(_) if !self.model_vision => {
                self.toast("This model can't see images. Switch to a vision model with Ctrl+L.", ToastLevel::Warning);
            }
            Some(image) => self.pending_images.push(image),
        }
    }

    /// Handles pasted text.
    pub fn paste(&mut self, text: &str) {
        if let Some(Overlay::FreeSetup(FreeStep::Key { key, .. })) = &mut self.overlay {
            key.push_str(text.trim());
        } else if let Some(Overlay::Form(form)) = &mut self.overlay {
            form.paste(text);
        } else if self.overlay.is_none() && self.screen == Screen::Conversation {
            self.input.insert(text);
        } else if let (None, Screen::Welcome(welcome)) = (&self.overlay, &mut self.screen)
            && welcome.tab == HomeTab::Models
        {
            let filter = format!("{}{}", welcome.filter, text.trim());
            welcome.set_filter(filter, self.welcome_info.as_ref());
        }
    }
}

/// Who serves a model: its cloud provider, or the local runtime.
fn provider_of(info: &crate::welcome::WelcomeInfo, model: &str) -> String {
    info.models
        .iter()
        .find(|m| m.name == model)
        .and_then(|m| m.provider_label.clone())
        .unwrap_or_else(|| info.provider_name.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use recode_core::ids::TurnId;
    use serde_json::json;

    fn app() -> App {
        App::new(Instant::now(), "p", "m", "x", false)
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn with_pending_approval() -> (App, ApprovalId) {
        let mut app = app();
        let approval = ApprovalId::new();
        app.apply(EventKind::ToolCallRequested {
            call: ToolCall { id: "c".into(), name: "edit".into(), arguments: json!({"path": "a.rs"}) },
        });
        app.apply(EventKind::ApprovalRequired {
            approval,
            call_id: "c".into(),
            description: "Change a.rs".into(),
            diff: Some("-old\n+new\n+another".into()),
            always: Some("always edit in this project".into()),
        });
        app.apply(EventKind::StateChanged { state: AgentState::AwaitingApproval });
        (app, approval)
    }

    #[test]
    fn ctrl_z_asks_for_an_undo_but_not_while_a_turn_is_running() {
        let mut app = app();
        app.screen = Screen::Conversation;
        assert_eq!(app.handle_key(ctrl('z')), [Action::Undo]);

        app.apply(EventKind::StateChanged { state: AgentState::RunningTool { name: "edit".into() } });
        assert!(app.handle_key(ctrl('z')).is_empty(), "files are still changing mid-turn");
        assert!(app.last_toast().is_some_and(|text| text.contains("once this turn finishes")));
    }

    #[test]
    fn a_resumed_session_opens_the_conversation_or_asks_for_a_model() {
        let mut app = app();
        app.transcript.push(Entry::Notice("old".into()));
        app.resumed(Some("qwen3:4b".into()));
        assert_eq!(app.screen, Screen::Conversation);
        assert_eq!(app.model, "qwen3:4b");
        assert!(app.transcript.is_empty(), "the saved conversation replaces what was showing");

        let mut missing = super::tests::app();
        missing.resumed(None);
        assert!(matches!(missing.screen, Screen::Welcome(_)), "pick a model to carry on");
        assert!(missing.home_message.as_ref().is_some_and(|(text, _)| text.contains("isn't available")));
    }

    #[test]
    fn a_new_conversation_clears_the_view_but_not_while_busy() {
        let mut app = app();
        app.transcript.push(Entry::Assistant("earlier".into()));
        assert_eq!(app.run_command(PaletteCommand::NewConversation), [Action::NewConversation]);
        assert!(app.transcript.is_empty());

        app.apply(EventKind::StateChanged { state: AgentState::Responding });
        assert!(app.run_command(PaletteCommand::NewConversation).is_empty());
        assert!(app.run_command(PaletteCommand::Compact).is_empty());
    }

    #[test]
    fn a_compaction_is_reported_with_estimated_sizes() {
        let mut app = app();
        app.apply(EventKind::Compacted {
            before_tokens: 41_200,
            after_tokens: 9_800,
            elided: 23,
            summarised: 0,
            problem: None,
            focus: None,
        });
        let Some(Entry::Notice(text)) = app.transcript.last() else { panic!("no notice") };
        assert!(text.contains("~41k → ~9.8k tokens (estimated)"), "{text}");
        assert!(text.contains("23 old tool results"), "{text}");
    }

    #[test]
    fn always_is_only_accepted_when_it_is_offered() {
        let mut app = app();
        app.apply(EventKind::ToolCallRequested {
            call: ToolCall { id: "s".into(), name: "read".into(), arguments: json!({"path": "~/.ssh/id_ed25519"}) },
        });
        let approval = ApprovalId::new();
        app.apply(EventKind::ApprovalRequired {
            approval,
            call_id: "s".into(),
            description: "Read ~/.ssh/id_ed25519".into(),
            diff: None,
            always: None,
        });
        app.apply(EventKind::StateChanged { state: AgentState::AwaitingApproval });
        assert!(app.handle_key(key(KeyCode::Char('a'))).is_empty(), "secrets are asked every time");
        assert!(app.handle_key(key(KeyCode::Char('p'))).is_empty(), "nor remembered for the project");
        assert_eq!(
            app.handle_key(key(KeyCode::Char('y'))),
            [Action::ResolveApproval(approval, ApprovalDecision::AllowOnce)]
        );
    }

    fn typed(app: &mut App, text: &str) {
        for c in text.chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn a_slash_that_names_a_usable_skill_is_sent_as_a_message() {
        let mut app = app();
        app.screen = Screen::Conversation;
        let mut snapshot = recode_core::extensions::ExtensionsSnapshot::default();
        snapshot.skills.push(recode_core::extensions::Skill {
            name: "greet".into(),
            description: "Greets.".into(),
            path: "/x/greet/SKILL.md".into(),
            enabled: true,
            ..Default::default()
        });
        app.extensions = Some(snapshot);
        typed(&mut app, "/greet hello there");
        app.handle_key(key(KeyCode::Esc));
        let actions = app.handle_key(key(KeyCode::Enter));
        assert!(matches!(&actions[..], [Action::Send { text, .. }] if text == "/greet hello there"), "{actions:?}");
        // A name that isn't a skill is still an unknown command.
        typed(&mut app, "/nope hello");
        app.handle_key(key(KeyCode::Esc));
        assert!(app.handle_key(key(KeyCode::Enter)).is_empty());
        assert!(app.last_toast().is_some_and(|t| t.contains("no /nope command")));
    }

    #[test]
    fn tab_completes_a_file_mention_and_enter_runs_a_complete_command() {
        let mut app = app();
        app.screen = Screen::Conversation;
        app.project_files = vec!["src/main.rs".into(), "src/chat.rs".into()];
        typed(&mut app, "fix @cha");
        assert!(app.completion().is_some());
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.input.text(), "fix @src/chat.rs ");
        assert!(app.completion().is_none(), "the popup closes once the mention is complete");

        let mut app = super::tests::app();
        app.screen = Screen::Conversation;
        typed(&mut app, "/und");
        assert_eq!(app.handle_key(key(KeyCode::Enter)), [Action::Undo], "Enter on /undo runs it");
        assert!(app.input.is_empty());
    }

    #[test]
    fn transcript_mode_searches_and_hands_the_text_to_the_terminal() {
        let mut app = self::tests::app();
        app.screen = Screen::Conversation;
        app.terminal_width = 100;
        app.transcript.push(Entry::User("fix the retry loop".into()));
        app.transcript.push(Entry::Assistant("The retry delay was discarded.\nNow it sleeps.".into()));
        app.transcript.push(Entry::User("thanks".into()));
        app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL));
        assert!(matches!(app.overlay, Some(Overlay::Transcript { .. })));
        app.handle_key(key(KeyCode::Char('/')));
        for c in "retry".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        app.handle_key(key(KeyCode::Enter));
        let Some(Overlay::Transcript { matches, query, offset, .. }) = &app.overlay else { panic!() };
        assert_eq!(query, "retry");
        assert_eq!(matches.len(), 2, "the request and the answer mention it");
        assert_eq!(*offset, matches[0]);
        app.handle_key(key(KeyCode::Char('n')));
        let Some(Overlay::Transcript { offset, matches, current, .. }) = &app.overlay else { panic!() };
        assert_eq!((*current, *offset), (1, matches[1]));
        let actions = app.handle_key(key(KeyCode::Char('[')));
        assert!(
            matches!(&actions[..], [Action::DumpScrollback(text)] if text.contains("fix the retry loop") && text.contains("Now it sleeps.")),
            "{actions:?}"
        );
        app.handle_key(key(KeyCode::Char('q')));
        assert!(app.overlay.is_none());
    }

    #[test]
    fn esc_esc_rewinds_ctrl_r_searches_and_export_renders() {
        let mut app = self::tests::app();
        app.screen = Screen::Conversation;
        app.transcript.push(Entry::User("first request".into()));
        app.transcript.push(Entry::Assistant("ok".into()));
        app.transcript.push(Entry::User("second request".into()));
        app.transcript.push(Entry::Assistant("done".into()));
        // One Esc does nothing; two within a moment open the picker, newest request first.
        assert!(app.handle_key(key(KeyCode::Esc)).is_empty());
        assert!(app.overlay.is_none());
        app.now += Duration::from_millis(100);
        app.handle_key(key(KeyCode::Esc));
        assert!(
            matches!(&app.overlay, Some(Overlay::Rewind { turns, .. }) if turns[0] == (1, "second request".into()))
        );
        app.handle_key(key(KeyCode::Right));
        let actions = app.handle_key(key(KeyCode::Enter));
        assert_eq!(actions, [Action::Rewind { to_turn: 1, files: true, summarize: false }]);
        app.apply(EventKind::Rewound { to_turn: 1, files: vec!["src/a.rs".into()], summary: None });
        assert_eq!(app.transcript.requests(), [(0, "first request".to_owned())]);
        assert!(matches!(app.transcript.last(), Some(Entry::Notice(text)) if text.contains("Put back 1 file")));

        // Ctrl+R searches this project's prompts; Tab asks for every project's.
        app.history = vec!["fix the parser".into(), "add tests for auth".into(), "fix the lexer".into()];
        app.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
        assert!(matches!(app.overlay, Some(Overlay::HistorySearch { .. })));
        for c in "fix".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(app.history_matches("fix", false), ["fix the lexer", "fix the parser"], "newest first");
        assert_eq!(app.handle_key(key(KeyCode::Tab)), [Action::LoadHistory]);
        app.history_all_loaded(vec!["fix everything".into(), "fix the lexer".into()]);
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.input.text(), "fix the lexer");
        assert!(app.overlay.is_none());

        // /export clipboard copies the Markdown.
        app.input.take();
        typed(&mut app, "/export clipboard");
        let actions = app.handle_key(key(KeyCode::Enter));
        assert!(
            matches!(&actions[..], [Action::Copy(text)] if text.contains("## You\n\nfirst request")),
            "{actions:?}"
        );
        typed(&mut app, "/export notes/session.md");
        let actions = app.handle_key(key(KeyCode::Enter));
        assert!(
            matches!(&actions[..], [Action::Export { path: Some(path), .. }] if path == "notes/session.md"),
            "{actions:?}"
        );
    }

    #[test]
    fn typing_while_the_model_works_queues_and_bang_runs_a_command() {
        let mut app = app();
        app.screen = Screen::Conversation;
        // Idle: `!` runs a command without the model; `!!` with network; `\!` is a plain message.
        typed(&mut app, "!cargo --version");
        let actions = app.handle_key(key(KeyCode::Enter));
        assert!(
            matches!(&actions[..], [Action::RunShell { command, network: false, background: false }] if command == "cargo --version"),
            "{actions:?}"
        );
        typed(&mut app, "!!curl -sI https://example.com");
        let actions = app.handle_key(key(KeyCode::Enter));
        assert!(
            matches!(&actions[..], [Action::RunShell { command, network: true, background: false }] if command == "curl -sI https://example.com"),
            "{actions:?}"
        );
        typed(&mut app, "!cargo build --release &");
        let actions = app.handle_key(key(KeyCode::Enter));
        assert!(
            matches!(&actions[..], [Action::RunShell { command, background: true, .. }] if command == "cargo build --release"),
            "{actions:?}"
        );
        typed(&mut app, "\\!bang as a message");
        let sent = app.handle_key(key(KeyCode::Enter));
        assert!(matches!(&sent[..], [Action::Send { text, .. }] if text == "!bang as a message"), "{sent:?}");

        // Busy: Enter queues instead of sending; Backspace on an empty prompt takes it back.
        app.apply(EventKind::StateChanged { state: AgentState::Thinking });
        typed(&mut app, "also update the docs");
        assert!(app.handle_key(key(KeyCode::Enter)).is_empty());
        assert_eq!(app.queue.len(), 1);
        assert!(app.input.is_empty());
        typed(&mut app, "!ls");
        assert!(app.handle_key(key(KeyCode::Enter)).is_empty());
        assert!(app.last_toast().is_some_and(|text| text.contains("once this turn finishes")));
        app.input.take();
        app.handle_key(key(KeyCode::Backspace));
        assert_eq!(app.input.text(), "also update the docs");
        assert!(app.queue.is_empty());
        // Ctrl+Enter queues and interrupts, so the message goes out at once.
        assert_eq!(app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL)), [Action::Interrupt]);
        assert_eq!(app.queue.len(), 1);
        // Delivered mid-turn: shown as the user's message and gone from the queue.
        app.apply(EventKind::QueuedDelivered { text: "also update the docs".into() });
        assert!(app.queue.is_empty());
        assert!(matches!(app.transcript.last(), Some(Entry::User(text)) if text == "also update the docs"));
    }

    #[test]
    fn shift_tab_cycles_the_permission_mode_and_p_remembers_for_the_project() {
        let mut app = app();
        app.screen = Screen::Conversation;
        assert_eq!(app.permission_mode, PermissionMode::Ask);
        let actions = app.handle_key(key(KeyCode::BackTab));
        assert_eq!(actions, [Action::SetPermissionMode(PermissionMode::AcceptEdits)]);
        assert!(app.last_toast().is_some_and(|text| text.contains("accept edits")));
        app.handle_key(key(KeyCode::BackTab));
        assert_eq!(app.permission_mode, PermissionMode::Plan);
        app.handle_key(key(KeyCode::BackTab));
        app.handle_key(key(KeyCode::BackTab));
        assert_eq!(app.permission_mode, PermissionMode::Ask, "the cycle comes round");
        // The driver has the last word (auto mode needs the sandbox).
        app.apply(EventKind::PermissionModeChanged { mode: PermissionMode::AutoSandbox });
        assert_eq!(app.permission_mode, PermissionMode::AutoSandbox);

        let (mut app, approval) = with_pending_approval();
        assert_eq!(
            app.handle_key(key(KeyCode::Char('p'))),
            [Action::ResolveApproval(approval, ApprovalDecision::AllowForProject)]
        );

        // /permissions lists; d forgets the selected project grant.
        let mut app = self::tests::app();
        app.screen = Screen::Conversation;
        typed(&mut app, "/permissions");
        assert_eq!(app.handle_key(key(KeyCode::Enter)), [Action::ListPermissions]);
        app.permissions_listed(
            PermissionMode::Ask,
            vec!["run `cargo test`".into()],
            vec![("{\"kind\":\"project_edits\"}".into(), "edit and create files in this project".into())],
        );
        let actions = app.handle_key(key(KeyCode::Char('d')));
        assert_eq!(actions, [Action::RevokePermission("{\"kind\":\"project_edits\"}".into())]);
        assert!(matches!(&app.overlay, Some(Overlay::Permissions { project, .. }) if project.is_empty()));
    }

    #[test]
    fn a_hash_note_asks_where_it_goes_and_never_reaches_the_model() {
        use crate::link::NoteScope;
        let mut app = app();
        app.screen = Screen::Conversation;
        typed(&mut app, "# prefer small diffs");
        assert!(app.handle_key(key(KeyCode::Enter)).is_empty());
        assert!(matches!(&app.overlay, Some(Overlay::NoteTarget { text, .. }) if text == "prefer small diffs"));
        app.handle_key(key(KeyCode::Down));
        let actions = app.handle_key(key(KeyCode::Enter));
        assert!(
            matches!(&actions[..], [Action::WriteNote { scope: NoteScope::ProjectLocal, text }] if text == "prefer small diffs"),
            "{actions:?}"
        );
        assert!(app.overlay.is_none());
        // Esc brings the text back; a message that must start with # is escaped with a backslash.
        typed(&mut app, "# another");
        app.handle_key(key(KeyCode::Enter));
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.input.text(), "# another");
        app.input.take();
        typed(&mut app, "\\# heading in a message");
        app.handle_key(key(KeyCode::Esc));
        let sent = app.handle_key(key(KeyCode::Enter));
        assert!(matches!(&sent[..], [Action::Send { text, .. }] if text == "# heading in a message"), "{sent:?}");
        // /init previews and confirms; /memory lists.
        typed(&mut app, "/init");
        assert_eq!(app.handle_key(key(KeyCode::Enter)), [Action::Init]);
        app.init_preview("/p/RE-CODE.md".into(), "# p\n\nInstructions.\n".into());
        assert!(matches!(&app.overlay, Some(Overlay::Confirm(c)) if c.title == "Write RE-CODE.md?"));
        assert_eq!(app.handle_key(key(KeyCode::Char('y'))), [Action::WriteInit]);
        typed(&mut app, "/memory");
        assert_eq!(app.handle_key(key(KeyCode::Enter)), [Action::ListInstructions]);
        app.instructions_listed(vec![], "/home/u/.config/re-code/RE-CODE.md".into(), Some("/p/RE-CODE.md".into()));
        assert_eq!(app.handle_key(key(KeyCode::Char('n'))), [Action::Init], "no project file yet: write one");
        app.instructions_listed(vec![], "/home/u/.config/re-code/RE-CODE.md".into(), Some("/p/RE-CODE.md".into()));
        assert_eq!(
            app.handle_key(key(KeyCode::Char('u'))),
            [Action::EditFile("/home/u/.config/re-code/RE-CODE.md".into())]
        );
    }

    #[test]
    fn autocompact_sets_the_trigger_or_the_window_itself() {
        let mut app = app();
        app.screen = Screen::Conversation;
        typed(&mut app, "/autocompact 100k");
        assert!(app.handle_key(key(KeyCode::Enter)).is_empty());
        assert!(app.last_toast().is_some_and(|text| text.contains("Pick a model first")));

        // A local model with a known window: the figure is where compaction starts, capped at the window.
        let setting = WindowSetting::default();
        app.set_context_window(ContextWindow::resolve(&setting, Some(131_072), true, 16384), setting, None);
        typed(&mut app, "/autocompact 8k");
        let actions = app.handle_key(key(KeyCode::Enter));
        assert!(
            matches!(&actions[..], [Action::SetContextWindow { setting, .. }] if setting.trigger == Trigger::Tokens(8000)),
            "{actions:?}"
        );
        typed(&mut app, "/autocompact 1M");
        let actions = app.handle_key(key(KeyCode::Enter));
        assert!(
            matches!(&actions[..], [Action::SetContextWindow { setting, .. }] if setting.trigger == Trigger::Tokens(16384)),
            "{actions:?}"
        );
        assert!(app.last_toast().is_some_and(|text| text.contains("capped at the model's 16k window")));
        typed(&mut app, "/autocompact off");
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.context_setting.trigger, Trigger::Off);

        // A cloud model with no listed limit: the size given becomes its window.
        let setting = WindowSetting::default();
        app.set_context_window(ContextWindow::resolve(&setting, None, false, 16384), setting, None);
        typed(&mut app, "/autocompact 128k");
        let actions = app.handle_key(key(KeyCode::Enter));
        assert!(
            matches!(&actions[..], [Action::SetContextWindow { setting, .. }] if setting.size == Size::Tokens(128_000)),
            "{actions:?}"
        );
        assert_eq!(app.context_window.as_ref().unwrap().window, Some(128_000));
        typed(&mut app, "/autocompact lots");
        app.handle_key(key(KeyCode::Enter));
        assert!(app.last_toast().is_some_and(|text| text.contains("isn't a size")));
    }

    #[test]
    fn slash_commands_run_locally_and_never_reach_the_model() {
        let mut app = app();
        app.screen = Screen::Conversation;
        typed(&mut app, "/add-dir ~/notes");
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.handle_key(key(KeyCode::Enter)), [Action::AddFolder("~/notes".into())]);

        typed(&mut app, "/nonsense");
        assert!(app.handle_key(key(KeyCode::Enter)).is_empty());
        assert!(app.last_toast().is_some_and(|text| text.contains("no /nonsense")));

        typed(&mut app, "//etc is a folder");
        app.handle_key(key(KeyCode::Esc));
        let sent = app.handle_key(key(KeyCode::Enter));
        assert!(matches!(&sent[..], [Action::Send { text, .. }] if text == "/etc is a folder"), "{sent:?}");
    }

    #[test]
    fn up_on_an_empty_prompt_walks_back_through_history_and_down_returns() {
        let mut app = app();
        app.screen = Screen::Conversation;
        app.history = vec!["first".into(), "second".into()];
        app.handle_key(key(KeyCode::Up));
        assert_eq!(app.input.text(), "second");
        app.handle_key(key(KeyCode::Up));
        assert_eq!(app.input.text(), "first");
        app.handle_key(key(KeyCode::Up));
        assert_eq!(app.input.text(), "first", "stays at the oldest");
        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Down));
        assert_eq!(app.input.text(), "", "back to what was being typed");

        typed(&mut app, "third");
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.history.last().map(String::as_str), Some("third"), "sent prompts join the history");
    }

    #[test]
    fn continue_lists_sessions_and_enter_picks_one() {
        let mut app = app();
        app.screen = Screen::Conversation;
        typed(&mut app, "/continue");
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.handle_key(key(KeyCode::Enter)), [Action::ListSessions]);
        let session = |id: &str, title: &str| crate::link::SessionSummary {
            id: id.into(),
            title: title.into(),
            age: "2 hours ago".into(),
            messages: 6,
        };
        app.show_sessions(vec![session("a", "fix the login"), session("b", "add tests")]);
        app.handle_key(key(KeyCode::Down));
        assert_eq!(app.handle_key(key(KeyCode::Enter)), [Action::ContinueSession("b".into())]);
        assert!(app.overlay.is_none());

        app.show_sessions(Vec::new());
        assert!(app.last_toast().is_some_and(|text| text.contains("no saved sessions")));
    }

    #[test]
    fn the_theme_picker_previews_restores_and_saves() {
        let mut app = app();
        app.screen = Screen::Conversation;
        let start = app.theme.clone();
        typed(&mut app, "/theme");
        app.handle_key(key(KeyCode::Esc));
        app.handle_key(key(KeyCode::Enter));
        assert!(matches!(app.overlay, Some(Overlay::Themes { .. })));
        app.handle_key(key(KeyCode::Down));
        assert_ne!(app.theme.name, start.name, "moving previews the next theme");
        app.handle_key(key(KeyCode::Right));
        assert_eq!(app.theme.variant, crate::theme::Variant::Bright, "→ brightens");
        app.handle_key(key(KeyCode::Char('a')));
        assert!(app.theme.accent.is_some(), "a picks an accent");
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.theme, start, "Esc puts everything back");

        app.run_command(PaletteCommand::Theme);
        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Char('b')));
        assert!(app.theme.solid);
        let names = crate::theme::names();
        let expected = ThemeSelection { name: names[2].clone(), solid: true, ..ThemeSelection::default() };
        assert_eq!(app.handle_key(key(KeyCode::Enter)), [Action::SetTheme(expected.clone())]);
        assert_eq!(app.theme, expected);

        // A saved choice is applied at startup, unless config.toml names a theme.
        let mut fresh = super::tests::app();
        fresh.saved_theme(ThemeSelection::named("nord"));
        assert_eq!(fresh.theme.name, "Nord");
        let mut locked = super::tests::app();
        locked.theme_locked = true;
        locked.saved_theme(ThemeSelection::named("Nord"));
        assert_ne!(locked.theme.name, "Nord");
    }

    #[test]
    fn every_code_block_is_found_and_ctrl_y_asks_when_there_are_several() {
        let text = "Try:\n```rust\nfn a() {}\n```\nor\n~~~sh\ncargo test\n~~~\n";
        let blocks = code_blocks(text);
        assert_eq!(blocks, [("rust".to_owned(), "fn a() {}".to_owned()), ("sh".to_owned(), "cargo test".to_owned())]);
        let mut app = app();
        app.screen = Screen::Conversation;
        app.transcript.push(Entry::Assistant(text.into()));
        assert!(app.handle_key(ctrl('y')).is_empty());
        assert!(matches!(app.overlay, Some(Overlay::CodeBlocks { .. })), "two blocks: ask which");
        assert_eq!(app.handle_key(key(KeyCode::Char('2'))), [Action::Copy("cargo test".into())]);
        assert!(app.overlay.is_none());
        // One block copies straight away.
        app.transcript.push(Entry::Assistant("```py\nprint(1)\n```".into()));
        assert_eq!(app.handle_key(ctrl('y')), [Action::Copy("print(1)".into())]);
    }

    #[test]
    fn the_diff_pager_jumps_between_files_and_opens_the_current_one() {
        let diff = "--- a/one.rs\n+++ b/one.rs\n@@ -1 +1 @@\n-a\n+b\n--- a/two.rs\n+++ b/two.rs\n@@ -1 +1 @@\n-c\n+d\n";
        let starts = diff_file_starts(diff, 80, false);
        assert_eq!(starts.iter().map(|(_, path)| path.as_str()).collect::<Vec<_>>(), ["one.rs", "two.rs"]);
        assert!(starts[1].0 > starts[0].0);
        let mut app = app();
        app.screen = Screen::Conversation;
        app.terminal_width = 84;
        app.transcript.push(Entry::Tool(ToolEntry {
            id: "t".into(),
            name: "edit".into(),
            target: "one.rs".into(),
            status: ToolStatus::Succeeded { summary: "edited".into() },
            output: String::new(),
            diff: Some(diff.into()),
            elapsed: None,
        }));
        typed(&mut app, "/diff");
        app.handle_key(key(KeyCode::Esc));
        app.handle_key(key(KeyCode::Enter));
        assert!(matches!(&app.overlay, Some(Overlay::Pager { is_diff: true, .. })));
        app.handle_key(key(KeyCode::Char('n')));
        let Some(Overlay::Pager { offset, .. }) = &app.overlay else { panic!() };
        assert_eq!(*offset, starts[1].0, "n goes to the second file");
        assert_eq!(app.handle_key(key(KeyCode::Char('e'))), [Action::EditFile("two.rs".into())]);
    }

    #[test]
    fn output_is_sanitised_on_the_way_in() {
        let mut app = app();
        app.apply(EventKind::TextDelta { text: "ok\x1b]52;c;ZXZpbA==\x07\x1b[2J".into() });
        assert_eq!(app.transcript.last(), Some(&Entry::Assistant("ok".into())));
    }

    #[test]
    fn approval_keys_answer_only_when_the_input_is_empty() {
        let (mut app, approval) = with_pending_approval();
        assert_eq!(
            app.handle_key(key(KeyCode::Char('y'))),
            [Action::ResolveApproval(approval, ApprovalDecision::AllowOnce)]
        );

        let (mut app, _) = with_pending_approval();
        app.input.insert("hm");
        assert!(app.handle_key(key(KeyCode::Char('y'))).is_empty());
        assert_eq!(app.input.text(), "hmy");
    }

    #[test]
    fn full_diff_opens_and_closes() {
        let (mut app, _) = with_pending_approval();
        app.handle_key(key(KeyCode::Char('d')));
        assert!(matches!(app.overlay, Some(Overlay::Pager { is_diff: true, .. })));
        app.handle_key(key(KeyCode::Esc));
        assert!(app.overlay.is_none());
        assert!(app.pending_approval().is_some());
    }

    #[test]
    fn escape_interrupts_only_when_busy() {
        let mut app = app();
        assert!(app.handle_key(key(KeyCode::Esc)).is_empty());
        app.apply(EventKind::StateChanged { state: AgentState::Responding });
        assert_eq!(app.handle_key(key(KeyCode::Esc)), [Action::Interrupt]);
    }

    #[test]
    fn a_finished_turn_leaves_nothing_claiming_to_run() {
        let (mut app, _) = with_pending_approval();
        app.apply(EventKind::TurnFinished { turn: TurnId::new(), outcome: TurnOutcome::Interrupted });
        assert_eq!(app.state, AgentState::Idle);
        assert!(app.pending_approval().is_none());
        assert!(
            app.transcript
                .iter()
                .any(|entry| matches!(entry, Entry::Tool(ToolEntry { status: ToolStatus::Interrupted, .. })))
        );
    }

    #[test]
    fn denied_tools_stay_denied_and_do_not_count_as_changes() {
        let (mut app, approval) = with_pending_approval();
        app.apply(EventKind::ApprovalResolved { approval, decision: ApprovalDecision::Deny });
        app.apply(EventKind::ToolFinished { call_id: "c".into(), success: false, summary: "x".into() });
        assert!(app.files.is_empty());
        assert!(
            app.transcript
                .iter()
                .any(|entry| matches!(entry, Entry::Tool(ToolEntry { status: ToolStatus::Denied, .. })))
        );
    }

    /// One turn that edits a.rs through an approved change with the given diff.
    fn edit_turn(app: &mut App, id: &str, diff: &str) {
        app.apply(EventKind::TurnStarted { turn: TurnId::new(), text: "change it".into() });
        app.apply(EventKind::ToolCallRequested {
            call: ToolCall { id: id.into(), name: "edit".into(), arguments: json!({"path": "a.rs"}) },
        });
        let approval = ApprovalId::new();
        app.apply(EventKind::ApprovalRequired {
            approval,
            call_id: id.into(),
            description: "Change a.rs".into(),
            diff: Some(diff.into()),
            always: None,
        });
        app.apply(EventKind::ApprovalResolved { approval, decision: ApprovalDecision::AllowOnce });
        app.apply(EventKind::ToolFinished { call_id: id.into(), success: true, summary: "ok".into() });
        app.apply(EventKind::TurnFinished { turn: TurnId::new(), outcome: TurnOutcome::Completed });
    }

    #[test]
    fn undoing_a_turn_takes_back_only_that_turns_changes() {
        let mut app = app();
        edit_turn(&mut app, "c1", "-old\n+new");
        edit_turn(&mut app, "c2", "-x\n+y\n+z");
        assert_eq!(app.files, [ChangedFile { path: "a.rs".into(), added: 3, removed: 2 }]);

        app.apply(EventKind::Undone { summary: "Undid it.".into(), paths: vec!["a.rs".into()] });
        assert_eq!(
            app.files,
            [ChangedFile { path: "a.rs".into(), added: 1, removed: 1 }],
            "the first turn's change stays"
        );
        assert_eq!(app.transcript.last(), Some(&Entry::Notice("Undid it.".into())));

        app.apply(EventKind::Undone { summary: "Undid more.".into(), paths: vec!["a.rs".into()] });
        assert!(app.files.is_empty());
    }

    #[test]
    fn approved_edits_are_counted() {
        let (mut app, approval) = with_pending_approval();
        app.apply(EventKind::ApprovalResolved { approval, decision: ApprovalDecision::AllowOnce });
        app.apply(EventKind::ToolFinished { call_id: "c".into(), success: true, summary: "ok".into() });
        assert_eq!(app.files, [ChangedFile { path: "a.rs".into(), added: 2, removed: 1 }]);
    }

    #[test]
    fn control_c_clears_then_arms_then_quits() {
        let mut app = app();
        app.input.insert("draft");
        assert!(app.handle_key(ctrl('c')).is_empty());
        assert!(app.input.is_empty());
        assert!(app.handle_key(ctrl('c')).is_empty());
        assert!(app.quit_armed);
        assert_eq!(app.handle_key(ctrl('c')), [Action::Quit]);
    }

    #[test]
    fn any_other_key_disarms_quit() {
        let mut app = app();
        app.handle_key(ctrl('c'));
        app.handle_key(key(KeyCode::Left));
        assert!(!app.quit_armed);
        assert!(app.handle_key(ctrl('c')).is_empty());
    }

    #[test]
    fn enter_sends_and_modified_enter_makes_a_newline() {
        let mut app = app();
        app.input.insert("a");
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        app.handle_key(ctrl('j'));
        assert_eq!(app.input.text(), "a\n\n");
        assert_eq!(app.handle_key(key(KeyCode::Enter)), [Action::Send { text: "a\n\n".into(), images: vec![] }]);
        assert!(app.handle_key(key(KeyCode::Enter)).is_empty(), "empty input must not send");
    }

    #[test]
    fn palette_filters_and_runs() {
        assert_eq!(palette_matches("rson").first().map(|item| item.0), Some(PaletteCommand::ToggleReasoning));
        assert!(palette_matches("zzz").is_empty());
        let mut app = app();
        app.handle_key(ctrl('k'));
        for c in "panel".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        app.handle_key(key(KeyCode::Enter));
        assert!(app.overlay.is_none());
        assert!(app.panel_preference.is_some());
    }

    #[test]
    fn the_last_code_block_is_extracted_or_the_whole_answer_falls_through() {
        let text = "Intro\n\n```rust\nfn a() {}\n```\n\nmiddle\n\n```\nsecond block\nline two\n```\nend";
        assert_eq!(last_code_block(text).as_deref(), Some("second block\nline two"));
        assert_eq!(last_code_block("no code here"), None);
        // An unterminated fence (streaming) still yields its contents.
        assert_eq!(last_code_block("```\nopen code").as_deref(), Some("open code"));
    }

    #[test]
    fn copying_with_no_answer_does_not_panic() {
        let mut app = app();
        assert!(app.copy_last_code().is_empty());
    }

    #[test]
    fn copying_returns_the_code_and_notes_it() {
        let mut app = app();
        app.transcript.push(Entry::Assistant("here:\n\n```\nx = 1\n```".into()));
        let actions = app.copy_last_code();
        assert_eq!(actions, [Action::Copy("x = 1".into())]);
        assert!(app.last_toast().is_some_and(|text| text.contains("Copied")));
    }

    #[test]
    fn clicking_a_reasoning_row_toggles_that_block() {
        let mut app = app();
        app.screen = Screen::Conversation;
        app.transcript.push(Entry::Reasoning("a\nb\nc".into()));
        // Pretend the reasoning header was drawn at row 5 for entry 0.
        app.hits.lines = vec![(5, 0, 60, 0)];
        app.click(70, 5);
        assert!(
            !app.transcript.visible_mapped(60, 20, app.options).0.iter().any(|l| l.to_string().contains('▾')),
            "outside the transcript"
        );
        app.click(2, 5);
        let expanded = app.transcript.visible_mapped(60, 20, app.options).0;
        let text: String = expanded.iter().map(ToString::to_string).collect();
        assert!(text.contains('▾'), "block should be expanded: {text}");
    }

    #[test]
    fn images_attach_only_for_vision_models() {
        let image = || recode_core::message::ImageData {
            media_type: "image/png".into(),
            data_base64: "AAAA".into(),
            label: "x".into(),
        };
        let mut app = app();
        app.model_vision = false;
        app.attach_image(Some(image()));
        assert!(app.pending_images.is_empty());
        assert!(app.last_toast().is_some_and(|text| text.contains("can't see images")));
        app.model_vision = true;
        app.attach_image(Some(image()));
        assert_eq!(app.pending_images.len(), 1);
        app.attach_image(None);
        assert_eq!(app.pending_images.len(), 1);
    }

    #[test]
    fn select_mode_releases_and_retakes_the_mouse() {
        let mut app = app();
        assert_eq!(app.handle_key(key(KeyCode::F(2))), [Action::SetMouseCapture(false)]);
        assert_eq!(app.handle_key(key(KeyCode::F(2))), [Action::SetMouseCapture(true)]);
    }
}
