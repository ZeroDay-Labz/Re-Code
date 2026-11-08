//! A real conversation with a local model, driving the interface. It has no tools yet, so a model can
//! talk but can't read or change anything. This becomes the `engine` crate along with the agent loop.

use crate::compact;
use crate::doctor::hardware::GpuVendor;
use crate::doctor::runtime::{Installation, OllamaStatus};
use crate::doctor::{self, Report};
use crate::paths::Paths;
use crate::permissions::{Decision, Grant, Permissions, Reach, reach, tool_path};
use crate::secrets::{Keyring, SecretStore};
use crate::session::{self, Recorder, SavedConversation};
use futures_util::StreamExt;
use recode_core::context::{ContextWindow, WindowSetting};
use recode_core::extensions::{CustomTool, Skill};
use recode_core::extensions::{ExtensionCommand, ExtensionsSnapshot, InstallOutcome, McpProbe, PluginOutcome};
use recode_core::extensions::{McpServer, ProviderProbe};
use recode_core::ids::{ApprovalId, TurnId};
use recode_core::message::{ImageData, Message};
use recode_core::message::{ToolCall, ToolDefinition};
use recode_core::protocol::{AgentState, ApprovalDecision, EventKind, PermissionMode, TurnOutcome};
use recode_core::providers::{CloudProvider, ProviderKind};
use recode_core::usage::ComponentKind;
use recode_core::usage::TokenCount;
use recode_extensions::catalog::Catalog;
use recode_extensions::fetch::{Bases, Fetcher};
use recode_extensions::hub::McpHub;
use recode_extensions::plugins::PluginHub;
use recode_extensions::skillhub::Hub;
use recode_extensions::{Extensions, Locations};
use recode_providers::anthropic::AnthropicProvider;
use recode_providers::ollama::{OllamaOptions, OllamaProvider};
use recode_providers::openai::OpenAiProvider;
use recode_providers::{ChatRequest, FinishReason, ModelInfo, Provider, ProviderOptions, StreamEvent};
use recode_store::Store;
use recode_tools::sandbox::{self, Policy};
use recode_tools::{Checkpoints, Workspace, target_of, tool_definitions};
use recode_tui::link::{
    ContainerRow, PodRow, ProjectTrust, RuntimeRow, RuntimeRowState, RuntimeStartEvent, RuntimesInfo,
};
use recode_tui::link::{Link, UiCommand, UiEvent};
use recode_tui::welcome::{GpuFit, Meter, ProviderState, WelcomeInfo, WelcomeModel, sort_models};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio_util::sync::CancellationToken;

const SYSTEM_PROMPT: &str = "You are the assistant in re-code, a terminal coding tool. You have no tools in this \
session: you can't read files, run commands or change anything, so never claim that you did. If you need to see \
something, ask the user to paste it. Be concise.";

const AGENT_SYSTEM_PROMPT: &str = "You are re-code, a coding agent running on the user's own computer, started in their project folder. You have tools: read, list, grep, write, edit, delete, shell and git, plus any MCP servers listed below. Use them to inspect and change things yourself; never ask the user to paste file contents. You can reach files anywhere on this computer: paths are relative to the project folder, or absolute or ~/... for anything else. Reading inside the project happens straight away; for anything outside it, or any change, the user is asked to approve, so just make the call rather than saying you can't. To change a file, use edit (give the exact existing text as 'old') or write. shell runs commands in a sandbox: the project folder (and folders the user allowed) can be written, the rest of the system is read-only, and network is off unless you set network to true. git runs read-only git commands in the project. For a job with several steps, keep the todo tool's list up to date so the user can follow along. Work in small steps, and give a short summary when you're done.";

const CHAT_ONLY_NOTICE: &str = "Chat only: this model can talk, but it can't read or change your files here (the Tools line in the side panel says why). The conversation is saved; re-code --continue picks it up later. Ctrl+L switches model; Ctrl+E manages providers, MCP servers, skills and tools.";

const LAST_MODEL_KEY: &str = "last_model";
const THEME_KEY: &str = "theme";
const THEME_SOLID_KEY: &str = "theme_solid";
const THEME_VARIANT_KEY: &str = "theme_variant";
const THEME_ACCENT_KEY: &str = "theme_accent";
const PROBE_KEY_PREFIX: &str = "mcp_probe:";
const PROVIDER_PROBE_PREFIX: &str = "provider_probe:";
const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
/// Room left for what a loaded model needs beyond its weights. Consistent with the 8 models measured on 2026-11-06,
/// where this overhead varied, so results stay labelled as estimates.
const OVERHEAD_GIB: f64 = 0.75;

#[allow(clippy::cast_precision_loss)]
fn gib(bytes: u64) -> f64 {
    bytes as f64 / GIB
}

/// Starts the session task and returns the interface's end of the connection.
/// With `resume`, the last session in this folder is continued.
/// `allowed` folders are open to the agent for the whole session.
pub fn spawn(
    paths: Paths,
    url: String,
    num_ctx: u32,
    resume: bool,
    allowed: Vec<std::path::PathBuf>,
    max_steps: Option<usize>,
    memory_off: bool,
) -> Link {
    let (events, event_receiver) = mpsc::unbounded_channel();
    let (commands, command_receiver) = mpsc::unbounded_channel();
    tokio::spawn(drive(paths, url, num_ctx, resume, allowed, max_steps, memory_off, events, command_receiver));
    Link { events: event_receiver, commands }
}

pub(crate) fn locations(paths: &Paths) -> Locations {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    Locations {
        config_file: paths.extensions_file.clone(),
        user_skills: paths.skills_dir.clone(),
        project_skills: std::env::current_dir().ok().map(|dir| dir.join(".re-code/skills")),
        claude_config: home.as_ref().map(|home| home.join(".claude.json")),
        project: std::env::current_dir().ok(),
        claude_skills: home.as_ref().map(|home| home.join(".claude/skills")),
        claude_plugins: home.map(|home| home.join(".claude/plugins")),
        plugins_dir: paths.data_dir.join("plugins"),
        user_config_root: Some(paths.user_config_root.clone()),
    }
}

/// What the management screen needs, beyond what is stored in the file.
struct Manager {
    places: Locations,
    extensions: Extensions,
    probes: BTreeMap<String, McpProbe>,
    probing: Vec<String>,
    provider_probes: BTreeMap<String, ProviderProbe>,
    providers_with_keys: BTreeSet<String>,
    store: Option<Store>,
}

/// Whether the project folder's current configuration is trusted.
fn project_trusted(places: &Locations, store: Option<&Store>) -> bool {
    places.project.as_ref().is_some_and(|project| {
        let config = crate::trust::project_config(project);
        crate::trust::is_trusted(store, project, &config)
    })
}

/// What `/trust` would switch on, for the interface.
fn project_trust_event(manager: &Manager) -> UiEvent {
    let (folder, config, trusted) = match &manager.places.project {
        Some(project) => {
            let config = crate::trust::project_config(project);
            let trusted = crate::trust::is_trusted(manager.store.as_ref(), project, &config);
            (project.display().to_string(), config, trusted)
        }
        None => (String::new(), crate::trust::project_config(std::path::Path::new("/nonexistent")), false),
    };
    UiEvent::ProjectTrust(ProjectTrust { folder, trusted, skills: config.skills, files: config.files })
}

impl Manager {
    /// Re-reads the project folder's trust after a reload or a change.
    fn refresh_trust(&mut self) {
        let trusted = project_trusted(&self.places, self.store.as_ref());
        self.extensions.set_project_trusted(trusted);
    }

    fn load(places: Locations, store: Option<Store>) -> Self {
        let mut extensions = Extensions::load(places.clone());
        extensions.set_project_trusted(project_trusted(&places, store.as_ref()));
        let mut manager = Self {
            places,
            extensions,
            probes: BTreeMap::new(),
            probing: Vec::new(),
            provider_probes: BTreeMap::new(),
            providers_with_keys: BTreeSet::new(),
            store,
        };
        manager.load_probes();
        manager
    }

    /// Results of earlier tests are kept, so opening the screen doesn't start or contact anything.
    fn load_probes(&mut self) {
        let Some(store) = &self.store else { return };
        self.probes = self
            .extensions
            .snapshot()
            .servers
            .iter()
            .filter_map(|server| {
                let text = store.setting(&format!("{PROBE_KEY_PREFIX}{}", server.name)).ok().flatten()?;
                Some((server.name.clone(), serde_json::from_str(&text).ok()?))
            })
            .collect();
        self.provider_probes = self
            .extensions
            .snapshot()
            .providers
            .iter()
            .filter_map(|provider| {
                let text = store.setting(&format!("{PROVIDER_PROBE_PREFIX}{}", provider.name)).ok().flatten()?;
                Some((provider.name.clone(), serde_json::from_str(&text).ok()?))
            })
            .collect();
    }

    fn reload(&mut self) {
        self.extensions = Extensions::load(self.places.clone());
        self.load_probes();
    }

    fn snapshot(&self) -> ExtensionsSnapshot {
        ExtensionsSnapshot {
            probes: self.probes.clone(),
            probing: self.probing.clone(),
            provider_probes: self.provider_probes.clone(),
            providers_with_keys: self.providers_with_keys.clone(),
            ..self.extensions.snapshot()
        }
    }

    fn forget_probe(&mut self, name: &str) {
        self.probes.remove(name);
        if let Some(store) = &self.store
            && let Err(error) = store.delete_setting(&format!("{PROBE_KEY_PREFIX}{name}"))
        {
            tracing::warn!(%error, "can't forget an MCP test result");
        }
    }

    fn record_probe(&mut self, name: String, probe: McpProbe) {
        self.probing.retain(|existing| *existing != name);
        if let Some(store) = &self.store
            && let Ok(text) = serde_json::to_string(&probe)
            && let Err(error) = store.set_setting(&format!("{PROBE_KEY_PREFIX}{name}"), &text)
        {
            tracing::warn!(%error, "can't remember an MCP test result");
        }
        self.probes.insert(name, probe);
    }

    fn forget_provider_probe(&mut self, name: &str) {
        self.provider_probes.remove(name);
        if let Some(store) = &self.store {
            let _ = store.delete_setting(&format!("{PROVIDER_PROBE_PREFIX}{name}"));
        }
    }

    fn record_provider_probe(&mut self, name: String, probe: ProviderProbe) {
        if let Some(store) = &self.store
            && let Ok(text) = serde_json::to_string(&probe)
        {
            let _ = store.set_setting(&format!("{PROVIDER_PROBE_PREFIX}{name}"), &text);
        }
        self.provider_probes.insert(name, probe);
    }

    /// Applies a change, returning a message for the user. Testing is handled by the caller, as it runs in the background.
    fn apply(&mut self, command: ExtensionCommand) -> Result<String, String> {
        let done = |result: Result<(), recode_extensions::ExtensionError>, message: String| {
            result.map(|()| message).map_err(|error| error.to_string())
        };
        match command {
            ExtensionCommand::SaveServer { original, server } => {
                let name = server.name.clone();
                done(self.extensions.save_server(original.as_deref(), server), format!("Saved {name}.")).inspect(|_| {
                    // The connection may have changed, so an earlier result no longer says anything about it.
                    self.forget_probe(original.as_deref().unwrap_or(&name));
                    self.forget_probe(&name);
                })
            }
            ExtensionCommand::DeleteServer(name) => {
                self.forget_probe(&name);
                done(self.extensions.delete_server(&name), format!("Deleted {name}."))
            }
            ExtensionCommand::SetServerEnabled(name, enabled) => {
                let state = if enabled { "on" } else { "off" };
                done(self.extensions.set_server_enabled(&name, enabled), format!("{name} is {state}."))
            }
            ExtensionCommand::SetServerExposed(name, exposed) => {
                let how = if exposed {
                    "exposed directly: its tools go straight to the model"
                } else {
                    "discover-then-call: the model lists its tools before using them"
                };
                done(self.extensions.set_server_exposed(&name, exposed), format!("{name} is {how}."))
            }
            ExtensionCommand::SaveTool { original, tool } => {
                let name = tool.name.clone();
                done(self.extensions.save_tool(original.as_deref(), tool), format!("Saved {name}."))
            }
            ExtensionCommand::DeleteTool(name) => done(self.extensions.delete_tool(&name), format!("Deleted {name}.")),
            ExtensionCommand::SetToolEnabled(name, enabled) => {
                let state = if enabled { "on" } else { "off" };
                done(self.extensions.set_tool_enabled(&name, enabled), format!("{name} is {state}."))
            }
            ExtensionCommand::CreateSkill { name, description } => self
                .extensions
                .create_skill(&name, &description)
                .map(|_| format!("Created {name}. Press e to write its instructions."))
                .map_err(|error| error.to_string()),
            ExtensionCommand::DeleteSkill(name) => {
                done(self.extensions.delete_skill(&name), format!("Deleted {name}."))
            }
            ExtensionCommand::SetSkillEnabled(name, enabled) => {
                let state = if enabled { "on" } else { "off" };
                done(self.extensions.set_skill_enabled(&name, enabled), format!("{name} is {state}."))
            }
            ExtensionCommand::SetToolFilter { server, tools } => {
                let how = match &tools {
                    None => "offers all its tools".to_owned(),
                    Some(filter) if !filter.include.is_empty() => {
                        format!("offers {} of its tools", filter.include.len())
                    }
                    Some(filter) => format!("hides {} of its tools", filter.exclude.len()),
                };
                done(self.extensions.set_tool_filter(&server, tools), format!("{server} {how}."))
            }
            ExtensionCommand::TestServer(_) | ExtensionCommand::SearchMcp(_) => Ok(String::new()),
            ExtensionCommand::SaveProvider { .. }
            | ExtensionCommand::DeleteProvider(_)
            | ExtensionCommand::SetProviderEnabled(..)
            | ExtensionCommand::TestProvider(_)
            | ExtensionCommand::ImportClaudeMcp
            | ExtensionCommand::ImportOpenCode
            | ExtensionCommand::AddMcpFromLink { .. }
            | ExtensionCommand::StageSkill(_)
            | ExtensionCommand::CommitInstall { .. }
            | ExtensionCommand::AcceptScan(_)
            | ExtensionCommand::DiscardInstall(_)
            | ExtensionCommand::UpdateSkill(_)
            | ExtensionCommand::BrowseSkills { .. }
            | ExtensionCommand::StagePlugin(_)
            | ExtensionCommand::StageSkillBatch(_)
            | ExtensionCommand::StageSkillText(_)
            | ExtensionCommand::CommitInstallBatch(_)
            | ExtensionCommand::StagePluginBatch(_)
            | ExtensionCommand::CommitPluginBatch(_)
            | ExtensionCommand::CommitPlugin { .. }
            | ExtensionCommand::SetPluginEnabled(..)
            | ExtensionCommand::DeletePlugin(_)
            | ExtensionCommand::UpdatePlugin(_)
            | ExtensionCommand::AddMarketplace(_)
            | ExtensionCommand::RemoveMarketplace(_)
            | ExtensionCommand::BrowseMarketplace(_)
            | ExtensionCommand::SearchPlugins(_) => {
                unreachable!("provider, import and install commands are handled in the driver")
            }
        }
    }
}

/// How to reach the model the user picked.
#[derive(Clone)]
enum Route {
    Ollama,
    Cloud { provider: CloudProvider, key: Option<String> },
}

/// Builds the provider for a route, and whether it's a local model (whose context size is ours to set).
fn build_provider(route: &Route, url: &str) -> Result<(Box<dyn Provider>, bool), String> {
    match route {
        Route::Ollama => {
            let provider = OllamaProvider::new(url).map_err(|error| error.to_string())?;
            Ok((Box::new(provider), true))
        }
        Route::Cloud { provider, key } => Ok((build_cloud(provider, key.clone())?, false)),
    }
}

/// After re-code itself changed a project file at the user's request, a trusted folder stays trusted:
/// the fingerprint moves with the edit the user asked for.
fn keep_trust_after_own_edit(
    manager: &mut Manager,
    project: &std::path::Path,
    was_trusted: bool,
    events: &UnboundedSender<UiEvent>,
) {
    if was_trusted && let Some(store) = &manager.store {
        let config = crate::trust::project_config(project);
        let _ = crate::trust::trust(store, project, &config);
    }
    manager.refresh_trust();
    let _ = events.send(project_trust_event(manager));
}

/// Loads the instruction files for this request: tells the user once about project files held back
/// until the folder is trusted and about oversized ones, and notes a change since the last request
/// (the prompt cache rebuilds once). The files themselves ride in the system prompt.
fn refresh_instructions(
    chat: &mut Chat,
    manager: &Manager,
    paths: &Paths,
    events: &UnboundedSender<UiEvent>,
    notified: &mut bool,
) {
    let trusted = project_trusted(&manager.places, manager.store.as_ref());
    let user_dir = paths.config_file.parent().map(std::path::Path::to_path_buf).unwrap_or_default();
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let loaded = crate::instructions::load(manager.places.project.as_deref(), &user_dir, home.as_deref(), trusted);
    if !*notified {
        *notified = true;
        let held: Vec<String> = loaded
            .untrusted()
            .iter()
            .map(|file| {
                let name = file.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                format!("{name} (~{} tokens)", file.tokens)
            })
            .collect();
        if !held.is_empty() {
            let _ = events.send(UiEvent::Notice(format!(
                "This folder has instructions that stay off until you trust it: {}. /trust shows what would load.",
                held.join(", ")
            )));
        }
        for file in loaded.oversized() {
            let text = match &file.problem {
                Some(problem) => format!("{} {problem}.", file.path.display()),
                None => format!(
                    "{} costs about {} tokens on every request; shorter instructions leave more room for the work.",
                    file.path.display(),
                    file.tokens
                ),
            };
            let _ = events.send(UiEvent::Notice(text));
        }
    }
    if chat.instructions_hash != 0 && chat.instructions_hash != loaded.hash {
        let _ = events.send(UiEvent::Notice(
            "Instruction files changed on disk; they're used from this message on (the prompt cache rebuilds once)."
                .to_owned(),
        ));
    }
    chat.instructions_hash = loaded.hash;
    chat.instructions = loaded.part();
    chat.compact_instructions = loaded.compact_instructions();
}

/// `re-code-export-20261107-1432.md`, from the clock, without a date crate: Howard Hinnant's civil-from-days.
fn export_file_name(now: recode_core::ids::UnixMillis) -> String {
    let seconds = now.0.div_euclid(1000);
    let days = seconds.div_euclid(86_400);
    let rem = seconds.rem_euclid(86_400);
    let (hour, minute) = (rem / 3600, rem % 3600 / 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!("re-code-export-{year:04}{month:02}{day:02}-{hour:02}{minute:02}.md")
}

/// The user's and the project's hooks as they are now (the project's only once trusted).
fn load_hooks(manager: &Manager, paths: &Paths) -> crate::hooks::Hooks {
    let trusted = project_trusted(&manager.places, manager.store.as_ref());
    let user_dir = paths.config_file.parent().map(std::path::Path::to_path_buf).unwrap_or_default();
    crate::hooks::load(&user_dir, manager.places.project.as_deref(), trusted)
}

/// The grants remembered for a project, from the database.
fn project_grants(store: Option<&Store>, project_key: &str) -> Vec<Grant> {
    store
        .and_then(|store| store.permissions_for(project_key).ok())
        .unwrap_or_default()
        .iter()
        .filter_map(|json| serde_json::from_str(json).ok())
        .collect()
}

/// The idle-compaction setting: minutes of quiet, or `None` for off.
fn idle_minutes_setting(store: Option<&Store>) -> Option<u64> {
    store
        .and_then(|store| store.setting("ui:idle_compaction").ok().flatten())
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|minutes| *minutes > 0)
}

/// When an idle compaction should run: after `minutes` of quiet since `last_activity`, and only
/// while the conversation is over half of its budget. `None` means never.
fn idle_deadline(last_activity: std::time::Instant, minutes: Option<u64>, due: bool) -> Option<std::time::Instant> {
    let minutes = minutes.filter(|_| due)?;
    Some(last_activity + std::time::Duration::from_secs(minutes * 60))
}

/// What re-code learnt about a model's characters-per-token ratio, from the database.
fn calibration_for(store: Option<&Store>, model: &str) -> compact::Calibration {
    store
        .and_then(|store| store.setting(&format!("cal:{model}")).ok().flatten())
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default()
}

/// After a turn or a compaction: tell the user what the turn noticed, keep what it learnt, and write
/// any compaction records (the report and the messages the summary replaced).
fn after_turn(chat: &mut Chat, store: Option<&Store>, events: &UnboundedSender<UiEvent>, recorder: &Recorder) {
    for warning in chat.warnings.drain(..) {
        let _ = events.send(UiEvent::Notice(warning));
    }
    for (report, originals) in chat.compaction_records.drain(..) {
        recorder.compaction(report, originals);
    }
    if let Some(store) = store
        && chat.calibration.samples > 0
        && !chat.model.is_empty()
        && let Ok(json) = serde_json::to_string(&chat.calibration)
    {
        let _ = store.set_setting(&format!("cal:{}", chat.model), &json);
    }
}

/// The user's context setting for a model, from the database.
fn window_setting(store: Option<&Store>, model: &str) -> WindowSetting {
    store
        .and_then(|store| store.setting(&WindowSetting::key(model)).ok().flatten())
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default()
}

/// The window a model is budgeted against, from its setting and what the listing or runtime says.
fn resolve_window(
    store: Option<&Store>,
    model: &str,
    model_limit: Option<u64>,
    is_local: bool,
    default_num_ctx: u32,
) -> (ContextWindow, WindowSetting) {
    let setting = window_setting(store, model);
    (ContextWindow::resolve(&setting, model_limit, is_local, default_num_ctx), setting)
}

/// Builds a cloud provider's adapter from its configuration and key.
fn build_cloud(provider: &CloudProvider, key: Option<String>) -> Result<Box<dyn Provider>, String> {
    let base = provider.base_url().ok_or("this provider has no address")?;
    if provider.kind == ProviderKind::Anthropic {
        let key = key.ok_or("this provider needs an API key. Select it and press e to add one.")?;
        Ok(Box::new(AnthropicProvider::new(&base, &key).map_err(|error| error.to_string())?))
    } else {
        Ok(Box::new(OpenAiProvider::new(&base, key.as_deref().unwrap_or("")).map_err(|error| error.to_string())?))
    }
}

/// Lists a cloud provider's models and sums them up. Free models a provider serves only to its own
/// app are left out and explained, so nothing is offered that can't be used; on OpenRouter the key's
/// own standing (free allowance used today, credits) is read too.
async fn probe_cloud(provider: &CloudProvider, key: Option<&str>) -> Result<(Vec<ModelInfo>, ProviderProbe), String> {
    let adapter = build_cloud(provider, key.map(str::to_owned))?;
    let models = adapter.list_models().await.map_err(|error| error.to_string())?;
    let base = provider.base_url().unwrap_or_default();
    let mut notes = Vec::new();
    let models: Vec<ModelInfo> = match recode_core::providers::free_models_client_only(&base) {
        Some(reason) => {
            let (hidden, usable): (Vec<_>, Vec<_>) = models.into_iter().partition(|m| m.facts.free == Some(true));
            if !hidden.is_empty() {
                notes.push(format!("{} free models aren't listed: {reason}.", hidden.len()));
            }
            usable
        }
        None => models,
    };
    if let Some(key) = key.filter(|key| !key.trim().is_empty())
        && recode_providers::openrouter::is_openrouter(&base)
    {
        match recode_providers::openrouter::key_status(&base, key).await {
            Ok(status) => notes.push(status.summary()),
            Err(error) => tracing::debug!(%error, "OpenRouter key status not read"),
        }
    }
    let free = models.iter().filter(|model| model.facts.free == Some(true)).count();
    let note = (!notes.is_empty()).then(|| notes.join(" "));
    let probe = ProviderProbe::Reachable { model_count: models.len(), free_model_count: free, note };
    Ok((models, probe))
}

/// Reads the model list from every enabled provider and builds the home screen plus the routing table.
async fn scan(
    paths: &Paths,
    url: &str,
    last_model: Option<String>,
    manager: &mut Manager,
    secrets: &dyn SecretStore,
    catalog: &Catalog,
    num_ctx: u32,
) -> (WelcomeInfo, HashMap<String, Route>, recode_runtime::Report) {
    let report = doctor::gather(paths, url).await;
    let mut info = welcome_info(&report, last_model, num_ctx);
    // A measured cache size (from a loaded model) is kept, so it's known the next time the model is unloaded.
    if let Some(store) = &manager.store {
        for model in info.models.iter_mut().filter(|m| m.provider_label.is_none()) {
            let key = format!("kv:{}", model.name);
            if model.loaded
                && let Some(bytes) = model.kv_bytes_per_token
            {
                let _ = store.set_setting(&key, &bytes.to_string());
            } else if let Ok(Some(stored)) = store.setting(&key)
                && let Ok(bytes) = stored.parse::<u64>()
            {
                model.kv_bytes_per_token = Some(bytes);
            }
        }
    }
    let mut routes: HashMap<String, Route> =
        info.models.iter().map(|model| (model.name.clone(), Route::Ollama)).collect();

    let providers = manager.extensions.snapshot().providers;
    let names: Vec<String> = providers.iter().map(|provider| provider.name.clone()).collect();
    manager.providers_with_keys = secrets.present(&names).await;

    for provider in providers.iter().filter(|provider| provider.enabled) {
        let key = secrets.get(&provider.name).await.ok().flatten();
        let needs_key = provider.kind != ProviderKind::OpenaiCompatible;
        if needs_key && key.is_none() {
            manager.record_provider_probe(provider.name.clone(), ProviderProbe::NoKey);
            continue;
        }
        match probe_cloud(provider, key.as_deref()).await {
            Ok((models, probe)) => {
                manager.record_provider_probe(provider.name.clone(), probe);
                for model in models {
                    // The first provider to offer a name owns it, so a later one can't hijack routing.
                    if routes.contains_key(&model.name) {
                        continue;
                    }
                    // What the provider says comes first; the models.dev catalog fills in the rest.
                    // Only the provider's own listing says what it charges: the catalog is matched by name.
                    let facts = model
                        .facts
                        .clone()
                        .merge(catalog.facts(&model.name).map(recode_core::model_facts::ModelFacts::without_pricing));
                    // A model that doesn't answer in text (music, image and video generators) can't chat.
                    if facts
                        .modalities
                        .as_deref()
                        .is_some_and(|m| m.split_once('→').is_some_and(|(_, output)| !output.contains("text")))
                    {
                        continue;
                    }
                    routes.insert(model.name.clone(), Route::Cloud { provider: provider.clone(), key: key.clone() });
                    let parameters = recode_core::model_facts::ModelFacts::parameters_from_name(&model.name);
                    info.models.push(WelcomeModel {
                        name: model.name,
                        size_gib: None,
                        family: Some(provider.kind.label().to_owned()),
                        parameters,
                        quantization: None,
                        context_limit: facts.context,
                        // Unknown defaults to tool-capable for frontier kinds, where it's the norm.
                        tools: facts.tools.unwrap_or(provider.kind != ProviderKind::OpenaiCompatible),
                        vision: facts.vision.unwrap_or(false),
                        thinking: facts.reasoning.unwrap_or(false),
                        fit: GpuFit::Cloud,
                        loaded: false,
                        provider_label: Some(provider.name.clone()),
                        free: facts.free.unwrap_or(false),
                        needs_key: key.is_none(),
                        price: facts.price,
                        kv_bytes_per_token: None,
                        details: facts.clone(),
                    });
                }
            }
            Err(error) => manager.record_provider_probe(provider.name.clone(), ProviderProbe::Failed { error }),
        }
    }
    info.models = sort_models(info.models);
    (info, routes, report.runtimes)
}

/// Pulls `@path` image attachments out of the prompt text, returning the cleaned text and the images.
/// Non-image `@` tokens are left in place.
fn take_image_attachments(text: &str) -> (String, Vec<ImageData>) {
    let mut images = Vec::new();
    let mut kept = Vec::new();
    for token in text.split_whitespace() {
        if let Some(path) = token.strip_prefix('@')
            && let Some(image) = read_image_file(std::path::Path::new(path))
        {
            images.push(image);
        } else {
            kept.push(token);
        }
    }
    (kept.join(" "), images)
}

fn read_image_file(path: &std::path::Path) -> Option<ImageData> {
    let media_type = match path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        _ => return None,
    };
    let bytes = std::fs::read(path).ok()?;
    Some(ImageData {
        media_type: media_type.to_owned(),
        data_base64: recode_core::base64::encode(&bytes),
        label: path.file_name()?.to_string_lossy().into_owned(),
    })
}

/// Builds the agent configuration for a turn: built-in + custom tool schemas, the custom tools
/// themselves, and any skill instructions whose triggers match the request.
/// Brings an OpenCode import into the configuration and says what happened, in one message.
pub(crate) fn summarise_opencode_import(
    extensions: &mut Extensions,
    import: recode_extensions::opencode::Import,
) -> (String, bool) {
    if import.files.is_empty() {
        let mut text = "No OpenCode configuration was found (~/.config/opencode/opencode.json).".to_owned();
        if let Some(path) = &import.held_back {
            text = format!("{} holds OpenCode configuration; trust this folder (/trust) to import it.", path.display());
        }
        if !import.warnings.is_empty() {
            text.push(' ');
            text.push_str(&import.warnings.join(" "));
        }
        return (text, true);
    }
    let mut parts = Vec::new();
    let servers: Vec<McpServer> =
        import.servers.into_iter().map(|server| McpServer { external: None, ..server }).collect();
    let server_count = servers.len();
    match extensions.import_servers(servers) {
        Ok(0) if server_count > 0 => parts.push("its MCP servers were already here".to_owned()),
        Ok(0) => {}
        Ok(n) => parts.push(format!("{n} MCP server(s) added (press t to test one)")),
        Err(error) => parts.push(format!("MCP servers couldn't be saved: {error}")),
    }
    let mut added_providers = 0;
    for provider in import.providers {
        if extensions.provider(&provider.name).is_some() {
            continue;
        }
        match extensions.save_provider(None, provider) {
            Ok(()) => added_providers += 1,
            Err(error) => parts.push(format!("a provider couldn't be saved: {error}")),
        }
    }
    if added_providers > 0 {
        parts.push(format!("{added_providers} provider(s) added without keys (k on the Providers tab stores one)"));
    }
    if !import.instructions.is_empty() {
        parts.push(format!(
            "OpenCode loads {} instruction file(s) ({}); re-code reads RE-CODE.md, AGENTS.md and CLAUDE.md instead",
            import.instructions.len(),
            import.instructions.join(", ")
        ));
    }
    if !import.not_run.is_empty() {
        parts.push(format!("not brought over: {}", import.not_run.join("; ")));
    }
    if let Some(path) = &import.held_back {
        parts.push(format!("{} waits for /trust", path.display()));
    }
    let mut text =
        format!("From OpenCode: {}.", if parts.is_empty() { "nothing new".to_owned() } else { parts.join("; ") });
    if !import.warnings.is_empty() {
        text.push(' ');
        text.push_str(&import.warnings.join(" "));
    }
    (text, false)
}

/// Shown when the install client couldn't be built (TLS setup failed), which has never been seen.
const HUB_UNAVAILABLE: &str = "Skill installs aren't available: the HTTP client couldn't be set up. The log says why.";

/// The two meta-tools that let the model discover and use MCP tools without their schemas filling
/// the context. Added only when at least one MCP server is enabled.
pub(crate) const LIST_MCP_TOOLS: &str = "list_mcp_tools";
pub(crate) const CALL_MCP_TOOL: &str = "call_mcp_tool";
/// Prefix for a directly-exposed MCP tool: `mcp__<server>__<tool>`.
pub(crate) const MCP_DIRECT_PREFIX: &str = "mcp__";

/// Whether a tool call targets MCP (either meta-tool, or a directly-exposed server tool).
fn is_mcp_call(name: &str) -> bool {
    name == LIST_MCP_TOOLS || name == CALL_MCP_TOOL || name.starts_with(MCP_DIRECT_PREFIX)
}

fn mcp_meta_tools(servers: &[String]) -> Vec<ToolDefinition> {
    let list = servers.join(", ");
    vec![
        ToolDefinition {
            name: LIST_MCP_TOOLS.to_owned(),
            description: format!(
                "Find tools on the MCP servers ({list}). Without arguments it lists each server's tools, with \
                 arguments like search(query: string, limit?: number) (? marks optional); a large server is listed \
                 by name only. Give `query` to search tool names and descriptions, or `server` and `tool` to see one \
                 tool's full description and argument schema before calling it."
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "server": {"type": "string", "description": "Limit to one server by name. Omit for all."},
                    "query": {"type": "string", "description": "Words to search for in tool names and descriptions."},
                    "tool": {"type": "string", "description": "One tool's name, to see it in full."}
                }
            }),
        },
        ToolDefinition {
            name: CALL_MCP_TOOL.to_owned(),
            description: "Call a tool on an MCP server, with the arguments list_mcp_tools showed for it.".to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "server": {"type": "string", "description": "The MCP server's name."},
                    "tool": {"type": "string", "description": "The tool to call on that server."},
                    "arguments": {"type": "object", "description": "The tool's arguments, as its schema requires."}
                },
                "required": ["server", "tool"]
            }),
        },
    ]
}

/// The MCP servers the agent can use: switched on, and of a kind re-code supports.
fn usable_servers(servers: &[McpServer]) -> Vec<String> {
    servers
        .iter()
        .filter(|server| server.enabled && server.external.as_ref().is_none_or(|e| e.unsupported.is_none()))
        .map(|server| server.name.clone())
        .collect()
}

fn build_agent_config(
    manager: &Manager,
    request: &str,
    plan_path: std::path::PathBuf,
    max_steps: usize,
    hooks: std::sync::Arc<crate::hooks::Hooks>,
) -> AgentConfig {
    let snapshot = manager.extensions.snapshot();
    let custom: std::collections::HashMap<String, CustomTool> =
        snapshot.tools.iter().filter(|tool| tool.enabled).map(|tool| (tool.name.clone(), tool.clone())).collect();
    let mut tools = tool_definitions();
    tools.extend(crate::background::tool_definitions());
    for tool in custom.values() {
        let properties: serde_json::Map<String, serde_json::Value> =
            tool.parameters.iter().map(|parameter| (parameter.clone(), json!({"type": "string"}))).collect();
        tools.push(ToolDefinition {
            name: tool.name.clone(),
            description: tool.description.clone(),
            parameters: json!({"type": "object", "properties": properties, "required": tool.parameters}),
        });
    }
    let servers = usable_servers(&snapshot.servers);
    let skills: Vec<Skill> = snapshot.skills.iter().filter(|s| s.enabled && s.problem.is_none()).cloned().collect();
    let mut extra_system = Vec::new();
    let index = crate::skill_tool::index(&skills);
    if !index.is_empty() {
        extra_system.push((ComponentKind::SkillsIndex, index));
    }
    let mut invoked_text = None;
    if let Some((skill, rest)) = invoked_skill(&skills, request) {
        if let Some(body) = recode_extensions::skills::read_body(std::path::Path::new(&skill.path)) {
            extra_system.push((
                ComponentKind::InvokedSkill,
                format!(
                    "The user invoked the skill `{}` for this request. Its instructions are here already, so don't \
                     load it with the skill tool; just follow them:\n{body}",
                    skill.name
                ),
            ));
        }
        invoked_text = Some(rest);
    }
    if !skills.is_empty() {
        tools.push(crate::skill_tool::tool_definition());
    }
    if !servers.is_empty() {
        tools.extend(mcp_meta_tools(&servers));
        let note = format!(
            "MCP servers are available: {}. To use their tools, call list_mcp_tools to discover them, \
             then call_mcp_tool to run one. Don't guess tool names.",
            servers.join(", ")
        );
        extra_system.push((ComponentKind::McpNote, note));
    }
    let project_trusted = project_trusted(&manager.places, manager.store.as_ref());
    AgentConfig {
        tools,
        custom,
        extra_system,
        skills,
        invoked_text,
        project_trusted,
        plan_path,
        max_steps,
        hooks,
        spawn_provider: None,
        depth: 0,
        memory: None,
    }
}

/// `/<skill-name> rest…` names a skill directly. Returns the skill and the request without the prefix.
fn invoked_skill<'a>(skills: &'a [Skill], request: &str) -> Option<(&'a Skill, String)> {
    let trimmed = request.trim_start();
    let mut words = trimmed.strip_prefix('/')?.splitn(2, char::is_whitespace);
    let name = words.next()?;
    let skill = skills.iter().find(|skill| skill.name == name)?;
    let rest = words.next().unwrap_or_default().trim();
    let rest = if rest.is_empty() { format!("Use the `{name}` skill.") } else { rest.to_owned() };
    Some((skill, rest))
}

async fn open_secrets() -> (Arc<dyn SecretStore>, Option<String>) {
    match Keyring::open().await {
        Ok(keyring) => (Arc::new(keyring), None),
        Err(reason) => {
            tracing::warn!(%reason, "the OS keyring is unavailable; API keys won't be remembered");
            (Arc::new(crate::secrets::Unavailable { reason: reason.clone() }), Some(reason))
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn drive(
    paths: Paths,
    url: String,
    num_ctx: u32,
    resume: bool,
    allowed: Vec<std::path::PathBuf>,
    max_steps: Option<usize>,
    memory_off: bool,
    events: UnboundedSender<UiEvent>,
    mut commands: UnboundedReceiver<UiCommand>,
) {
    let max_steps = max_steps.unwrap_or(MAX_AGENT_STEPS).max(1);
    let store = Store::open(&paths.database);
    if let Err(error) = &store {
        tracing::warn!(%error, "can't remember settings");
    }
    let (secrets, keyring_problem) = open_secrets().await;
    let catalog = Catalog::load(&paths.cache_dir.join("models-dev.json")).await;
    let workspace = std::env::current_dir().ok().and_then(|dir| Workspace::new(&dir).ok());
    // Folders given with --add-dir, granted for the session (and again after "New conversation").
    let mut allowed_folders = resolve_folders(&allowed, &events);
    let project_key = workspace.as_ref().map(|ws| crate::trust::canonical_key(ws.root())).unwrap_or_default();
    let plan_path = paths.data_dir.join("plans").join(format!("{}.md", crate::trust::short_hash(&project_key)));
    let memory_places = crate::memory::Places::new(&paths.data_dir, &project_key);
    let mut permissions = Permissions::with_folders(&allowed_folders);
    permissions.load_project(project_grants(store.as_ref().ok(), &project_key));
    // A context size chosen on the Settings tab wins over the configured default.
    let mut num_ctx = store
        .as_ref()
        .ok()
        .and_then(|store| store.setting("ui:num_ctx").ok().flatten())
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(num_ctx);
    let mut checkpoints = Checkpoints::default();
    let project = std::env::current_dir().map(|dir| dir.to_string_lossy().into_owned()).unwrap_or_default();
    let recorder = Recorder::start(paths.database.clone(), project.clone(), events.clone());
    let last_model = store.as_ref().ok().and_then(|store| store.setting(LAST_MODEL_KEY).ok().flatten());
    let mut manager = Manager::load(locations(&paths), store.ok());
    // Skill installs: fetched into the cache, scanned, shown, then moved into the skills folder.
    let (skill_hub, plugin_hub): (Option<std::sync::Arc<Hub>>, Option<std::sync::Arc<PluginHub>>) =
        match Fetcher::new(Bases::from_env()) {
            Ok(fetcher) => (
                Some(std::sync::Arc::new(Hub::new(fetcher.clone(), paths.cache_dir.clone(), paths.skills_dir.clone()))),
                Some(std::sync::Arc::new(PluginHub::new(
                    fetcher,
                    paths.cache_dir.clone(),
                    paths.data_dir.join("plugins"),
                ))),
            ),
            Err(error) => {
                tracing::warn!(%error, "skill and plugin installs unavailable");
                (None, None)
            }
        };

    let (mut info, mut routes, mut runtimes) =
        scan(&paths, &url, last_model.clone(), &mut manager, secrets.as_ref(), &catalog, num_ctx).await;
    let mut vision: HashMap<String, bool> = info.models.iter().map(|m| (m.name.clone(), m.vision)).collect();
    let mut tool_models: HashMap<String, bool> = info.models.iter().map(|m| (m.name.clone(), m.tools)).collect();
    let mut context_limits: HashMap<String, Option<u64>> =
        info.models.iter().map(|m| (m.name.clone(), m.context_limit)).collect();
    let mut kv_per_token: HashMap<String, Option<u64>> =
        info.models.iter().map(|m| (m.name.clone(), m.kv_bytes_per_token)).collect();
    let mut active_vision =
        last_model.as_deref().map(|name| vision.get(name).copied().unwrap_or(false)).unwrap_or(false);
    // Agent mode needs a project folder and a tool-capable model, local or cloud.
    let mut active_agent = workspace.is_some()
        && last_model.as_deref().is_some_and(|name| tool_models.get(name).copied().unwrap_or(false));
    let _ = events.send(UiEvent::Extensions(manager.snapshot()));
    let _ = events.send(UiEvent::Welcome(info.clone()));
    let _ = events.send(UiEvent::Runtimes(runtimes_info(&runtimes)));
    let _ = events.send(project_trust_event(&manager));

    // A live pool of MCP connections for the agent. Rebuilt (connections dropped, reconnected lazily)
    // only when the enabled server set changes, so connections persist across turns otherwise.
    // Stdio MCP servers run under bubblewrap with the project writable and the plugin folders bound;
    // the folders are read each time so a plugin installed during the session is seen.
    let project_root = workspace.as_ref().map(|ws| ws.root().to_path_buf());
    let plugins_dir = paths.data_dir.join("plugins");
    let mcp_launch = {
        let project_root = project_root.clone();
        let plugins_dir = plugins_dir.clone();
        move || recode_extensions::Launch::for_project(project_root.clone(), &plugins_dir)
    };
    let mut hub = McpHub::with_launch(manager.extensions.snapshot().servers, mcp_launch());

    let mut chat = match OllamaProvider::new(&url) {
        Ok(provider) => Chat::new(provider, num_ctx),
        Err(error) => {
            tracing::error!(%error, "can't create the Ollama client");
            return;
        }
    };
    // Honour a remembered model, so a returning user lands on their last choice.
    if let Some(name) = &last_model
        && let Some(route) = routes.get(name)
        && let Ok((provider, local)) = build_provider(route, &url)
    {
        let limit = context_limits.get(name).copied().flatten();
        let (window, setting) = resolve_window(manager.store.as_ref(), name, limit, local, num_ctx);
        chat.set_backend(provider, name.clone(), window.clone());
        chat.calibration = calibration_for(manager.store.as_ref(), name);
        let kv_bytes_per_token = kv_per_token.get(name).copied().flatten();
        let _ = events.send(UiEvent::ContextWindow { window, setting, kv_bytes_per_token });
    }

    let (probe_results, mut finished_probes) = mpsc::unbounded_channel::<(String, McpProbe)>();
    let (start_events, mut start_progress) = mpsc::unbounded_channel::<RuntimeStartEvent>();
    let (provider_results, mut finished_provider_probes) = mpsc::unbounded_channel::<(String, ProviderProbe)>();
    let mut announced = false;
    send_project_files(workspace.as_ref(), &events);
    crate::themes::watch(paths.themes_dir.clone(), events.clone());
    let _ = events.send(UiEvent::History(load_history(manager.store.as_ref(), &project)));
    if let Some(store) = &manager.store {
        let _ = events.send(UiEvent::Settings(store.settings_with_prefix("ui:").unwrap_or_default()));
    }
    let _ = events.send(UiEvent::Sessions(session_summaries(manager.store.as_ref(), &project)));
    watch_hardware(events.clone());
    if let Some(store) = &manager.store
        && let Ok(Some(name)) = store.setting(THEME_KEY)
    {
        let solid = store.setting(THEME_SOLID_KEY).ok().flatten().is_some_and(|value| value == "1");
        let variant = store.setting(THEME_VARIANT_KEY).ok().flatten().unwrap_or_default();
        let accent = store.setting(THEME_ACCENT_KEY).ok().flatten().unwrap_or_default();
        let _ = events.send(UiEvent::Theme { name, variant, accent, solid });
    }
    // Background checks of HTTP MCP servers, so the MCP tab shows which are up. They only send a
    // request (no program is started), and they report quietly: just the status, no message.
    let mut quiet_probes: BTreeSet<String> = BTreeSet::new();
    // Providers whose key was just saved: once they answer, the model list is rebuilt.
    let mut rescan_after_probe: BTreeSet<String> = BTreeSet::new();
    probe_http_servers(&mut manager, &probe_results, &mut quiet_probes);
    let _ = events.send(UiEvent::Extensions(manager.snapshot()));
    // Commands the driver gives itself, handled before waiting for the interface.
    let mut pending: std::collections::VecDeque<UiCommand> = std::collections::VecDeque::new();
    // Runtimes the user asked to start at launch (`a` on the start confirmation): only when the saved
    // command is still exactly the one re-code would run, so a changed setup asks again.
    if let Some(store) = manager.store.as_ref() {
        for runtime in &runtimes.runtimes {
            let recode_runtime::RuntimeState::InstalledStopped { start: Some(method) } = &runtime.state else {
                continue;
            };
            let Ok(Some(saved)) = store.setting(&format!("ui:autostart:{}", runtime.id)) else { continue };
            if saved == method.command_line() {
                let _ = events.send(UiEvent::Notice(format!(
                    "Starting {} as you asked ({}). Remove it on the Settings tab if you'd rather not.",
                    runtime.kind.name(),
                    method.command_line()
                )));
                pending.push_back(UiCommand::StartRuntime(runtime.id.clone()));
            } else {
                let _ = events.send(UiEvent::Notice(format!(
                    "{} isn't started automatically any more: its start command changed from `{saved}` to `{}`. Enter on the Runtimes tab starts it.",
                    runtime.kind.name(),
                    method.command_line()
                )));
            }
        }
    }
    if resume {
        pending.push_back(UiCommand::ContinueLast);
    }
    // Instruction files are read fresh for every request; the user hears about held-back ones once.
    let mut instructions_notified = false;
    // Stop hooks may ask for one more turn; never more than twice per request of the user's.
    let mut stop_continuations: u8 = 0;
    {
        let hooks = load_hooks(&manager, &paths);
        for problem in &hooks.problems {
            let _ = events.send(UiEvent::Notice(format!("Hook file skipped: {problem}")));
        }
        if hooks.held_back > 0 {
            let _ = events.send(UiEvent::Notice(format!(
                "This folder has {} hook{} in .re-code/hooks.toml that stay off until you trust it (/trust).",
                hooks.held_back,
                if hooks.held_back == 1 { "" } else { "s" }
            )));
        }
        let payload = json!({"event": "session_start", "project": manager.places.project});
        let grants = permissions.sandbox_grants();
        for outcome in hooks.run(crate::hooks::HookEvent::SessionStart, None, &payload, workspace.as_ref(), &grants) {
            let kind = EventKind::HookRan {
                name: outcome.name.clone(),
                event: "session_start".to_owned(),
                ok: outcome.ok,
                summary: outcome.summary.clone(),
            };
            let _ = events.send(UiEvent::Session(kind));
            if let Some(context) = outcome.add_context {
                chat.notes.push(format!("from the hook {}: {context}", outcome.name));
            }
        }
    }
    // `!command` runs: numbered so their tool rows have ids.
    let mut user_shell_count: u32 = 0;
    // Commands running in the background; all stopped when the driver ends.
    let mut background = crate::background::BackgroundTasks::default();
    // Idle compaction: opt-in, so a conversation left alone over half full is compacted while nobody waits.
    let mut idle_minutes = idle_minutes_setting(manager.store.as_ref());
    let mut last_activity = std::time::Instant::now();
    loop {
        let idle_at = idle_deadline(last_activity, idle_minutes, chat.compaction_is_due());
        let tasks_running = background.any_running();
        let command = if let Some(command) = pending.pop_front() {
            Some(command)
        } else {
            tokio::select! {
                command = commands.recv() => {
                    last_activity = std::time::Instant::now();
                    if matches!(command, Some(UiCommand::Send { .. })) {
                        stop_continuations = 0;
                    }
                    command
                }
                // Background commands are looked at once a second while any runs.
                () = async {
                    if tasks_running {
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => {
                    let ended = background.poll();
                    for (id, status, tail) in ended {
                        let _ = events.send(UiEvent::Notice(format!("Background task {id} {}: /tasks shows its output.", status.label())));
                        chat.notes.push(format!("background task {id} ended ({}). Its last output:\n{tail}", status.label()));
                    }
                    let _ = events.send(UiEvent::Tasks(background.list()));
                    continue;
                }
                () = async {
                    match idle_at {
                        Some(at) => tokio::time::sleep_until(at.into()).await,
                        None => std::future::pending::<()>().await,
                    }
                } => {
                    last_activity = std::time::Instant::now();
                    let _ = events.send(UiEvent::Notice(
                        "Compacting while idle, as the Settings tab's idle compaction asks.".to_owned(),
                    ));
                    pending.push_back(UiCommand::Compact { focus: None });
                    continue;
                }
                Some((name, probe)) = finished_probes.recv() => {
                    let message = match &probe {
                        McpProbe::Connected { tools, .. } => (format!("{name} connected, offering {} tools.", tools.len()), false),
                        McpProbe::Failed { error } => (format!("{name} failed: {error}"), true),
                    };
                    let quiet = quiet_probes.remove(&name);
                    manager.record_probe(name, probe);
                    let _ = events.send(UiEvent::Extensions(manager.snapshot()));
                    if !chat.model.is_empty() {
                        let declared = tool_models.get(&chat.model).copied().unwrap_or(false);
                        let _ = events.send(tool_status(&manager, active_agent, declared, workspace.is_some()));
                    }
                    if !quiet {
                        let _ = events.send(UiEvent::HomeMessage { text: message.0, is_error: message.1 });
                    }
                    continue;
                }
                Some(event) = start_progress.recv() => {
                    let ready = matches!(event, RuntimeStartEvent::Ready { .. });
                    let _ = events.send(UiEvent::RuntimeStart(event));
                    if ready {
                        pending.push_back(UiCommand::Refresh);
                    }
                    continue;
                }
                Some((name, probe)) = finished_provider_probes.recv() => {
                    let rescan = rescan_after_probe.remove(&name);
                    let message = match &probe {
                        ProviderProbe::Reachable { model_count, free_model_count, note } if rescan => {
                            pending.push_back(UiCommand::Refresh);
                            let added = if *free_model_count > 0 {
                                format!("{free_model_count} free models added")
                            } else {
                                format!("{model_count} models added")
                            };
                            let note = note.as_deref().map(|note| format!(" {note}")).unwrap_or_default();
                            (format!("{name} connected: {added}. They're in the Models tab.{note}"), false)
                        }
                        ProviderProbe::Reachable { model_count, free_model_count, note } => {
                            let free = if *free_model_count > 0 { format!(", {free_model_count} free") } else { String::new() };
                            let note = note.as_deref().map(|note| format!(" {note}")).unwrap_or_default();
                            (format!("{name} connected: {model_count} models{free}. Press Ctrl+R to add them to the list.{note}"), false)
                        }
                        ProviderProbe::Failed { error } => (format!("{name} failed: {error}"), true),
                        ProviderProbe::NoKey => (format!("{name} has no API key yet."), true),
                    };
                    manager.record_provider_probe(name, probe);
                    let _ = events.send(UiEvent::Extensions(manager.snapshot()));
                    let _ = events.send(UiEvent::HomeMessage { text: message.0, is_error: message.1 });
                    continue;
                }
            }
        };
        let Some(command) = command else { return };
        match command {
            UiCommand::Refresh => {
                manager.reload();
                manager.refresh_trust();
                let _ = events.send(project_trust_event(&manager));
                probe_http_servers(&mut manager, &probe_results, &mut quiet_probes);
                let last = manager.store.as_ref().and_then(|store| store.setting(LAST_MODEL_KEY).ok().flatten());
                let scanned = scan(&paths, &url, last, &mut manager, secrets.as_ref(), &catalog, num_ctx).await;
                info = scanned.0;
                routes = scanned.1;
                runtimes = scanned.2;
                let _ = events.send(UiEvent::Runtimes(runtimes_info(&runtimes)));
                vision = info.models.iter().map(|m| (m.name.clone(), m.vision)).collect();
                tool_models = info.models.iter().map(|m| (m.name.clone(), m.tools)).collect();
                context_limits = info.models.iter().map(|m| (m.name.clone(), m.context_limit)).collect();
                kv_per_token = info.models.iter().map(|m| (m.name.clone(), m.kv_bytes_per_token)).collect();
                let _ = events.send(UiEvent::Extensions(manager.snapshot()));
                let _ = events.send(UiEvent::Welcome(info.clone()));
                if let Some(reason) = &keyring_problem {
                    let _ = events.send(UiEvent::HomeMessage {
                        text: format!(
                            "Note: the system keyring isn't available ({reason}), so API keys won't be saved."
                        ),
                        is_error: true,
                    });
                }
            }
            UiCommand::UntrustProject => {
                let result = match (&manager.places.project, manager.store.as_ref()) {
                    (Some(project), Some(store)) => crate::trust::revoke(store, project),
                    (None, _) => Err("there's no project folder".to_owned()),
                    (_, None) => Err("the session database isn't available".to_owned()),
                };
                let (text, is_error) = match result {
                    Ok(()) => {
                        manager.refresh_trust();
                        let _ = events.send(UiEvent::Extensions(manager.snapshot()));
                        let _ = events.send(project_trust_event(&manager));
                        ("This folder's configuration is no longer trusted: its skills, hooks and instruction files are off.".to_owned(), false)
                    }
                    Err(error) => (format!("Couldn't stop trusting it: {error}"), true),
                };
                let _ = events.send(UiEvent::Notice(text.clone()));
                let _ = events.send(UiEvent::HomeMessage { text, is_error });
            }
            UiCommand::TrustProject => {
                let result = match (&manager.places.project, manager.store.as_ref()) {
                    (Some(project), Some(store)) => {
                        let config = crate::trust::project_config(project);
                        crate::trust::trust(store, project, &config).map(|()| config)
                    }
                    (None, _) => Err("there's no project folder".to_owned()),
                    (_, None) => Err("the session database isn't available, so trust can't be remembered".to_owned()),
                };
                match result {
                    Ok(config) => {
                        manager.refresh_trust();
                        let _ = events.send(UiEvent::Extensions(manager.snapshot()));
                        let _ = events.send(project_trust_event(&manager));
                        let count = config.skills.len();
                        let _ = events.send(UiEvent::HomeMessage {
                            text: format!(
                                "Trusted this folder's configuration: {count} project skill{} can now load. A change to it asks again.",
                                if count == 1 { "" } else { "s" }
                            ),
                            is_error: false,
                        });
                    }
                    Err(error) => {
                        let _ = events.send(UiEvent::HomeMessage {
                            text: format!("Couldn't trust the folder: {error}"),
                            is_error: true,
                        });
                    }
                }
            }
            UiCommand::StartRuntime(id) => {
                let found = runtimes.runtimes.iter().find(|r| r.id == id).cloned();
                match found {
                    Some(runtime) => match runtime.state {
                        recode_runtime::RuntimeState::InstalledStopped { start: Some(method) } => {
                            let sender = start_events.clone();
                            let log_dir = paths.log_dir.clone();
                            let kind = runtime.kind;
                            // Where to look for it afterwards: its own address, or the configured one for Ollama.
                            let wait_url = runtime
                                .url
                                .clone()
                                .or_else(|| (kind == recode_runtime::RuntimeKind::Ollama).then(|| url.clone()));
                            tokio::spawn(async move {
                                let _ = sender.send(RuntimeStartEvent::Started {
                                    id: id.clone(),
                                    command: method.command_line(),
                                });
                                if let Err(error) = recode_runtime::start::start(&method, Some(&log_dir)).await {
                                    let _ = sender.send(RuntimeStartEvent::Failed { id, error });
                                    return;
                                }
                                let Some(wait_url) = wait_url else {
                                    let _ = sender.send(RuntimeStartEvent::Ready {
                                        id,
                                        name: kind.name().to_owned(),
                                        version: None,
                                        verified: false,
                                    });
                                    return;
                                };
                                let progress = sender.clone();
                                let waiting_id = id.clone();
                                let result = recode_runtime::start::wait_until_answering(
                                    &wait_url,
                                    kind,
                                    recode_runtime::start::READY_TIMEOUT,
                                    move |seconds| {
                                        let _ = progress
                                            .send(RuntimeStartEvent::Waiting { id: waiting_id.clone(), seconds });
                                    },
                                )
                                .await;
                                let _ = sender.send(match result {
                                    Ok(version) => RuntimeStartEvent::Ready {
                                        id,
                                        name: kind.name().to_owned(),
                                        version,
                                        verified: true,
                                    },
                                    Err(error) => RuntimeStartEvent::Failed { id, error },
                                });
                            });
                        }
                        _ => {
                            let _ = events.send(UiEvent::RuntimeStart(RuntimeStartEvent::Failed {
                                id,
                                error: "it isn't stopped, or re-code doesn't know a safe way to start it".to_owned(),
                            }));
                        }
                    },
                    None => {
                        let _ = events.send(UiEvent::RuntimeStart(RuntimeStartEvent::Failed {
                            id,
                            error: "it isn't in the current list; press Ctrl+R to look again".to_owned(),
                        }));
                    }
                }
            }
            UiCommand::Extension(ExtensionCommand::TestServer(name)) => {
                if let Some(reason) =
                    manager.extensions.server(&name).and_then(|s| s.external).and_then(|e| e.unsupported)
                {
                    let _ = events
                        .send(UiEvent::HomeMessage { text: format!("{name} can't be used: {reason}"), is_error: true });
                } else if let Some(server) = manager.extensions.server(&name)
                    && !manager.probing.contains(&name)
                {
                    quiet_probes.remove(&name);
                    manager.probing.push(name.clone());
                    let _ = events.send(UiEvent::Extensions(manager.snapshot()));
                    let sender = probe_results.clone();
                    let launch = mcp_launch();
                    tokio::spawn(async move {
                        let result = recode_extensions::probe_with(&server, &launch).await;
                        let _ = sender.send((name, result));
                    });
                }
            }
            UiCommand::Extension(ExtensionCommand::TestProvider(name)) => {
                if let Some(provider) = manager.extensions.provider(&name).cloned() {
                    let key = secrets.get(&name).await.ok().flatten();
                    let sender = provider_results.clone();
                    tokio::spawn(async move {
                        let probe = match probe_cloud(&provider, key.as_deref()).await {
                            Ok((_, probe)) => probe,
                            Err(error) => ProviderProbe::Failed { error },
                        };
                        let _ = sender.send((name, probe));
                    });
                }
            }
            UiCommand::Extension(ExtensionCommand::SaveProvider { original, provider, key }) => {
                let name = provider.name.clone();
                let message = store_provider_key(secrets.as_ref(), original.as_deref(), &name, key.as_deref()).await;
                let message = message.map(|error| (error, true));
                let result =
                    manager.extensions.save_provider(original.as_deref(), provider).map_err(|error| error.to_string());
                manager.forget_provider_probe(&name);
                let present = secrets.present(std::slice::from_ref(&name)).await;
                manager.providers_with_keys.extend(present);
                let saved_key = key.as_ref().is_some_and(|key| !key.trim().is_empty());
                let outcome = message.unwrap_or_else(|| match result {
                    Ok(()) if saved_key => (format!("Saved {name}. Checking it…"), false),
                    Ok(()) => (format!("Saved {name}. Press t to test it."), false),
                    Err(error) => (error, true),
                });
                let _ = events.send(UiEvent::Extensions(manager.snapshot()));
                let _ = events.send(UiEvent::HomeMessage { text: outcome.0, is_error: outcome.1 });
                // A fresh key is checked at once, and the models it unlocks join the list by themselves.
                if saved_key && !outcome.1 {
                    rescan_after_probe.insert(name.clone());
                    pending.push_back(UiCommand::Extension(ExtensionCommand::TestProvider(name)));
                }
            }
            UiCommand::Extension(ExtensionCommand::DeleteProvider(name)) => {
                let _ = secrets.delete(&name).await;
                manager.forget_provider_probe(&name);
                manager.providers_with_keys.remove(&name);
                let outcome = match manager.extensions.delete_provider(&name) {
                    Ok(()) => (format!("Deleted {name}."), false),
                    Err(error) => (error.to_string(), true),
                };
                let _ = events.send(UiEvent::Extensions(manager.snapshot()));
                let _ = events.send(UiEvent::HomeMessage { text: outcome.0, is_error: outcome.1 });
            }
            UiCommand::Extension(ExtensionCommand::SetProviderEnabled(name, enabled)) => {
                let _ = manager.extensions.set_provider_enabled(&name, enabled);
                let state = if enabled { "on" } else { "off" };
                let _ = events.send(UiEvent::Extensions(manager.snapshot()));
                let _ = events.send(UiEvent::HomeMessage { text: format!("{name} is {state}."), is_error: false });
            }
            UiCommand::Extension(ExtensionCommand::AddMcpFromLink { name, link }) => {
                let outcome = match recode_extensions::registry::resolve_any(name.as_deref(), &link).await {
                    Ok(resolved) => {
                        let mut added = Vec::new();
                        let mut failed = Vec::new();
                        for server in resolved.servers {
                            let server_name = server.name.clone();
                            match manager.extensions.save_server(None, server) {
                                Ok(()) => added.push(server_name),
                                Err(error) => failed.push(format!("{server_name}: {error}")),
                            }
                        }
                        let mut text = match added.len() {
                            0 => String::new(),
                            1 => format!("Added {}. Press t to test it.", added[0]),
                            n => format!("Added {n} servers: {}. Press t on each to test it.", added.join(", ")),
                        };
                        if !failed.is_empty() {
                            text.push_str(&format!(" Not added: {}.", failed.join("; ")));
                        }
                        if let Some(note) = resolved.note {
                            text.push(' ');
                            text.push_str(&note);
                        }
                        (text, added.is_empty())
                    }
                    Err(error) => (error, true),
                };
                let _ = events.send(UiEvent::Extensions(manager.snapshot()));
                let _ = events.send(UiEvent::HomeMessage { text: outcome.0, is_error: outcome.1 });
            }
            UiCommand::Extension(ExtensionCommand::SearchMcp(query)) => {
                let sink = events.clone();
                tokio::spawn(async move {
                    let (hits, error) = match recode_extensions::registry::search(&query, 25).await {
                        Ok(hits) => (hits, None),
                        Err(error) => (Vec::new(), Some(error)),
                    };
                    let _ = sink.send(UiEvent::McpSearch { query, hits, error });
                });
            }
            UiCommand::Extension(ExtensionCommand::StageSkill(tap)) => {
                let sink = events.clone();
                match skill_hub.clone() {
                    Some(hub) => {
                        tokio::spawn(async move {
                            let _ = sink.send(UiEvent::Install(hub.stage(&tap).await));
                        });
                    }
                    None => {
                        let _ = sink
                            .send(UiEvent::Install(InstallOutcome::Failed { tap, error: HUB_UNAVAILABLE.to_owned() }));
                    }
                }
            }
            UiCommand::Extension(ExtensionCommand::StageSkillText(text)) => {
                let outcome = match skill_hub.as_deref() {
                    Some(hub) => hub.stage_text(&text),
                    None => InstallOutcome::Failed { tap: "pasted".to_owned(), error: HUB_UNAVAILABLE.to_owned() },
                };
                let _ = events.send(UiEvent::Install(outcome));
            }
            UiCommand::Extension(ExtensionCommand::StageSkillBatch(taps)) => {
                let sink = events.clone();
                match skill_hub.clone() {
                    Some(hub) => {
                        tokio::spawn(async move {
                            let mut items = Vec::new();
                            for tap in taps {
                                let outcome = hub.stage(&tap).await;
                                items.push((tap, outcome));
                            }
                            let _ = sink.send(UiEvent::InstallBatch(items));
                        });
                    }
                    None => {
                        let items = taps
                            .into_iter()
                            .map(|tap| (tap.clone(), InstallOutcome::Failed { tap, error: HUB_UNAVAILABLE.to_owned() }))
                            .collect();
                        let _ = sink.send(UiEvent::InstallBatch(items));
                    }
                }
            }
            UiCommand::Extension(ExtensionCommand::CommitInstallBatch(items)) => {
                let mut done = Vec::new();
                let mut failed = Vec::new();
                for (staging, enable) in items {
                    match skill_hub.as_deref().map(|hub| hub.commit(&staging, enable, false)) {
                        Some(Ok(committed)) => {
                            let _ = manager.extensions.set_skill_enabled(&committed.name, committed.enabled);
                            done.push(format!("{} ({})", committed.name, if committed.enabled { "on" } else { "off" }));
                        }
                        Some(Err(error)) => failed.push(error),
                        None => failed.push(HUB_UNAVAILABLE.to_owned()),
                    }
                }
                let mut text = if done.is_empty() { String::new() } else { format!("Installed {}.", done.join(", ")) };
                if !failed.is_empty() {
                    text.push_str(&format!(" Not installed: {}", failed.join("; ")));
                }
                let _ = events.send(UiEvent::Extensions(manager.snapshot()));
                let _ = events.send(UiEvent::HomeMessage { text, is_error: done.is_empty() });
            }
            UiCommand::Extension(ExtensionCommand::StagePluginBatch(taps)) => {
                let sink = events.clone();
                let markets = manager.extensions.marketplaces().to_vec();
                match plugin_hub.clone() {
                    Some(hub) => {
                        tokio::spawn(async move {
                            let mut items = Vec::new();
                            for tap in taps {
                                let outcome = hub.stage(&tap, &markets).await;
                                items.push((tap, outcome));
                            }
                            let _ = sink.send(UiEvent::PluginInstallBatch(items));
                        });
                    }
                    None => {
                        let items = taps
                            .into_iter()
                            .map(|tap| (tap.clone(), PluginOutcome::Failed { tap, error: HUB_UNAVAILABLE.to_owned() }))
                            .collect();
                        let _ = sink.send(UiEvent::PluginInstallBatch(items));
                    }
                }
            }
            UiCommand::Extension(ExtensionCommand::CommitPluginBatch(items)) => {
                let mut done = Vec::new();
                let mut failed = Vec::new();
                for (staging, enable) in items {
                    match plugin_hub.as_deref().map(|hub| hub.commit(&staging, enable)) {
                        Some(Ok(committed)) => {
                            let _ = manager.extensions.set_plugin_enabled(&committed.name, committed.enabled);
                            done.push(format!("{} ({})", committed.name, if committed.enabled { "on" } else { "off" }));
                        }
                        Some(Err(error)) => failed.push(error),
                        None => failed.push(HUB_UNAVAILABLE.to_owned()),
                    }
                }
                let mut text =
                    if done.is_empty() { String::new() } else { format!("Installed plugins {}.", done.join(", ")) };
                if !failed.is_empty() {
                    text.push_str(&format!(" Not installed: {}", failed.join("; ")));
                }
                let _ = events.send(UiEvent::Extensions(manager.snapshot()));
                let _ = events.send(UiEvent::HomeMessage { text, is_error: done.is_empty() });
            }
            UiCommand::Extension(ExtensionCommand::UpdateSkill(name)) => {
                let sink = events.clone();
                match skill_hub.clone() {
                    Some(hub) => {
                        tokio::spawn(async move {
                            let _ = sink.send(UiEvent::Install(hub.update(&name).await));
                        });
                    }
                    None => {
                        let _ = sink.send(UiEvent::Install(InstallOutcome::Failed {
                            tap: format!("update {name}"),
                            error: HUB_UNAVAILABLE.to_owned(),
                        }));
                    }
                }
            }
            UiCommand::Extension(ExtensionCommand::CommitInstall { staging, enable, accept }) => {
                let outcome = match skill_hub.as_deref().map(|hub| hub.commit(&staging, enable, accept)) {
                    Some(Ok(committed)) => {
                        let mut text = if committed.enabled {
                            format!("Installed {} and switched it on.", committed.name)
                        } else {
                            format!("Installed {}, off. Space switches it on.", committed.name)
                        };
                        if let Some(note) = &committed.note {
                            text.push(' ');
                            text.push_str(note);
                        }
                        let saved = match &committed.accepted {
                            Some(fingerprint) => manager.extensions.accept_scan(&committed.name, fingerprint),
                            None => manager.extensions.set_skill_enabled(&committed.name, committed.enabled),
                        };
                        if let Err(error) = saved {
                            text.push_str(&format!(" (Its on/off choice couldn't be saved: {error})"));
                        }
                        (text, false)
                    }
                    Some(Err(error)) => (error, true),
                    None => (HUB_UNAVAILABLE.to_owned(), true),
                };
                let _ = events.send(UiEvent::Extensions(manager.snapshot()));
                let _ = events.send(UiEvent::HomeMessage { text: outcome.0, is_error: outcome.1 });
            }
            UiCommand::Extension(ExtensionCommand::AcceptScan(name)) => {
                let skill = manager.extensions.snapshot().skills.into_iter().find(|skill| skill.name == name);
                let outcome = match skill {
                    Some(skill) if skill.scan.blocks() => {
                        match manager.extensions.accept_scan(&skill.name, &skill.scan.fingerprint()) {
                            Ok(()) => (
                                format!(
                                    "{} is on by your acceptance of its {} dangerous findings; it goes off again if they change.",
                                    skill.name,
                                    skill.scan.findings().len()
                                ),
                                false,
                            ),
                            Err(error) => (format!("Couldn't record that: {error}"), true),
                        }
                    }
                    Some(skill) => (format!("{} isn't flagged as dangerous; space switches it on.", skill.name), true),
                    None => (format!("There's no skill called {name}."), true),
                };
                let _ = events.send(UiEvent::Extensions(manager.snapshot()));
                let _ = events.send(UiEvent::HomeMessage { text: outcome.0, is_error: outcome.1 });
            }
            UiCommand::Extension(ExtensionCommand::DiscardInstall(staging)) => {
                // Skills and plugins share the staging area; whichever recognises the folder removes it.
                let skill = skill_hub.as_deref().map(|hub| hub.discard(&staging));
                if !matches!(skill, Some(Ok(())))
                    && let Some(Err(error)) = plugin_hub.as_deref().map(|hub| hub.discard(&staging))
                {
                    tracing::warn!(%error, "couldn't discard a staged install");
                }
            }
            UiCommand::Extension(ExtensionCommand::StagePlugin(tap)) => {
                let sink = events.clone();
                let markets = manager.extensions.marketplaces().to_vec();
                match plugin_hub.clone() {
                    Some(hub) => {
                        tokio::spawn(async move {
                            let _ = sink.send(UiEvent::PluginInstall(hub.stage(&tap, &markets).await));
                        });
                    }
                    None => {
                        let _ = sink.send(UiEvent::PluginInstall(PluginOutcome::Failed {
                            tap,
                            error: HUB_UNAVAILABLE.to_owned(),
                        }));
                    }
                }
            }
            UiCommand::Extension(ExtensionCommand::UpdatePlugin(name)) => {
                let sink = events.clone();
                let markets = manager.extensions.marketplaces().to_vec();
                match plugin_hub.clone() {
                    Some(hub) => {
                        tokio::spawn(async move {
                            let _ = sink.send(UiEvent::PluginInstall(hub.update(&name, &markets).await));
                        });
                    }
                    None => {
                        let _ = sink.send(UiEvent::PluginInstall(PluginOutcome::Failed {
                            tap: format!("update {name}"),
                            error: HUB_UNAVAILABLE.to_owned(),
                        }));
                    }
                }
            }
            UiCommand::Extension(ExtensionCommand::CommitPlugin { staging, enable }) => {
                let outcome = match plugin_hub.as_deref().map(|hub| hub.commit(&staging, enable)) {
                    Some(Ok(committed)) => {
                        let mut text = if committed.enabled {
                            format!("Installed plugin {}: its skills and MCP servers are on.", committed.name)
                        } else {
                            format!("Installed plugin {}, off. Space switches it on.", committed.name)
                        };
                        if let Some(note) = &committed.note {
                            text.push(' ');
                            text.push_str(note);
                        }
                        if let Err(error) = manager.extensions.set_plugin_enabled(&committed.name, committed.enabled) {
                            text.push_str(&format!(" (Its on/off choice couldn't be saved: {error})"));
                        }
                        (text, false)
                    }
                    Some(Err(error)) => (error, true),
                    None => (HUB_UNAVAILABLE.to_owned(), true),
                };
                let _ = events.send(UiEvent::Extensions(manager.snapshot()));
                let _ = events.send(UiEvent::HomeMessage { text: outcome.0, is_error: outcome.1 });
            }
            UiCommand::Extension(ExtensionCommand::SetPluginEnabled(name, enabled)) => {
                let outcome = match manager.extensions.set_plugin_enabled(&name, enabled) {
                    Ok(()) => (format!("{name} is {}.", if enabled { "on" } else { "off" }), false),
                    Err(error) => (error.to_string(), true),
                };
                let _ = events.send(UiEvent::Extensions(manager.snapshot()));
                let _ = events.send(UiEvent::HomeMessage { text: outcome.0, is_error: outcome.1 });
            }
            UiCommand::Extension(ExtensionCommand::DeletePlugin(name)) => {
                let outcome = match plugin_hub.as_deref().map(|hub| hub.remove(&name)) {
                    Some(Ok(())) => {
                        let _ = manager.extensions.forget_plugin(&name);
                        (format!("Removed plugin {name} and everything it brought."), false)
                    }
                    Some(Err(error)) => (error, true),
                    None => (HUB_UNAVAILABLE.to_owned(), true),
                };
                let _ = events.send(UiEvent::Extensions(manager.snapshot()));
                let _ = events.send(UiEvent::HomeMessage { text: outcome.0, is_error: outcome.1 });
            }
            UiCommand::Extension(ExtensionCommand::AddMarketplace(text)) => {
                let outcome = match plugin_hub.as_deref() {
                    Some(hub) => match hub.add_marketplace(&text).await {
                        Ok(market) => {
                            let summary = format!(
                                "Following {} ({} plugins{}). Press b to browse it.",
                                market.name,
                                market.plugin_count,
                                if market.sha.is_empty() { String::new() } else { format!(" at {}", &market.sha[..8]) }
                            );
                            match manager.extensions.add_marketplace(market) {
                                Ok(()) => (summary, false),
                                Err(error) => (error.to_string(), true),
                            }
                        }
                        Err(error) => (error, true),
                    },
                    None => (HUB_UNAVAILABLE.to_owned(), true),
                };
                let _ = events.send(UiEvent::Extensions(manager.snapshot()));
                let _ = events.send(UiEvent::HomeMessage { text: outcome.0, is_error: outcome.1 });
            }
            UiCommand::Extension(ExtensionCommand::RemoveMarketplace(name)) => {
                let outcome = match manager.extensions.remove_marketplace(&name) {
                    Ok(()) => {
                        if let Some(hub) = plugin_hub.as_deref() {
                            hub.forget_marketplace(&name);
                        }
                        (format!("No longer following {name}. Plugins installed from it stay."), false)
                    }
                    Err(error) => (error.to_string(), true),
                };
                let _ = events.send(UiEvent::Extensions(manager.snapshot()));
                let _ = events.send(UiEvent::HomeMessage { text: outcome.0, is_error: outcome.1 });
            }
            UiCommand::Extension(ExtensionCommand::BrowseMarketplace(name)) => {
                let sink = events.clone();
                let markets = manager.extensions.marketplaces().to_vec();
                match plugin_hub.clone() {
                    Some(hub) => {
                        tokio::spawn(async move {
                            let (listings, error) = match hub.browse(&name, &markets).await {
                                Ok(listings) => (listings, None),
                                Err(error) => (Vec::new(), Some(error)),
                            };
                            let _ = sink.send(UiEvent::PluginListings { title: name, listings, error });
                        });
                    }
                    None => {
                        let _ = sink.send(UiEvent::PluginListings {
                            title: name,
                            listings: Vec::new(),
                            error: Some(HUB_UNAVAILABLE.to_owned()),
                        });
                    }
                }
            }
            UiCommand::Extension(ExtensionCommand::SearchPlugins(query)) => {
                let sink = events.clone();
                let markets = manager.extensions.marketplaces().to_vec();
                match plugin_hub.clone() {
                    Some(hub) => {
                        tokio::spawn(async move {
                            let (listings, error) = match hub.search(&query, &markets, 40).await {
                                Ok(listings) => (listings, None),
                                Err(error) => (Vec::new(), Some(error)),
                            };
                            let _ = sink.send(UiEvent::PluginListings {
                                title: format!("search: {query}"),
                                listings,
                                error,
                            });
                        });
                    }
                    None => {
                        let _ = sink.send(UiEvent::PluginListings {
                            title: format!("search: {query}"),
                            listings: Vec::new(),
                            error: Some(HUB_UNAVAILABLE.to_owned()),
                        });
                    }
                }
            }
            UiCommand::Extension(ExtensionCommand::BrowseSkills { source, query }) => {
                let sink = events.clone();
                match skill_hub.clone() {
                    Some(hub) => {
                        tokio::spawn(async move {
                            let event = match hub.browse(source.as_deref(), &query, 40).await {
                                Ok(page) => UiEvent::SkillSearch {
                                    source,
                                    query,
                                    hits: page.hits,
                                    total: page.total,
                                    sources: page.sources,
                                    note: page.note,
                                    error: None,
                                },
                                Err(error) => UiEvent::SkillSearch {
                                    source,
                                    query,
                                    hits: Vec::new(),
                                    total: 0,
                                    sources: Vec::new(),
                                    note: None,
                                    error: Some(error),
                                },
                            };
                            let _ = sink.send(event);
                        });
                    }
                    None => {
                        let _ = sink.send(UiEvent::SkillSearch {
                            source,
                            query,
                            hits: Vec::new(),
                            total: 0,
                            sources: Vec::new(),
                            note: None,
                            error: Some(HUB_UNAVAILABLE.to_owned()),
                        });
                    }
                }
            }
            UiCommand::Extension(ExtensionCommand::ImportOpenCode) => {
                let trusted = project_trusted(&manager.places, manager.store.as_ref());
                let import = recode_extensions::opencode::read(
                    manager.places.user_config_root.as_deref(),
                    manager.places.project.as_deref(),
                    trusted,
                );
                let (text, is_error) = summarise_opencode_import(&mut manager.extensions, import);
                let _ = events.send(UiEvent::Extensions(manager.snapshot()));
                let _ = events.send(UiEvent::HomeMessage { text, is_error });
            }
            UiCommand::Extension(ExtensionCommand::ImportClaudeMcp) => {
                // Copies Claude Code's servers into re-code's own file, so they stay even if Claude Code changes.
                let copies = recode_extensions::claude::servers(
                    &std::path::Path::new(&std::env::var_os("HOME").unwrap_or_default()).join(".claude.json"),
                    std::env::current_dir().ok().as_deref(),
                )
                .into_iter()
                .filter(|server| server.external.as_ref().is_none_or(|e| e.unsupported.is_none()))
                .map(|server| McpServer { external: None, ..server })
                .collect();
                let outcome = match manager.extensions.import_servers(copies) {
                    Ok(0) => ("Nothing new to import from Claude Code.".to_owned(), false),
                    Ok(count) => {
                        (format!("Imported {count} MCP server(s) from Claude Code. Press t to test one."), false)
                    }
                    Err(error) => (error.to_string(), true),
                };
                let _ = events.send(UiEvent::Extensions(manager.snapshot()));
                let _ = events.send(UiEvent::HomeMessage { text: outcome.0, is_error: outcome.1 });
            }
            UiCommand::Extension(change) => {
                let (text, is_error) = match manager.apply(change) {
                    Ok(text) => (text, false),
                    Err(text) => (text, true),
                };
                let _ = events.send(UiEvent::Extensions(manager.snapshot()));
                let _ = events.send(UiEvent::HomeMessage { text, is_error });
                if !chat.model.is_empty() {
                    let declared = tool_models.get(&chat.model).copied().unwrap_or(false);
                    let _ = events.send(tool_status(&manager, active_agent, declared, workspace.is_some()));
                }
            }
            UiCommand::ChooseModel(name) => {
                if let Some(store) = &manager.store
                    && let Err(error) = store.set_setting(LAST_MODEL_KEY, &name)
                {
                    tracing::warn!(%error, "can't remember the model");
                }
                active_vision = vision.get(&name).copied().unwrap_or(false);
                active_agent = workspace.is_some() && tool_models.get(&name).copied().unwrap_or(false);
                let declared = tool_models.get(&name).copied().unwrap_or(false);
                let _ = events.send(tool_status(&manager, active_agent, declared, workspace.is_some()));
                match routes.get(&name) {
                    Some(route) => match build_provider(route, &url) {
                        Ok((provider, local)) => {
                            let limit = context_limits.get(&name).copied().flatten();
                            let (window, setting) =
                                resolve_window(manager.store.as_ref(), &name, limit, local, num_ctx);
                            if window.window.is_none() {
                                let _ = events.send(UiEvent::Notice(format!(
                                    "{name}'s context limit is {}, so the conversation can't be compacted on its own. \
                                     Set one with /autocompact, such as /autocompact 128k.",
                                    window.source
                                )));
                            }
                            chat.set_backend(provider, name.clone(), window.clone());
                            chat.calibration = calibration_for(manager.store.as_ref(), &name);
                            let kv_bytes_per_token = kv_per_token.get(&name).copied().flatten();
                            let _ = events.send(UiEvent::ContextWindow { window, setting, kv_bytes_per_token });
                        }
                        Err(error) => {
                            let _ = events.send(UiEvent::Notice(format!("Can't use {name}: {error}")));
                        }
                    },
                    None => chat.model = name,
                }
                if !announced {
                    announced = true;
                    // The agent introduction is shown once per project; after that the side panel's
                    // Tools line says what's on. The chat-only notice always explains itself.
                    let seen_key = format!("intro_seen:{project}");
                    let seen =
                        manager.store.as_ref().and_then(|store| store.setting(&seen_key).ok().flatten()).is_some();
                    if !active_agent {
                        let _ = events.send(UiEvent::Notice(CHAT_ONLY_NOTICE.to_owned()));
                    } else if !seen {
                        let _ = events.send(UiEvent::Notice(AGENT_NOTICE.to_owned()));
                        if let Some(store) = &manager.store {
                            let _ = store.set_setting(&seen_key, "1");
                        }
                    }
                }
            }
            UiCommand::Send { text, images } => {
                remember_prompt(manager.store.as_ref(), &project, &text);
                refresh_instructions(&mut chat, &manager, &paths, &events, &mut instructions_notified);
                // The model is reminded of commands still running in the background, so it doesn't start twins.
                let running = background.describe_running();
                if !running.is_empty() {
                    chat.notes.push(format!("background tasks still running:\n{}", running.join("\n")));
                }
                let (clean, file_images) = take_image_attachments(&text);
                if let Some(ws) = workspace.as_ref() {
                    chat.notes.extend(mentioned_files(&clean, ws));
                }
                let mut all = images;
                all.extend(file_images);
                let cancel = CancellationToken::new();
                let sink = events.clone();
                if !all.is_empty() && !active_vision {
                    let _ = sink.send(UiEvent::Notice(
                        "The attached image was dropped: this model can't see images. Switch with Ctrl+L.".to_owned(),
                    ));
                    all.clear();
                }
                let record = recorder.clone();
                let emit = move |kind| {
                    record.event(&kind);
                    let _ = sink.send(UiEvent::Session(kind));
                };
                let queued: Queued = std::sync::Mutex::new(std::collections::VecDeque::new());
                match (active_agent, workspace.as_ref()) {
                    (true, Some(ws)) => {
                        // Rebuild the hub if any server changed since it was built (a JetBrains IDE gets a
                        // new port each launch, so the address matters, not just the name).
                        let servers = manager.extensions.snapshot().servers;
                        if !hub.matches(&servers) {
                            hub = McpHub::with_launch(servers, mcp_launch());
                        }
                        let hooks = std::sync::Arc::new(load_hooks(&manager, &paths));
                        let mut config = build_agent_config(&manager, &clean, plan_path.clone(), max_steps, hooks);
                        let mode = if memory_off {
                            crate::memory::Mode::Off
                        } else {
                            crate::memory::Mode::parse(
                                manager.store.as_ref().and_then(|s| s.setting("ui:memory").ok().flatten()).as_deref(),
                            )
                        };
                        if mode != crate::memory::Mode::Off {
                            let memories = crate::memory::list(&memory_places);
                            if let Some(part) = crate::memory::system_part(&memories, mode) {
                                config.extra_system.insert(0, (ComponentKind::Memory, part));
                            }
                            config.tools.push(crate::memory::tool_definition(mode));
                            config.memory = Some(MemoryConfig {
                                places: memory_places.clone(),
                                read_only: mode == crate::memory::Mode::ReadOnly,
                            });
                        }
                        // Tasks need their own connection to the same model, and room to be worth it.
                        config.spawn_provider = routes.get(&chat.model).cloned().map(|route| {
                            let url = url.clone();
                            Arc::new(move || build_provider(&route, &url).map(|(provider, _)| provider))
                                as ProviderFactory
                        });
                        if config.spawn_provider.is_some() && chat.window.window.unwrap_or(0) >= 16384 {
                            config.tools.push(crate::subagent::tool_definition());
                        }
                        let clean = config.invoked_text.take().unwrap_or(clean);
                        // Directly-exposed servers send their tool schemas straight to the model.
                        config.tools.extend(hub.direct_tool_defs().await);
                        let (approval_tx, mut approval_rx) = mpsc::unbounded_channel();
                        let turn = chat.agent_turn(
                            clean,
                            all,
                            ws,
                            &config,
                            &mut hub,
                            &mut permissions,
                            &mut checkpoints,
                            &mut approval_rx,
                            &queued,
                            &mut background,
                            cancel.clone(),
                            emit,
                        );
                        tokio::pin!(turn);
                        loop {
                            tokio::select! {
                                () = &mut turn => break,
                                command = commands.recv() => match command {
                                    Some(UiCommand::Interrupt) => cancel.cancel(),
                                    Some(UiCommand::ResolveApproval(id, decision)) => {
                                        let _ = approval_tx.send((id, decision));
                                    }
                                    None => return,
                                    // Typed while the model works: delivered after its tool calls, or next.
                                    Some(UiCommand::Send { text, images }) => {
                                        remember_prompt(manager.store.as_ref(), &project, &text);
                                        if let Ok(mut queue) = queued.lock() {
                                            queue.push_back((text, images));
                                        }
                                    }
                                    Some(_) => {}
                                },
                            }
                        }
                    }
                    _ => {
                        let turn = chat.run_turn_with_images(clean, all, cancel.clone(), emit);
                        tokio::pin!(turn);
                        loop {
                            tokio::select! {
                                () = &mut turn => break,
                                command = commands.recv() => match command {
                                    Some(UiCommand::Interrupt) => cancel.cancel(),
                                    None => return,
                                    // Typed while the model works: delivered after its tool calls, or next.
                                    Some(UiCommand::Send { text, images }) => {
                                        remember_prompt(manager.store.as_ref(), &project, &text);
                                        if let Ok(mut queue) = queued.lock() {
                                            queue.push_back((text, images));
                                        }
                                    }
                                    Some(_) => {}
                                },
                            }
                        }
                    }
                }
                recorder.snapshot(chat.saved());
                after_turn(&mut chat, manager.store.as_ref(), &events, &recorder);
                // Stop hooks: the user's own check on the result; exit 2 sends its words back as the next request.
                {
                    let hooks = load_hooks(&manager, &paths);
                    if !hooks.is_empty() {
                        let result = chat
                            .messages
                            .iter()
                            .rev()
                            .find(|m| m.role == recode_core::message::Role::Assistant && !m.content.is_empty())
                            .map(|m| m.content.clone())
                            .unwrap_or_default();
                        let payload = json!({"event": "stop", "project": manager.places.project, "result": result});
                        let grants = permissions.sandbox_grants();
                        for outcome in
                            hooks.run(crate::hooks::HookEvent::Stop, None, &payload, workspace.as_ref(), &grants)
                        {
                            let kind = EventKind::HookRan {
                                name: outcome.name.clone(),
                                event: "stop".to_owned(),
                                ok: outcome.ok,
                                summary: outcome.summary.clone(),
                            };
                            recorder.event(&kind);
                            let _ = events.send(UiEvent::Session(kind));
                            if let Some(reason) = outcome.block
                                && stop_continuations < 2
                            {
                                stop_continuations += 1;
                                pending.push_back(UiCommand::Send {
                                    text: format!("[hook {}] {reason}", outcome.name),
                                    images: Vec::new(),
                                });
                            }
                        }
                    }
                }
                // Whatever was still queued when the turn ended becomes the next turn, in order.
                let leftover: Vec<(String, Vec<ImageData>)> =
                    queued.lock().map(|mut queue| queue.drain(..).collect()).unwrap_or_default();
                for (text, images) in leftover {
                    pending.push_back(UiCommand::Send { text, images });
                }
                if let Some(store) = &manager.store {
                    for grant in permissions.take_new_project_grants() {
                        if let Ok(json) = serde_json::to_string(&grant) {
                            let _ = store.add_permission(&project_key, &json, recode_core::ids::UnixMillis::now());
                        }
                    }
                }
                refresh_sessions_soon(paths.database.clone(), project.clone(), events.clone());
                // Files may have been created or deleted by the turn.
                send_project_files(workspace.as_ref(), &events);
            }
            UiCommand::Init => match manager.places.project.clone() {
                None => {
                    let _ = events
                        .send(UiEvent::Notice("There's no project folder here to write a RE-CODE.md in.".to_owned()));
                }
                Some(project) => {
                    let path = project.join(crate::init::FILE_NAME);
                    if path.exists() {
                        let _ = events.send(UiEvent::Notice(format!(
                            "{} exists already; /memory opens it in your editor.",
                            path.display()
                        )));
                    } else if let Some(text) = crate::init::generate(&project) {
                        let _ = events.send(UiEvent::InitPreview { path: path.display().to_string(), text });
                    } else {
                        let _ = events.send(UiEvent::Notice("The project folder can't be read.".to_owned()));
                    }
                }
            },
            UiCommand::WriteInit => {
                if let Some(project) = manager.places.project.clone() {
                    let path = project.join(crate::init::FILE_NAME);
                    let was_trusted = project_trusted(&manager.places, manager.store.as_ref());
                    let result = match crate::init::generate(&project) {
                        Some(text) if !path.exists() => std::fs::write(&path, text).map_err(|e| e.to_string()),
                        Some(_) => Err("it exists already".to_owned()),
                        None => Err("the folder can't be read".to_owned()),
                    };
                    match result {
                        Ok(()) => {
                            keep_trust_after_own_edit(&mut manager, &project, was_trusted, &events);
                            let _ = events.send(UiEvent::Notice(format!(
                                "Wrote {}. It's read with every request from now on; /memory edits it.",
                                path.display()
                            )));
                        }
                        Err(reason) => {
                            let _ =
                                events.send(UiEvent::Notice(format!("Couldn't write {}: {reason}.", path.display())));
                        }
                    }
                }
            }
            UiCommand::ListInstructions => {
                let trusted = project_trusted(&manager.places, manager.store.as_ref());
                let user_dir = paths.config_file.parent().map(std::path::Path::to_path_buf).unwrap_or_default();
                let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
                let loaded =
                    crate::instructions::load(manager.places.project.as_deref(), &user_dir, home.as_deref(), trusted);
                let files = loaded
                    .files
                    .iter()
                    .map(|file| recode_tui::link::InstructionFileInfo {
                        scope: file.scope.label().to_owned(),
                        path: file.path.display().to_string(),
                        tokens: file.tokens,
                        trusted: file.trusted,
                        problem: file.problem.clone(),
                        memory: None,
                    })
                    .chain(crate::memory::list(&memory_places).into_iter().map(|memory| {
                        let kind = if memory.kind.is_empty() { String::new() } else { format!(" · {}", memory.kind) };
                        recode_tui::link::InstructionFileInfo {
                            scope: format!("remembered · {}{kind}: {}", memory.scope.label(), memory.description),
                            path: memory.path.display().to_string(),
                            tokens: memory.tokens(),
                            trusted: true,
                            problem: None,
                            memory: Some((
                                if memory.scope == crate::memory::Scope::User { "user" } else { "project" }.to_owned(),
                                memory.name.clone(),
                            )),
                        }
                    }))
                    .collect();
                let _ = events.send(UiEvent::InstructionFiles {
                    files,
                    user_path: user_dir.join(crate::init::FILE_NAME).display().to_string(),
                    project_path: manager
                        .places
                        .project
                        .as_ref()
                        .map(|project| project.join(crate::init::FILE_NAME).display().to_string()),
                });
            }
            UiCommand::ForgetMemory { scope, name } => {
                let scope = if scope == "user" { crate::memory::Scope::User } else { crate::memory::Scope::Project };
                let text = match crate::memory::forget(&memory_places, scope, &name) {
                    Ok(()) => format!("Forgot \"{name}\"; re-code won't bring it up again."),
                    Err(reason) => format!("Couldn't forget \"{name}\": {reason}."),
                };
                let _ = events.send(UiEvent::Notice(text));
            }
            UiCommand::WriteNote { scope, text } => {
                use recode_tui::link::NoteScope;
                let user_dir = paths.config_file.parent().map(std::path::Path::to_path_buf).unwrap_or_default();
                let target = match scope {
                    NoteScope::User => Some(user_dir.join(crate::init::FILE_NAME)),
                    NoteScope::ProjectShared => manager.places.project.as_ref().map(|p| p.join(crate::init::FILE_NAME)),
                    NoteScope::ProjectLocal => manager.places.project.as_ref().map(|p| p.join("RE-CODE.local.md")),
                };
                let was_trusted = project_trusted(&manager.places, manager.store.as_ref());
                match target {
                    None => {
                        let _ =
                            events.send(UiEvent::Notice("There's no project folder here for that note.".to_owned()));
                    }
                    Some(path) => match crate::instructions::append_note(&path, &text) {
                        Ok(()) => {
                            if scope != NoteScope::User
                                && let Some(project) = manager.places.project.clone()
                            {
                                keep_trust_after_own_edit(&mut manager, &project, was_trusted, &events);
                            }
                            let _ = events.send(UiEvent::Notice(format!("Added to {}: {text}", path.display())));
                        }
                        Err(error) => {
                            let _ = events.send(UiEvent::Notice(format!("Couldn't write {}: {error}", path.display())));
                        }
                    },
                }
            }
            UiCommand::Rewind { to_turn, files, summarize } => {
                let result = chat.rewind(to_turn, summarize, &CancellationToken::new()).await;
                match result {
                    Err(reason) => {
                        let _ = events.send(UiEvent::Notice(reason));
                    }
                    Ok((keep_checkpoints, summary)) => {
                        let mut restored = Vec::new();
                        if files {
                            match workspace.as_ref() {
                                Some(ws) if checkpoints.turn_count() > 0 => {
                                    for report in checkpoints.undo_to(ws, keep_checkpoints) {
                                        restored.extend(report.restored.iter().chain(&report.removed).cloned());
                                        for (path, error) in &report.failed {
                                            let _ = events
                                                .send(UiEvent::Notice(format!("{path} couldn't be put back: {error}")));
                                        }
                                    }
                                }
                                Some(_) => {
                                    let _ = events.send(UiEvent::Notice(
                                        "The files weren't restored: file checkpoints don't survive a restart, so only \
                                         changes made since re-code started can be put back."
                                            .to_owned(),
                                    ));
                                }
                                None => {}
                            }
                        }
                        let rewound = EventKind::Rewound { to_turn, files: restored, summary };
                        recorder.event(&rewound);
                        let _ = events.send(UiEvent::Session(rewound));
                        recorder.snapshot(chat.saved());
                    }
                }
            }
            UiCommand::LoadHistory => {
                let mut all: Vec<String> = Vec::new();
                if let Some(store) = &manager.store
                    && let Ok(histories) = store.settings_with_prefix("history:")
                {
                    for (_, json) in histories {
                        if let Ok(prompts) = serde_json::from_str::<Vec<String>>(&json) {
                            for prompt in prompts {
                                all.retain(|known| *known != prompt);
                                all.push(prompt);
                            }
                        }
                    }
                }
                let _ = events.send(UiEvent::HistoryAll(all));
            }
            UiCommand::Export { path, text } => {
                let target = match path {
                    Some(path) if !path.trim().is_empty() => std::path::PathBuf::from(path.trim()),
                    _ => {
                        let base = workspace.as_ref().map_or_else(
                            || std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_default(),
                            |ws| ws.root().to_path_buf(),
                        );
                        base.join(export_file_name(recode_core::ids::UnixMillis::now()))
                    }
                };
                let message = match std::fs::write(&target, text) {
                    Ok(()) => format!("Exported the conversation to {}.", target.display()),
                    Err(error) => format!("Couldn't write {}: {error}", target.display()),
                };
                let _ = events.send(UiEvent::Notice(message));
            }
            UiCommand::ListTasks => {
                let _ = events.send(UiEvent::Tasks(background.list()));
            }
            UiCommand::KillTask(id) => {
                let stopped = background.kill(&id);
                let _ = events.send(UiEvent::Notice(if stopped {
                    format!("Stopped background task {id}.")
                } else {
                    format!("Background task {id} wasn't running.")
                }));
                let _ = events.send(UiEvent::Tasks(background.list()));
            }
            UiCommand::ShowTaskOutput(id) => match background.output(&id) {
                Some((status, output)) => {
                    let _ =
                        events.send(UiEvent::Pager { title: format!("Task {id} · {}", status.label()), text: output });
                }
                None => {
                    let _ = events.send(UiEvent::Notice(format!("There's no background task {id}.")));
                }
            },
            UiCommand::RunShell { command, network, background: in_background } => {
                let Some(ws) = workspace.as_ref() else {
                    let _ = events.send(UiEvent::Notice("Commands need a project folder to run in.".to_owned()));
                    continue;
                };
                if in_background {
                    let grants = permissions.sandbox_grants();
                    match background.spawn(ws, &command, network, &grants) {
                        Ok(id) => {
                            let _ = events.send(UiEvent::Notice(format!(
                                "Started `{command}` in the background as task {id}. /tasks shows it."
                            )));
                            chat.notes.push(format!("the user started `{command}` in the background as task {id}"));
                        }
                        Err(error) => {
                            let _ = events.send(UiEvent::Notice(format!("Couldn't start `{command}`: {error}")));
                        }
                    }
                    let _ = events.send(UiEvent::Tasks(background.list()));
                    continue;
                }
                user_shell_count += 1;
                let call = ToolCall {
                    id: format!("user-shell-{user_shell_count}"),
                    name: "shell".to_owned(),
                    arguments: json!({"command": command, "network": network}),
                    extra: None,
                };
                let record = recorder.clone();
                let sink = events.clone();
                let emit = move |kind: EventKind| {
                    record.event(&kind);
                    let _ = sink.send(UiEvent::Session(kind));
                };
                emit(EventKind::ToolCallRequested { call: call.clone() });
                emit(EventKind::StateChanged { state: AgentState::RunningTool { name: "shell".to_owned() } });
                let cancel = CancellationToken::new();
                let grants = permissions.sandbox_grants();
                let no_custom_tools = HashMap::new();
                let run = execute_tool(&call, &no_custom_tools, ws, &mut hub, &grants, &cancel);
                tokio::pin!(run);
                let output = loop {
                    tokio::select! {
                        output = &mut run => break output,
                        command = commands.recv() => match command {
                            Some(UiCommand::Interrupt) => cancel.cancel(),
                            None => return,
                            Some(_) => {}
                        },
                    }
                };
                emit(EventKind::ToolOutput { call_id: call.id.clone(), text: output.content.clone() });
                emit(EventKind::ToolFinished {
                    call_id: call.id.clone(),
                    success: output.success,
                    summary: output.summary,
                });
                emit(EventKind::StateChanged { state: AgentState::Idle });
                // The model hears about it next time, with the tail of the output.
                let tail: String = {
                    let content = output.content.trim_end();
                    let start = content.len().saturating_sub(4000);
                    let mut cut = start;
                    while !content.is_char_boundary(cut) {
                        cut += 1;
                    }
                    content[cut..].to_owned()
                };
                chat.notes.push(format!(
                    "the user ran `{command}` themselves ({}):\n{tail}",
                    if output.success { "it succeeded" } else { "it failed" }
                ));
                recorder.snapshot(chat.saved());
            }
            UiCommand::SetPermissionMode(mode) => {
                if mode == PermissionMode::AutoSandbox && !sandbox::isolation().is_sandboxed() {
                    let reason = match sandbox::isolation() {
                        sandbox::Isolation::Unavailable { reason } => reason.clone(),
                        sandbox::Isolation::Bubblewrap => String::new(),
                    };
                    let _ = events.send(UiEvent::Notice(format!(
                        "Auto mode needs the sandbox, and bubblewrap isn't available here ({reason}). Staying in {} mode.",
                        permissions.mode.label()
                    )));
                    let _ = events.send(UiEvent::Session(EventKind::PermissionModeChanged { mode: permissions.mode }));
                    continue;
                }
                let leaving_plan = permissions.mode == PermissionMode::Plan && mode != PermissionMode::Plan;
                permissions.mode = mode;
                recorder.event(&EventKind::PermissionModeChanged { mode });
                let _ = events.send(UiEvent::Session(EventKind::PermissionModeChanged { mode }));
                if leaving_plan && plan_path.is_file() {
                    let _ = events.send(UiEvent::Notice(
                        "Plan mode is off. The plan is at /plan; say \"carry out the plan\" to have it done."
                            .to_owned(),
                    ));
                }
            }
            UiCommand::ListPermissions => {
                let project_grants: Vec<(String, String)> = permissions
                    .project_grants()
                    .iter()
                    .filter_map(|grant| serde_json::to_string(grant).ok().map(|json| (json, grant.describe())))
                    .collect();
                let _ = events.send(UiEvent::Permissions {
                    mode: permissions.mode,
                    session: permissions.session_grants(),
                    project: project_grants,
                });
            }
            UiCommand::RevokePermission(json) => {
                if let Ok(grant) = serde_json::from_str::<Grant>(&json) {
                    permissions.revoke_project(&grant);
                    if let Some(store) = &manager.store {
                        let _ = store.remove_permission(&project_key, &json);
                    }
                    let _ = events.send(UiEvent::Notice(format!("Forgot: {}.", grant.describe())));
                }
            }
            UiCommand::ShowPlan => match std::fs::read_to_string(&plan_path) {
                Ok(text) => {
                    let _ = events.send(UiEvent::Pager { title: "The plan".to_owned(), text });
                }
                Err(_) => {
                    let _ = events.send(UiEvent::Notice(
                        "No plan has been written for this project. Switch to plan mode (Shift+Tab) and ask for one."
                            .to_owned(),
                    ));
                }
            },
            UiCommand::ListCompactions => recorder.list_compactions(),
            UiCommand::ShowCompaction(seq) => recorder.show_compaction(seq),
            UiCommand::ShowLastRequest => {
                let tools = if active_agent && workspace.is_some() { tool_definitions() } else { Vec::new() };
                let tools = chat.offered_tools(&tools);
                let title = format!("The next request to {} · {} messages", chat.model, chat.messages.len());
                let _ = events.send(UiEvent::Pager { title, text: chat.request_preview(&tools) });
            }
            UiCommand::Compact { focus } => {
                if let Some(focus) = focus.map(|f| f.trim().to_owned()).filter(|f| !f.is_empty()) {
                    chat.compact_focus = Some(focus);
                }
                let tools = if active_agent && workspace.is_some() { tool_definitions() } else { Vec::new() };
                let sink = events.clone();
                let record = recorder.clone();
                let mut emit = move |kind: EventKind| {
                    record.event(&kind);
                    let _ = sink.send(UiEvent::Session(kind));
                };
                let result = chat.fit(&tools, workspace.as_ref(), true, &CancellationToken::new(), &mut emit).await;
                emit(EventKind::StateChanged { state: AgentState::Idle });
                if let Err(reason) = result {
                    let _ = events.send(UiEvent::Notice(reason));
                }
                recorder.snapshot(chat.saved());
                after_turn(&mut chat, manager.store.as_ref(), &events, &recorder);
            }
            UiCommand::NewConversation => {
                chat.reset();
                checkpoints = Checkpoints::default();
                let mode = permissions.mode;
                permissions = Permissions::with_folders(&allowed_folders);
                permissions.mode = mode;
                permissions.load_project(project_grants(manager.store.as_ref(), &project_key));
                recorder.new_session();
                let _ = events.send(UiEvent::Notice(
                    "New conversation. The previous one is saved: Ctrl+K, then \"Continue the last session\", brings it back."
                        .to_owned(),
                ));
            }
            UiCommand::ContinueLast => {
                if !chat.is_empty() {
                    let _ = events.send(UiEvent::Notice(
                        "This conversation has already started. Type /continue to choose an earlier one.".to_owned(),
                    ));
                    continue;
                }
                pending.push_front(UiCommand::ContinueSession(String::new()));
            }
            UiCommand::ListSessions => {
                let _ = events.send(UiEvent::Sessions(session_summaries(manager.store.as_ref(), &project)));
            }
            UiCommand::DeleteSession(id) => match recode_core::ids::SessionId::parse(&id) {
                Ok(session) => {
                    // Deleting the open conversation means starting afresh; the writer forgets the id too.
                    if recorder.current() == Some(session) {
                        pending.push_front(UiCommand::NewConversation);
                    }
                    recorder.delete(session);
                }
                Err(_) => {
                    let _ = events.send(UiEvent::Notice(format!("{id} isn't a session id.")));
                }
            },
            UiCommand::ContinueSession(id) => {
                // Whatever is open now is already saved, so it can be set aside.
                if !chat.is_empty() {
                    chat.reset();
                    recorder.new_session();
                }
                match load_session(manager.store.as_ref(), &project, &id) {
                    Err(message) => {
                        let _ = events.send(UiEvent::Notice(message));
                    }
                    Ok((snapshot, saved, replay)) => {
                        let model = saved.model.clone();
                        let available = routes.contains_key(&model);
                        chat.restore(saved);
                        if let Some(request) = &replay.interrupted {
                            chat.notes.push(format!(
                                "re-code was closed while working on \"{}\". Tool calls from that turn may have partly \
                                 run and weren't repeated; check the files before carrying on.",
                                session::title_of(request)
                            ));
                        }
                        recorder.continue_session(snapshot.session, snapshot.title.clone());
                        checkpoints = Checkpoints::default();
                        let _ = events.send(UiEvent::Resumed { model: available.then(|| model.clone()) });
                        for kind in replay.events {
                            let _ = events.send(UiEvent::Session(kind));
                        }
                        let mut notice = format!(
                            "Continued \"{}\" from {}. Changes made before the restart can't be undone with Ctrl+Z.",
                            snapshot.title,
                            ago(snapshot.updated_at)
                        );
                        if replay.interrupted.is_some() {
                            notice.push_str(" The last turn was cut off; the model will be told.");
                        }
                        if available {
                            pending.push_front(UiCommand::ChooseModel(model));
                        } else {
                            notice.push_str(&format!(
                                " It used {model}, which isn't available now: pick a model to carry on."
                            ));
                        }
                        let _ = events.send(UiEvent::Notice(notice));
                    }
                }
            }
            UiCommand::Undo => match workspace.as_ref().map(|ws| checkpoints.undo(ws)) {
                None => {
                    let _ =
                        events.send(UiEvent::Notice("Nothing to undo: re-code isn't in a project folder.".to_owned()));
                }
                Some(None) => {
                    let _ = events.send(UiEvent::Notice("Nothing to undo.".to_owned()));
                }
                Some(Some(report)) => {
                    let summary = report.describe();
                    chat.notes.push(format!("the user undid the file changes from an earlier turn. {summary}"));
                    let paths = report.restored.iter().chain(&report.removed).cloned().collect();
                    // Recorded like the rest of the session, so a continued session shows it too.
                    let undone = EventKind::Undone { summary, paths };
                    recorder.event(&undone);
                    let _ = events.send(UiEvent::Session(undone));
                    recorder.snapshot(chat.saved());
                }
            },
            UiCommand::AddFolder(path) => {
                let expanded = match path.strip_prefix('~') {
                    Some(rest) => std::path::Path::new(&std::env::var_os("HOME").unwrap_or_default())
                        .join(rest.trim_start_matches('/')),
                    None => std::path::PathBuf::from(&path),
                };
                let added = resolve_folders(&[expanded], &events);
                for dir in added {
                    permissions.allow(Grant::Folder { dir: dir.clone(), write: true });
                    allowed_folders.push(dir);
                }
            }
            UiCommand::SetContextWindow { model, setting } => {
                if let Some(store) = &manager.store {
                    let key = WindowSetting::key(&model);
                    match serde_json::to_string(&setting) {
                        Ok(json) if !setting.is_auto() => {
                            let _ = store.set_setting(&key, &json);
                        }
                        _ => {
                            let _ = store.delete_setting(&key);
                        }
                    }
                }
                if model == chat.model {
                    // A local model reloads with a new size on its next request; that's the cost of changing it.
                    let is_local = match routes.get(&model) {
                        Some(route) => matches!(route, Route::Ollama),
                        None => chat.window.local_num_ctx.is_some(),
                    };
                    let limit = context_limits.get(&model).copied().flatten();
                    let window = ContextWindow::resolve(&setting, limit, is_local, num_ctx);
                    chat.window = window.clone();
                    let kv_bytes_per_token = kv_per_token.get(&model).copied().flatten();
                    let _ = events.send(UiEvent::ContextWindow { window, setting, kv_bytes_per_token });
                }
            }
            UiCommand::SetSetting { key, value } => {
                // The default context size applies from the next model switch, as changing it reloads the model.
                if key == "num_ctx"
                    && let Ok(size) = value.parse::<u32>()
                {
                    num_ctx = size;
                }
                if key == "idle_compaction" {
                    idle_minutes = value.parse::<u64>().ok().filter(|minutes| *minutes > 0);
                }
                if let Some(store) = &manager.store {
                    let key = format!("ui:{key}");
                    let _ = if value.is_empty() { store.delete_setting(&key) } else { store.set_setting(&key, &value) };
                }
            }
            UiCommand::SetTheme { name, variant, accent, solid } => {
                if let Some(store) = &manager.store {
                    let _ = store.set_setting(THEME_KEY, &name);
                    let _ = store.set_setting(THEME_VARIANT_KEY, &variant);
                    let _ = store.set_setting(THEME_ACCENT_KEY, &accent);
                    let _ = store.set_setting(THEME_SOLID_KEY, if solid { "1" } else { "0" });
                }
            }
            UiCommand::Interrupt | UiCommand::ResolveApproval(..) => {}
        }
    }
}

const HISTORY_LIMIT: usize = 200;

fn history_key(project: &str) -> String {
    format!("history:{project}")
}

/// Earlier prompts typed in this project, oldest first.
fn load_history(store: Option<&Store>, project: &str) -> Vec<String> {
    store
        .and_then(|store| store.setting(&history_key(project)).ok().flatten())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Adds a prompt to this project's history, keeping the most recent ones.
fn remember_prompt(store: Option<&Store>, project: &str, prompt: &str) {
    let Some(store) = store else { return };
    let mut history = load_history(Some(store), project);
    if history.last().map(String::as_str) != Some(prompt) {
        history.push(prompt.to_owned());
    }
    let excess = history.len().saturating_sub(HISTORY_LIMIT);
    history.drain(..excess);
    if let Ok(text) = serde_json::to_string(&history) {
        let _ = store.set_setting(&history_key(project), &text);
    }
}

/// Sends the project's file list for `@` mentions, walked off the async runtime.
fn send_project_files(workspace: Option<&Workspace>, events: &UnboundedSender<UiEvent>) {
    let Some(workspace) = workspace.cloned() else { return };
    let events = events.clone();
    tokio::task::spawn_blocking(move || {
        let _ = events.send(UiEvent::ProjectFiles(recode_tools::project_files(&workspace, 20_000)));
    });
}

/// Mentioned files are attached, as Claude Code does: `@path` to a small text file puts its contents
/// in front of the request, so even a model without tools sees it. Secret-looking files never are.
const MENTION_MAX_BYTES: usize = 32 * 1024;

fn mentioned_files(text: &str, workspace: &Workspace) -> Vec<String> {
    let mut notes = Vec::new();
    for token in text.split_whitespace() {
        let Some(path) = token.strip_prefix('@') else { continue };
        let path = path.trim_end_matches([',', '.', ';', ':', ')', '?', '!']);
        let Ok(location) = workspace.locate(path) else { continue };
        if recode_tools::sensitive::is_sensitive(&location, workspace.root()) {
            notes.push(format!(
                "the user mentioned {}, which looks like it holds secrets, so it wasn't attached.",
                location.display()
            ));
            continue;
        }
        let Ok(bytes) = workspace.read_at(&location) else { continue };
        if bytes.len() > MENTION_MAX_BYTES {
            notes.push(format!(
                "the user mentioned {} ({} KB, too large to attach; read it if needed).",
                location.display(),
                bytes.len() / 1024
            ));
            continue;
        }
        let Ok(content) = String::from_utf8(bytes) else { continue };
        notes.push(format!(
            "the user mentioned {}; its contents:\n```\n{}\n```",
            location.display(),
            content.trim_end()
        ));
    }
    notes
}

/// Stores, replaces or removes a provider's key, and carries it across a rename (keys are stored by
/// provider name, so a renamed provider would otherwise lose its key). Returns the error to show, if any.
async fn store_provider_key(
    secrets: &dyn SecretStore,
    original: Option<&str>,
    name: &str,
    key: Option<&str>,
) -> Option<String> {
    match key.map(str::trim) {
        Some("") => {
            let _ = secrets.delete(name).await;
            if let Some(old) = original.filter(|old| *old != name) {
                let _ = secrets.delete(old).await;
            }
        }
        Some(key) => {
            if let Err(error) = secrets.set(name, key).await {
                return Some(error);
            }
            if let Some(old) = original.filter(|old| *old != name) {
                let _ = secrets.delete(old).await;
            }
        }
        None => {
            // Nothing typed: a renamed provider keeps the key it had.
            if let Some(old) = original.filter(|old| *old != name)
                && let Ok(Some(existing)) = secrets.get(old).await
            {
                if let Err(error) = secrets.set(name, &existing).await {
                    return Some(error);
                }
                let _ = secrets.delete(old).await;
            }
        }
    }
    None
}

/// Resolves the folders given with --add-dir, telling the user which were allowed and which couldn't be.
fn resolve_folders(requested: &[std::path::PathBuf], events: &UnboundedSender<UiEvent>) -> Vec<std::path::PathBuf> {
    let mut allowed = Vec::new();
    for path in requested {
        match path.canonicalize() {
            Ok(dir) if dir.is_dir() => {
                let _ = events.send(UiEvent::Notice(format!(
                    "Allowed for this session: {} (the agent can read and change files there, and commands can reach it).",
                    recode_tools::workspace::tilde(&dir)
                )));
                allowed.push(dir);
            }
            Ok(_) => {
                let _ = events.send(UiEvent::Notice(format!(
                    "--add-dir {} isn't a folder, so it wasn't allowed.",
                    path.display()
                )));
            }
            Err(error) => {
                let _ =
                    events.send(UiEvent::Notice(format!("--add-dir {}: {error}. It wasn't allowed.", path.display())));
            }
        }
    }
    allowed
}

/// Starts quiet background checks of the enabled HTTP MCP servers that aren't already being checked.
/// Stdio servers are left alone: checking one would start its program.
fn probe_http_servers(
    manager: &mut Manager,
    results: &UnboundedSender<(String, McpProbe)>,
    quiet: &mut BTreeSet<String>,
) {
    for server in manager.extensions.snapshot().servers {
        let usable = server.enabled && server.external.as_ref().is_none_or(|e| e.unsupported.is_none());
        let http = matches!(server.transport, recode_core::extensions::McpTransport::Http { .. });
        if !usable || !http || manager.probing.contains(&server.name) {
            continue;
        }
        manager.probing.push(server.name.clone());
        quiet.insert(server.name.clone());
        let sender = results.clone();
        tokio::spawn(async move {
            let result = recode_extensions::probe(&server).await;
            let _ = sender.send((server.name.clone(), result));
        });
    }
}

/// This folder's saved sessions, most recent first, as the interface lists them.
pub(crate) fn session_summaries(store: Option<&Store>, project: &str) -> Vec<recode_tui::link::SessionSummary> {
    store
        .and_then(|store| store.snapshots_for_project(project).ok())
        .unwrap_or_default()
        .into_iter()
        .map(|snapshot| recode_tui::link::SessionSummary {
            id: snapshot.session.to_string(),
            messages: serde_json::from_str::<SavedConversation>(&snapshot.conversation)
                .map(|saved| saved.messages.iter().filter(|m| m.role != recode_core::message::Role::System).count())
                .unwrap_or(0),
            title: if snapshot.title.is_empty() { "(untitled)".to_owned() } else { snapshot.title },
            age: ago(snapshot.updated_at),
        })
        .collect()
}

/// Sends the session list a moment after a snapshot was queued, once the writer has had time to land it.
fn refresh_sessions_soon(database: std::path::PathBuf, project: String, events: UnboundedSender<UiEvent>) {
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        let sessions = tokio::task::spawn_blocking(move || {
            Store::open(&database).ok().map(|store| session_summaries(Some(&store), &project)).unwrap_or_default()
        })
        .await
        .unwrap_or_default();
        let _ = events.send(UiEvent::Sessions(sessions));
    });
}

/// Keeps the welcome screen's GPU and memory meters live, polling every few seconds and sending
/// only when something changed. Reading sysfs and /proc is cheap; drawing only happens on a change.
fn watch_hardware(events: UnboundedSender<UiEvent>) {
    tokio::spawn(async move {
        let mut last: Option<Vec<Meter>> = None;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            let meters = tokio::task::spawn_blocking(|| {
                let gpus = doctor::hardware::gpus();
                let memory = doctor::hardware::memory();
                let cpu = doctor::hardware::cpu();
                hardware_meters(gpus.iter().find(|gpu| gpu.vendor != GpuVendor::Other).or(gpus.first()), &memory, &cpu)
            })
            .await
            .unwrap_or_default();
            if last.as_ref() != Some(&meters) {
                if events.send(UiEvent::Hardware(meters.clone())).is_err() {
                    return;
                }
                last = Some(meters);
            }
        }
    });
}

/// The welcome screen's meters from the machine's figures.
fn hardware_meters(
    gpu: Option<&doctor::hardware::Gpu>,
    memory: &doctor::hardware::Memory,
    cpu: &doctor::hardware::Cpu,
) -> Vec<Meter> {
    let mut hardware = Vec::new();
    match gpu {
        Some(gpu) => {
            let vendor = match gpu.vendor {
                GpuVendor::Amd => "AMD ",
                GpuVendor::Nvidia => "NVIDIA ",
                GpuVendor::Intel => "Intel ",
                GpuVendor::Other => "",
            };
            hardware.push(Meter {
                label: "GPU".to_owned(),
                used_gib: gpu.vram_used_bytes.map(gib),
                total_gib: gpu.vram_total_bytes.map(gib),
                detail: format!("{vendor}{}", gpu.name.as_deref().unwrap_or(&gpu.pci_id)),
            });
        }
        None => hardware.push(Meter {
            label: "GPU".to_owned(),
            used_gib: None,
            total_gib: None,
            detail: "None found, so models will run on the CPU".to_owned(),
        }),
    }
    let total_ram = memory.total_bytes.map(gib);
    let used_ram = total_ram.zip(memory.available_bytes.map(gib)).map(|(total, free)| (total - free).max(0.0));
    hardware.push(Meter {
        label: "Memory".to_owned(),
        used_gib: used_ram,
        total_gib: total_ram,
        detail: cpu.model.clone().unwrap_or_else(|| "Unknown CPU".to_owned()),
    });
    hardware
}

/// Finds a saved session for this folder (`id`, or the most recent when it's empty) and prepares it
/// to continue.
fn load_session(
    store: Option<&Store>,
    project: &str,
    id: &str,
) -> Result<(recode_store::Snapshot, SavedConversation, session::Replay), String> {
    let store = store.ok_or("Earlier sessions can't be read: re-code's database couldn't be opened.")?;
    let snapshot = if id.is_empty() {
        store
            .latest_snapshot(project)
            .map_err(|error| format!("Couldn't read the last session: {error}"))?
            .ok_or("There's no earlier session in this folder to continue.")?
    } else {
        store
            .snapshots_for_project(project)
            .map_err(|error| format!("Couldn't read the saved sessions: {error}"))?
            .into_iter()
            .find(|snapshot| snapshot.session.to_string() == id)
            .ok_or("That session isn't saved any more.")?
    };
    let saved: SavedConversation = serde_json::from_str(&snapshot.conversation)
        .map_err(|error| format!("The last session couldn't be read ({error}). It was left as it is."))?;
    let stored =
        store.replay(snapshot.session).map_err(|error| format!("Couldn't read the last session's history: {error}"))?;
    let replay = session::replay(stored.into_iter().map(|event| event.kind).collect());
    Ok((snapshot, saved, replay))
}

/// "5 minutes ago", for a past time.
pub(crate) fn ago(at: recode_core::ids::UnixMillis) -> String {
    let seconds = (recode_core::ids::UnixMillis::now().0 - at.0).max(0) / 1000;
    let (count, unit) = match seconds {
        s if s < 60 => return "just now".to_owned(),
        s if s < 3600 => (s / 60, "minute"),
        s if s < 86_400 => (s / 3600, "hour"),
        s => (s / 86_400, "day"),
    };
    format!("{count} {unit}{} ago", if count == 1 { "" } else { "s" })
}

/// Turns a machine report into what the welcome screen shows.
fn welcome_info(report: &Report, last_model: Option<String>, num_ctx: u32) -> WelcomeInfo {
    let gpu = report.gpus.iter().find(|gpu| gpu.vendor != GpuVendor::Other).or(report.gpus.first());
    let loaded_vram: u64 = match &report.ollama {
        OllamaStatus::Running { loaded, .. } => loaded.iter().map(|model| model.size_vram_bytes).sum(),
        _ => 0,
    };
    // Memory held by models already loaded can be reclaimed, so it counts as free for this purpose.
    let free_vram = gpu.and_then(|gpu| {
        Some(gib(gpu.vram_total_bytes?.saturating_sub(gpu.vram_used_bytes?.saturating_sub(loaded_vram))))
    });
    let free_ram = report.memory.available_bytes.map(gib);

    let mut hardware = Vec::new();
    match gpu {
        Some(gpu) => {
            let vendor = match gpu.vendor {
                GpuVendor::Amd => "AMD ",
                GpuVendor::Nvidia => "NVIDIA ",
                GpuVendor::Intel => "Intel ",
                GpuVendor::Other => "",
            };
            let total = gpu.vram_total_bytes.map(gib);
            hardware.push(Meter {
                label: "GPU".to_owned(),
                used_gib: gpu.vram_used_bytes.map(gib),
                total_gib: total,
                detail: format!("{vendor}{}", gpu.name.as_deref().unwrap_or(&gpu.pci_id)),
            });
        }
        None => hardware.push(Meter {
            label: "GPU".to_owned(),
            used_gib: None,
            total_gib: None,
            detail: "None found, so models will run on the CPU".to_owned(),
        }),
    }
    let total_ram = report.memory.total_bytes.map(gib);
    let used_ram = total_ram.zip(free_ram).map(|(total, free)| (total - free).max(0.0));
    hardware.push(Meter {
        label: "Memory".to_owned(),
        used_gib: used_ram,
        total_gib: total_ram,
        detail: report.cpu.model.clone().unwrap_or_else(|| "Unknown CPU".to_owned()),
    });

    let (provider, models) = match &report.ollama {
        OllamaStatus::Running { version, models, loaded, .. } => {
            let rows = models
                .iter()
                .map(|entry| {
                    let capabilities = entry.capabilities.as_ref().ok();
                    let declared = |get: fn(
                        &recode_core::capability::ModelCapabilities,
                    )
                        -> &Option<recode_core::capability::Capability<bool>>| {
                        capabilities.and_then(|c| get(c).as_ref()).is_some_and(|flag| flag.value)
                    };
                    let size_gib = entry.info.size_bytes.map(gib);
                    let running = loaded.iter().find(|model| model.name == entry.info.name);
                    // Measured beats declared: a loaded model's memory beyond its weights, per token of its context.
                    let measured_kv = running.and_then(|model| {
                        let context = model.context_length.filter(|c| *c > 0)?;
                        let weights = entry.info.size_bytes?;
                        let extra = model.size_bytes.checked_sub(weights).filter(|extra| *extra > 0)?;
                        Some(extra / context)
                    });
                    let kv_bytes_per_token = measured_kv
                        .or_else(|| capabilities.and_then(|c| c.kv_bytes_per_token.as_ref()).map(|kv| kv.value));
                    let kv_gib = kv_bytes_per_token.map_or(0.0, |bytes| gib(bytes * u64::from(num_ctx)));
                    WelcomeModel {
                        name: entry.info.name.clone(),
                        size_gib,
                        family: entry.info.family.clone(),
                        parameters: entry.info.parameter_size.clone(),
                        quantization: entry.info.quantization.clone(),
                        context_limit: capabilities.and_then(|c| c.context_limit.as_ref()).map(|limit| limit.value),
                        tools: declared(|c| &c.tools),
                        vision: declared(|c| &c.vision),
                        thinking: declared(|c| &c.thinking),
                        fit: estimate_fit(size_gib, kv_gib, free_vram, free_ram),
                        loaded: running.is_some(),
                        provider_label: None,
                        free: false,
                        needs_key: false,
                        price: None,
                        kv_bytes_per_token,
                        details: recode_core::model_facts::ModelFacts::default(),
                    }
                })
                .collect();
            (ProviderState::Ready { detail: format!("Ollama {version}") }, sort_models(rows))
        }
        other => (unavailable(other), Vec::new()),
    };
    WelcomeInfo { provider_name: "Ollama".to_owned(), provider, models, hardware, free_vram_gib: free_vram, last_model }
}

/// Estimates how a model will run: its weights, the key/value cache for the context it would get
/// (`kv_gib`, 0 when unknown) and a fixed overhead, against free GPU memory. Size alone can't settle
/// this, which is why it's only ever shown as an estimate.
fn estimate_fit(size_gib: Option<f64>, kv_gib: f64, free_vram: Option<f64>, free_ram: Option<f64>) -> GpuFit {
    let (Some(size), Some(vram)) = (size_gib, free_vram) else { return GpuFit::Unknown };
    let needed = size + kv_gib + OVERHEAD_GIB;
    if needed <= vram {
        return GpuFit::FitsGpu;
    }
    let over_gib = needed - vram;
    match free_ram {
        Some(ram) if over_gib > ram => GpuFit::TooLarge,
        _ => GpuFit::PartlyCpu { over_gib },
    }
}

fn unavailable(status: &OllamaStatus) -> ProviderState {
    match status {
        OllamaStatus::InstalledStopped { install, .. } => {
            let mut details = vec![format!("To start it: {}", install.start_command())];
            if let Installation::Quadlet { pod: Some(pod), .. } = install {
                details.push(format!("This also starts the other containers in {pod}."));
            }
            details.push("The Runtimes tab (→) lists it; Enter there starts it after showing the command.".to_owned());
            details.push("re-code doesn't start services you manage yourself without asking.".to_owned());
            ProviderState::Unavailable { headline: "Ollama is installed but stopped".to_owned(), details }
        }
        OllamaStatus::ActiveNotAnswering { url, error, .. } => ProviderState::Unavailable {
            headline: format!("Ollama's service is active, but {url} doesn't answer"),
            details: vec![error.clone(), "It may still be starting, or it may use a different address.".to_owned()],
        },
        OllamaStatus::Unhealthy { url, error } => ProviderState::Unavailable {
            headline: format!("Something at {url} didn't behave like Ollama"),
            details: vec![error.clone()],
        },
        OllamaStatus::Absent { url, .. } => ProviderState::Unavailable {
            headline: "Ollama wasn't found".to_owned(),
            details: vec![
                format!("Nothing is answering at {url}, and no installation was found."),
                "If it runs elsewhere, use --ollama-url or set it in the config file.".to_owned(),
            ],
        },
        OllamaStatus::Running { .. } => ProviderState::Ready { detail: "Ollama".to_owned() },
    }
}

/// Makes a fresh connection to the current model, for a task's own conversation.
type ProviderFactory = Arc<dyn Fn() -> Result<Box<dyn Provider>, String> + Send + Sync>;

/// Messages typed while a turn runs, delivered after the next batch of tool calls or as the next turn.
type Queued = std::sync::Mutex<std::collections::VecDeque<(String, Vec<ImageData>)>>;

/// One conversation. The backend provider can be swapped (switching model) while the messages stay.
struct Chat {
    provider: Box<dyn Provider>,
    model: String,
    /// What requests are budgeted against (`recode_core::context`).
    window: ContextWindow,
    /// How the system message is made up (kind, characters), for the ledger.
    system_parts: Vec<(ComponentKind, usize)>,
    /// What `/compact <focus>` asked summaries to keep; stays until changed or a new conversation.
    compact_focus: Option<String>,
    /// The project's "Compact instructions" section, when it has one.
    compact_instructions: Option<String>,
    /// Characters per token for this model, learnt from reported usage.
    calibration: compact::Calibration,
    /// Things the user should hear about after the turn, such as a suspected truncation.
    warnings: Vec<String>,
    /// Set when the provider's figures suggest the context was cut: the next request compacts first.
    force_compact_next: bool,
    /// How many compactions this session has had, for numbering their records.
    compactions: u64,
    /// Summaries made during the current turn; a third one in one turn stops it (thrashing).
    compactions_this_turn: u8,
    /// Files the model read or changed most recently (latest last), re-read after a summary.
    recent_files: Vec<String>,
    /// Skills the model loaded (name, SKILL.md path), re-injected after a summary.
    invoked_skills: Vec<(String, String)>,
    /// Compaction records not yet written: the report, and the messages the summary replaced.
    compaction_records: Vec<(recode_core::usage::CompactionReport, Vec<Message>)>,
    /// Instruction files, rendered, as the system prompt's second part; refreshed by the driver each turn.
    instructions: Option<(ComponentKind, String)>,
    /// Fingerprint of the instructions last sent, to notice a change on disk.
    instructions_hash: u64,
    /// Subdirectory instruction files already loaded this session.
    nested_loaded: std::collections::BTreeSet<std::path::PathBuf>,
    /// Where each turn began: (messages before it, checkpoint turns before it), for rewinding.
    turn_marks: Vec<(usize, usize)>,
    messages: Vec<Message>,
    /// Things the model must be told before the next request, such as an undo it didn't see.
    notes: Vec<String>,
    /// Tool results shortened by compaction, by handle, for `read_result`.
    elided: BTreeMap<String, String>,
    /// The model's task list, kept by the `todo` tool.
    todos: Vec<recode_core::protocol::TodoItem>,
}

impl Chat {
    fn new(provider: OllamaProvider, num_ctx: u32) -> Self {
        Self::local(provider, num_ctx)
    }

    fn local(provider: OllamaProvider, num_ctx: u32) -> Self {
        Self {
            provider: Box::new(provider),
            model: String::new(),
            window: ContextWindow::resolve(&WindowSetting::default(), None, true, num_ctx),
            system_parts: vec![(ComponentKind::System, SYSTEM_PROMPT.chars().count())],
            compact_focus: None,
            compact_instructions: None,
            calibration: compact::Calibration::default(),
            warnings: Vec::new(),
            force_compact_next: false,
            compactions: 0,
            compactions_this_turn: 0,
            recent_files: Vec::new(),
            invoked_skills: Vec::new(),
            compaction_records: Vec::new(),
            instructions: None,
            instructions_hash: 0,
            nested_loaded: std::collections::BTreeSet::new(),
            turn_marks: Vec::new(),
            messages: vec![Message::system(SYSTEM_PROMPT)],
            notes: Vec::new(),
            elided: BTreeMap::new(),
            todos: Vec::new(),
        }
    }

    /// A fresh conversation on the same model for a task: the same window, calibration and instruction
    /// files, nothing of the history.
    fn nested(&self, provider: Box<dyn Provider>) -> Self {
        let mut nested = Self {
            provider,
            model: self.model.clone(),
            window: self.window.clone(),
            system_parts: vec![(ComponentKind::System, SYSTEM_PROMPT.chars().count())],
            compact_focus: None,
            compact_instructions: self.compact_instructions.clone(),
            calibration: self.calibration,
            warnings: Vec::new(),
            force_compact_next: false,
            compactions: 0,
            compactions_this_turn: 0,
            recent_files: Vec::new(),
            invoked_skills: Vec::new(),
            compaction_records: Vec::new(),
            instructions: self.instructions.clone(),
            instructions_hash: self.instructions_hash,
            nested_loaded: std::collections::BTreeSet::new(),
            turn_marks: Vec::new(),
            messages: vec![Message::system(SYSTEM_PROMPT)],
            notes: Vec::new(),
            elided: BTreeMap::new(),
            todos: Vec::new(),
        };
        nested.notes.push(
            "you are a helper doing one task for a larger session; you know nothing else about it. Report your \
             findings plainly and completely when you're done; don't ask questions"
                .to_owned(),
        );
        nested
    }

    /// Everything needed to continue this conversation later, exactly.
    fn saved(&self) -> SavedConversation {
        SavedConversation {
            model: self.model.clone(),
            messages: self.messages.clone(),
            notes: self.notes.clone(),
            elided: self.elided.clone(),
            todos: self.todos.clone(),
            compact_focus: self.compact_focus.clone(),
            compactions: self.compactions,
            recent_files: self.recent_files.clone(),
            invoked_skills: self.invoked_skills.clone(),
            turn_marks: self.turn_marks.clone(),
        }
    }

    /// Takes up a saved conversation (the model is chosen separately).
    fn restore(&mut self, saved: SavedConversation) {
        self.messages = saved.messages;
        self.notes = saved.notes;
        self.elided = saved.elided;
        self.todos = saved.todos;
        self.compact_focus = saved.compact_focus;
        self.compactions = saved.compactions;
        self.recent_files = saved.recent_files;
        self.invoked_skills = saved.invoked_skills;
        self.turn_marks = saved.turn_marks;
    }

    /// Starts afresh, keeping the model.
    fn reset(&mut self) {
        self.messages = vec![Message::system(SYSTEM_PROMPT)];
        self.notes.clear();
        self.elided.clear();
        self.todos.clear();
        self.compact_focus = None;
        self.force_compact_next = false;
        self.compactions = 0;
        self.compactions_this_turn = 0;
        self.recent_files.clear();
        self.invoked_skills.clear();
        self.compaction_records.clear();
        self.turn_marks.clear();
    }

    /// Whether anything has been said yet.
    fn is_empty(&self) -> bool {
        !self.messages.iter().any(|message| message.role == recode_core::message::Role::User)
    }

    fn options(&self) -> ProviderOptions {
        match self.window.local_num_ctx {
            Some(num_ctx) => {
                ProviderOptions::Ollama(OllamaOptions { num_ctx: Some(num_ctx), ..OllamaOptions::default() })
            }
            None => ProviderOptions::None,
        }
    }

    /// The tools to offer: the agent's, plus `read_result` once something has been shortened.
    fn offered_tools(&self, base: &[ToolDefinition]) -> Vec<ToolDefinition> {
        let mut tools = base.to_vec();
        if !base.is_empty() {
            tools.push(crate::todo::tool_definition());
        }
        if !base.is_empty() && !self.elided.is_empty() {
            tools.push(compact::read_result_tool());
        }
        tools
    }

    fn read_result(&self, call: &ToolCall) -> String {
        let handle = call.arguments.get("handle").and_then(|v| v.as_str()).unwrap_or_default().trim();
        let offset = call
            .arguments
            .get("offset")
            .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())))
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(1)
            .max(1);
        match self.elided.get(handle) {
            Some(original) => {
                let (text, total, next) = compact::page(original, offset, self.result_cap_chars().saturating_sub(300));
                if text.is_empty() {
                    return format!("Error: result {handle} has {total} lines; offset {offset} is past the end.");
                }
                match next {
                    Some(next) => format!(
                        "{text}[result {handle}: lines {offset}–{} of {total}; call read_result with \"{handle}\" and offset \
                         {next} for more]",
                        next - 1
                    ),
                    None => text,
                }
            }
            None => {
                let known: Vec<&str> = self.elided.keys().map(String::as_str).collect();
                format!("Error: there's no shortened result called '{handle}'. Known handles: {}.", known.join(", "))
            }
        }
    }

    /// The most one tool result may hold in the conversation: a quarter of the request budget, so a
    /// few results and the rest of the conversation still fit. Without a window, the read tool's own cap.
    fn result_cap_chars(&self) -> usize {
        match compact::Limits::for_window(&self.window) {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]
            Some(limits) => ((limits.budget / 4) as f64 * self.calibration.ratio()) as usize,
            None => 64 * 1024,
        }
        .max(2_000)
    }

    /// Makes sure the next request fits the model's context window (`docs/CONTEXT_ENGINE.md`). From the
    /// window's trigger point it shortens old tool results, then summarises older messages if that wasn't
    /// enough; `force` compacts now regardless. Returns an error if the request still won't fit, in which
    /// case nothing should be sent. With no window known (a cloud model whose listing gives no limit and
    /// no setting), nothing happens on its own; a forced compaction aims at half of today's size.
    async fn fit(
        &mut self,
        tools: &[ToolDefinition],
        workspace: Option<&Workspace>,
        force: bool,
        cancel: &CancellationToken,
        emit: &mut impl FnMut(EventKind),
    ) -> Result<(), String> {
        let force = force || std::mem::take(&mut self.force_compact_next);
        let ratio = self.calibration.ratio();
        // Before anything else, on every request: no single result may take more than its share.
        let cap = self.result_cap_chars();
        compact::clip(&mut self.messages, &mut self.elided, cap);
        // Measured against the tools the request would carry, so before and after compare like with like.
        let before = compact::estimate_with(&self.messages, &self.offered_tools(tools), ratio);
        let limits = match compact::Limits::for_window(&self.window) {
            Some(limits) => limits,
            None if force => compact::Limits { budget: before.max(1), trigger: None },
            None => return Ok(()),
        };
        if !force && limits.trigger.is_none_or(|trigger| before <= trigger) {
            return Ok(());
        }
        emit(EventKind::StateChanged { state: AgentState::Compacting });
        let mut elided = compact::elide(&mut self.messages, &mut self.elided);
        let tools = self.offered_tools(tools);
        let (mut summarised, mut problem) = (0, None);
        let mut originals: Vec<Message> = Vec::new();
        let needs_summary =
            compact::estimate_with(&self.messages, &tools, ratio) > limits.target() || (force && elided == 0);
        if needs_summary && let Some(region) = compact::summary_region(&self.messages) {
            match self.summarise(&region, limits, cancel).await {
                Ok(summary) => {
                    let removed = compact::apply_summary_keeping(&mut self.messages, &region, &summary);
                    summarised = removed.len();
                    originals = removed;
                    self.compactions_this_turn = self.compactions_this_turn.saturating_add(1);
                    self.reinject(workspace, ratio);
                }
                Err(error) => {
                    problem = Some(format!("Summarising didn't work ({error}), so only the shortening was kept."));
                }
            }
        }
        // Still too big: the newest results go behind handles too. They stay a read_result away, and a
        // conversation that can't be sent at all helps nobody.
        if compact::estimate_with(&self.messages, &tools, ratio) > limits.budget {
            elided += compact::elide_keeping(&mut self.messages, &mut self.elided, 0);
        }
        let after = compact::estimate_with(&self.messages, &tools, ratio);
        emit(EventKind::Compacted {
            before_tokens: before,
            after_tokens: after,
            elided,
            summarised,
            problem: problem.clone(),
            focus: self.compact_focus.clone(),
        });
        if elided > 0 || summarised > 0 {
            self.compactions += 1;
            let kept_objective = self
                .messages
                .iter()
                .find(|m| m.role == recode_core::message::Role::User)
                .map(|m| m.content.chars().take(200).collect())
                .unwrap_or_default();
            let report = recode_core::usage::CompactionReport {
                seq: self.compactions,
                at_ms: recode_core::ids::UnixMillis::now().0,
                before_tokens: before,
                after_tokens: after,
                elided,
                summarised,
                focus: self.compact_focus.clone(),
                problem,
                kept_objective,
            };
            self.compaction_records.push((report, originals));
        }
        if self.compactions_this_turn >= 3 {
            return Err(
                "This turn was compacted three times and still fills the context: a single step produces more \
                 than fits. Start a new conversation, or raise the context size on the Settings tab."
                    .to_owned(),
            );
        }
        if after > limits.budget
            && let Some(window) = self.window.window
        {
            return Err(format!(
                "This message wasn't sent: the conversation is about {after} tokens (estimated), more than fits in the \
                 {window}-token context even after compacting. Start a new conversation (Ctrl+K, then \"New conversation\")."
            ));
        }
        Ok(())
    }

    /// Asks the current model to summarise part of the conversation, without tools.
    async fn summarise(
        &self,
        region: &compact::Region,
        limits: compact::Limits,
        cancel: &CancellationToken,
    ) -> Result<String, String> {
        let max_chars =
            usize::try_from(limits.budget).unwrap_or(usize::MAX).saturating_mul(compact::CHARS_PER_TOKEN) / 2;
        let transcript = compact::render(&self.messages, region, max_chars);
        let objective = &self.messages[region.objective].content;
        let request = ChatRequest {
            model: self.model.clone(),
            messages: vec![
                Message::system(compact::summary_system(
                    self.compact_focus.as_deref(),
                    self.compact_instructions.as_deref(),
                )),
                Message::user(format!(
                    "The user's original request:\n{objective}\n\nThe conversation to summarise:\n{transcript}"
                )),
            ],
            tools: Vec::new(),
            options: self.options(),
        };
        let mut stream = self.provider.stream(request, cancel.clone());
        let mut summary = String::new();
        while let Some(event) = stream.next().await {
            match event.map_err(|error| error.to_string())? {
                StreamEvent::TextDelta(text) => summary.push_str(&text),
                StreamEvent::Finished(FinishReason::Cancelled) => return Err("it was interrupted".to_owned()),
                StreamEvent::Finished(FinishReason::Length) => return Err("the summary was cut off".to_owned()),
                _ => {}
            }
        }
        if summary.trim().is_empty() {
            return Err("the model returned nothing".to_owned());
        }
        Ok(summary)
    }

    /// After a summary, puts back what the model would otherwise have to rediscover: its task list,
    /// the files it was working on (re-read from disk, so they're current; outside the project by
    /// path only, since a re-read would bypass the approval the user gave once), and the skills it
    /// had loaded. Everything arrives as notes in front of the next request; nothing is invented.
    fn reinject(&mut self, workspace: Option<&Workspace>, ratio: f64) {
        const FILE_CAP_TOKENS: u64 = 5_000;
        const SKILL_CAP_TOKENS: u64 = 5_000;
        const SKILLS_TOTAL_CAP_TOKENS: u64 = 25_000;
        if !self.todos.is_empty() {
            self.notes.push(format!("your task list:\n{}", crate::todo::describe(&self.todos)));
        }
        let files: Vec<String> = self.recent_files.iter().rev().take(5).cloned().collect();
        for path in files {
            let Some(workspace) = workspace else {
                self.notes.push(format!("you were working on {path}; read it again if you need it"));
                continue;
            };
            match workspace.locate(&path) {
                Ok(location @ recode_tools::Location::Inside(_)) => match workspace.read_existing_at(&location) {
                    Ok(Some(bytes)) => {
                        let text = String::from_utf8_lossy(&bytes);
                        if compact::tokens_at(text.chars().count(), ratio) <= FILE_CAP_TOKENS {
                            self.notes.push(format!("after compacting, {path} as it is now:\n{text}"));
                        } else {
                            self.notes.push(format!(
                                "you were working on {path}; it's too long to repeat here, so read the part you need"
                            ));
                        }
                    }
                    Ok(None) => self.notes.push(format!("you were working on {path}; it no longer exists")),
                    Err(_) => self.notes.push(format!("you were working on {path}; read it again if you need it")),
                },
                _ => self.notes.push(format!("you were working on {path}; read it again if you need it")),
            }
        }
        let mut skill_budget = SKILLS_TOTAL_CAP_TOKENS;
        let skills: Vec<(String, String)> = self.invoked_skills.iter().rev().cloned().collect();
        for (name, path) in skills {
            let body = recode_extensions::skills::read_body(std::path::Path::new(&path)).unwrap_or_default();
            let tokens = compact::tokens_at(body.chars().count(), ratio);
            if body.is_empty() || tokens > SKILL_CAP_TOKENS || tokens > skill_budget {
                self.notes.push(format!("you had loaded the skill `{name}`; load it again with the skill tool"));
                continue;
            }
            skill_budget -= tokens;
            self.notes.push(format!("you had loaded the skill `{name}`; its instructions again:\n{body}"));
        }
    }

    /// Runs a `task` call: a nested conversation with its own context and a reading tool set, sharing
    /// the user's permissions, approvals and checkpoints. Returns what to show and give back, and
    /// whether it got an answer.
    // Boxed and declared `Send` so the recursion (a turn runs a task runs a turn) type-checks.
    #[allow(clippy::too_many_arguments)]
    fn run_task<'a, E: FnMut(EventKind) + Send>(
        &'a mut self,
        call: &'a ToolCall,
        workspace: &'a Workspace,
        config: &'a AgentConfig,
        hub: &'a mut McpHub,
        permissions: &'a mut Permissions,
        checkpoints: &'a mut Checkpoints,
        approvals: &'a mut UnboundedReceiver<(ApprovalId, ApprovalDecision)>,
        background: &'a mut crate::background::BackgroundTasks,
        cancel: &'a CancellationToken,
        emit: &'a mut E,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = (String, bool)> + Send + 'a>> {
        Box::pin(async move {
            if config.depth > 0 {
                return ("Error: a task can't start another task. Do this part yourself.".to_owned(), false);
            }
            let (description, prompt, requested) = match crate::subagent::arguments(call) {
                Ok(parts) => parts,
                Err(reason) => return (reason, false),
            };
            let Some(factory) = &config.spawn_provider else {
                return ("Error: tasks aren't available for this model here.".to_owned(), false);
            };
            let provider = match factory() {
                Ok(provider) => provider,
                Err(error) => return (format!("Error: couldn't open a connection for the task: {error}"), false),
            };
            let mut nested = self.nested(provider);
            let custom = match &requested {
                Some(names) => config
                    .custom
                    .iter()
                    .filter(|(name, _)| names.contains(name))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
                None => HashMap::new(),
            };
            let nested_config = AgentConfig {
                tools: crate::subagent::nested_tools(&config.tools, requested.as_deref()),
                custom,
                extra_system: config.extra_system.clone(),
                skills: config.skills.clone(),
                invoked_text: None,
                project_trusted: config.project_trusted,
                plan_path: config.plan_path.clone(),
                max_steps: crate::subagent::MAX_STEPS,
                hooks: config.hooks.clone(),
                spawn_provider: None,
                depth: config.depth + 1,
                memory: config.memory.clone().map(|memory| MemoryConfig { read_only: true, ..memory }),
            };
            let mut steps: Vec<String> = vec![format!("Task: {description}")];
            let mut answer = String::new();
            let mut finished = None;
            {
                // The helper's words stay here; its tool calls, approvals and state reach the user as
                // rows of their own (an approval card needs a row to sit on), and the task's own row
                // lists them again when it ends.
                let mut forward = |kind: EventKind| match kind {
                    EventKind::TextDelta { text } => answer.push_str(&text),
                    EventKind::ToolCallRequested { call } => {
                        steps.push(format!("→ {} {}", call.name, target_of(&call.name, &call.arguments)));
                        emit(EventKind::ToolCallRequested { call });
                    }
                    EventKind::ToolFinished { call_id, success, summary } => {
                        steps.push(format!("  {} {summary}", if success { "✓" } else { "✗" }));
                        emit(EventKind::ToolFinished { call_id, success, summary });
                    }
                    EventKind::TurnFinished { outcome, .. } => finished = Some(outcome),
                    EventKind::ToolOutput { .. }
                    | EventKind::ApprovalRequired { .. }
                    | EventKind::ApprovalResolved { .. }
                    | EventKind::StateChanged { .. }
                    | EventKind::UsageReported { .. }
                    | EventKind::HookRan { .. } => emit(kind),
                    _ => {}
                };
                // Type-erased, so each level of recursion is the same `agent_turn` instance.
                let forward: &mut (dyn FnMut(EventKind) + Send) = &mut forward;
                let queued = Queued::default();
                nested
                    .agent_turn(
                        prompt,
                        Vec::new(),
                        workspace,
                        &nested_config,
                        hub,
                        permissions,
                        checkpoints,
                        approvals,
                        &queued,
                        background,
                        cancel.child_token(),
                        forward,
                    )
                    .await;
            }
            let answer = answer.trim().to_owned();
            let outcome = match finished {
                Some(TurnOutcome::Completed) if !answer.is_empty() => Ok(()),
                Some(TurnOutcome::Completed) => Err("the task finished without an answer".to_owned()),
                Some(TurnOutcome::Interrupted) => Err("the task was interrupted".to_owned()),
                Some(TurnOutcome::Failed { reason }) => Err(format!("the task failed: {reason}")),
                None => Err("the task didn't finish".to_owned()),
            };
            let mut text = steps.join("\n");
            match outcome {
                Ok(()) => {
                    text.push_str("\n\nResult:\n");
                    text.push_str(&answer);
                    (text, true)
                }
                Err(reason) => {
                    text.push_str(&format!("\n\nError: {reason}."));
                    if !answer.is_empty() {
                        text.push_str("\nWhat it had so far:\n");
                        text.push_str(&answer);
                    }
                    (text, false)
                }
            }
        })
    }

    /// Winds the conversation back to before request `to_turn` (from 0). With `summarize`, the removed
    /// part is replaced by a summary from the model, so what was learnt isn't lost. Returns the
    /// checkpoint-turn count to keep and the summary, or why it couldn't.
    async fn rewind(
        &mut self,
        to_turn: usize,
        summarize: bool,
        cancel: &CancellationToken,
    ) -> Result<(usize, Option<String>), String> {
        let Some(&(messages_before, checkpoint_turns)) = self.turn_marks.get(to_turn) else {
            return Err(format!("There's no request {} in this conversation to go back to.", to_turn + 1));
        };
        let removed = self.messages.split_off(messages_before.min(self.messages.len()));
        self.turn_marks.truncate(to_turn);
        let mut summary = None;
        if summarize && !removed.is_empty() {
            let text = self.summarise_messages(&removed, cancel).await?;
            self.messages
                .push(Message::assistant(format!("{}\n\n{}", compact::SUMMARY_PREFIX, text.trim()), Vec::new()));
            summary = Some(text);
        }
        self.notes.push(if summary.is_some() {
            "the user wound the conversation back and kept a summary of what was removed".to_owned()
        } else {
            "the user wound the conversation back; the removed part is gone".to_owned()
        });
        Ok((checkpoint_turns, summary))
    }

    /// Asks the model to summarise these messages, with the usual headings and the saved focus.
    async fn summarise_messages(&self, messages: &[Message], cancel: &CancellationToken) -> Result<String, String> {
        let transcript = compact::render_messages(messages);
        let request = ChatRequest {
            model: self.model.clone(),
            messages: vec![
                Message::system(compact::summary_system(
                    self.compact_focus.as_deref(),
                    self.compact_instructions.as_deref(),
                )),
                Message::user(format!("The part of the conversation to summarise:\n{transcript}")),
            ],
            tools: Vec::new(),
            options: self.options(),
        };
        let mut stream = self.provider.stream(request, cancel.clone());
        let mut summary = String::new();
        while let Some(event) = stream.next().await {
            match event.map_err(|error| error.to_string())? {
                StreamEvent::TextDelta(text) => summary.push_str(&text),
                StreamEvent::Finished(FinishReason::Cancelled) => return Err("it was interrupted".to_owned()),
                StreamEvent::Finished(FinishReason::Length) => return Err("the summary was cut off".to_owned()),
                _ => {}
            }
        }
        if summary.trim().is_empty() {
            return Err("the model returned nothing".to_owned());
        }
        Ok(summary)
    }

    /// Whether the conversation is over half of its budget, so an idle compaction would help.
    fn compaction_is_due(&self) -> bool {
        match compact::Limits::for_window(&self.window) {
            Some(limits) => compact::estimate_with(&self.messages, &[], self.calibration.ratio()) > limits.target(),
            None => false,
        }
    }

    /// Remembers a file the model read or changed, latest last, at most ten.
    fn note_file(&mut self, path: String) {
        if path.is_empty() {
            return;
        }
        self.recent_files.retain(|known| *known != path);
        self.recent_files.push(path);
        if self.recent_files.len() > 10 {
            self.recent_files.remove(0);
        }
    }

    /// Puts any pending notes in front of the user's next message, so the model sees them in order.
    fn with_notes(&mut self, text: String) -> String {
        if self.notes.is_empty() {
            return text;
        }
        let notes: Vec<String> = self.notes.drain(..).map(|note| format!("[re-code: {note}]")).collect();
        format!("{}\n\n{text}", notes.join("\n"))
    }

    /// Switches the model, keeping the conversation so far.
    fn set_backend(&mut self, provider: Box<dyn Provider>, model: String, window: ContextWindow) {
        self.provider = provider;
        self.model = model;
        self.window = window;
    }

    /// Sets the system message from its parts, in a fixed order, and remembers how it's made up so
    /// `/context` can say what each part costs. Parts are joined by a blank line.
    fn set_system_parts(&mut self, parts: &[(ComponentKind, String)]) {
        let mut prompt = String::new();
        let mut recorded = Vec::with_capacity(parts.len());
        for (index, (kind, text)) in parts.iter().enumerate() {
            if index > 0 {
                prompt.push_str("\n\n");
            }
            let separator = if index > 0 { 2 } else { 0 };
            prompt.push_str(text);
            recorded.push((*kind, text.chars().count() + separator));
        }
        self.system_parts = recorded;
        match self.messages.first_mut() {
            Some(first) if first.role == recode_core::message::Role::System => first.content = prompt,
            _ => self.messages.insert(0, Message::system(prompt)),
        }
    }

    /// What the next request would hold, by part (estimated).
    fn ledger(&self, tools: &[ToolDefinition]) -> recode_core::usage::Ledger {
        compact::ledger(&self.messages, tools, &self.system_parts, &self.window, &self.calibration, &self.model)
    }

    /// Learns from the provider's figures for a request of `request_chars` characters, and notices
    /// when a local runtime seems to have cut the prompt: far fewer tokens than any plausible count of
    /// the text sent, with nothing served from cache to explain it. Compaction then runs before the
    /// next request, and the user is told. Samples that look cut aren't learnt from.
    fn learn(&mut self, usage: &recode_core::usage::ReportedUsage, request_chars: usize) {
        let Some(prompt) = usage.prompt_tokens else { return };
        let cached = usage.cached_prompt_tokens.unwrap_or(0);
        if cached > 0 || prompt < 200 {
            return;
        }
        // The most generous plausible count of what was sent: six characters per token.
        let least_plausible = compact::tokens_at(request_chars, 6.0);
        if self.window.local_num_ctx.is_some() && prompt >= 1000 && prompt * 3 < least_plausible {
            self.warnings.push(format!(
                "{} reported {prompt} prompt tokens, but what re-code sent was at least ~{least_plausible} \
                 tokens, so the oldest part of the conversation was probably dropped by the runtime. The \
                 next request is compacted first; lower the context size on the Settings tab if this keeps \
                 happening.",
                self.model
            ));
            self.force_compact_next = true;
            return;
        }
        self.calibration.add(request_chars, prompt);
    }

    /// The next request as it would go out, as readable JSON with secrets hidden, for `/context raw`.
    fn request_preview(&self, tools: &[ToolDefinition]) -> String {
        let options = match self.window.local_num_ctx {
            Some(num_ctx) => serde_json::json!({"num_ctx": num_ctx}),
            None => serde_json::Value::Null,
        };
        let request = serde_json::json!({
            "model": self.model,
            "messages": self.messages,
            "tools": tools,
            "options": options,
        });
        compact::redact(&serde_json::to_string_pretty(&request).unwrap_or_default())
    }

    #[cfg(test)]
    async fn run_turn(&mut self, text: String, cancel: CancellationToken, emit: impl FnMut(EventKind)) {
        self.run_turn_with_images(text, Vec::new(), cancel, emit).await;
    }

    async fn run_turn_with_images(
        &mut self,
        text: String,
        images: Vec<ImageData>,
        cancel: CancellationToken,
        mut emit: impl FnMut(EventKind),
    ) {
        let turn = TurnId::new();
        self.compactions_this_turn = 0;
        let checkpoint_turns = self.turn_marks.last().map_or(0, |(_, turns)| *turns);
        self.turn_marks.push((self.messages.len(), checkpoint_turns));
        emit(EventKind::TurnStarted { turn, text: text.clone() });
        let mut parts = vec![(ComponentKind::System, SYSTEM_PROMPT.to_owned())];
        parts.extend(self.instructions.clone());
        self.set_system_parts(&parts);
        let text = self.with_notes(text);
        self.messages.push(Message::user_with_images(text, images));

        // Ollama silently drops the oldest messages over its context, so an oversized local request is
        // compacted first and, if it still doesn't fit, never sent.
        if let Err(reason) = self.fit(&[], None, false, &cancel, &mut emit).await {
            self.messages.pop();
            emit(EventKind::TurnFinished { turn, outcome: TurnOutcome::Failed { reason } });
            return;
        }
        let ledger = self.ledger(&[]);
        // 0 means "no known limit" to the interface.
        let limit = self.window.window.unwrap_or(0);
        emit(EventKind::ContextMeasured { used: ledger.total.clone(), limit });
        emit(EventKind::ContextLedger { ledger });

        // A local model may need loading; a cloud request just goes out.
        let waiting =
            if self.window.local_num_ctx.is_some() { AgentState::LoadingModel } else { AgentState::EvaluatingPrompt };
        emit(EventKind::StateChanged { state: waiting });

        let options = match self.window.local_num_ctx {
            Some(num_ctx) => {
                ProviderOptions::Ollama(OllamaOptions { num_ctx: Some(num_ctx), ..OllamaOptions::default() })
            }
            None => ProviderOptions::None,
        };
        let request_chars = compact::chars_of(&self.messages, &[]);
        let request =
            ChatRequest { model: self.model.clone(), messages: self.messages.clone(), tools: Vec::new(), options };
        let mut stream = self.provider.stream(request, cancel);
        let mut reply = String::new();
        let mut state_is_text = false;
        let mut thinking = false;
        let mut outcome = TurnOutcome::Failed { reason: "The response ended unexpectedly.".to_owned() };
        while let Some(event) = stream.next().await {
            match event {
                Ok(StreamEvent::ReasoningDelta(delta)) => {
                    if !thinking {
                        thinking = true;
                        emit(EventKind::StateChanged { state: AgentState::Thinking });
                    }
                    emit(EventKind::ReasoningDelta { text: delta });
                }
                Ok(StreamEvent::TextDelta(delta)) => {
                    if !state_is_text {
                        state_is_text = true;
                        emit(EventKind::StateChanged { state: AgentState::Responding });
                    }
                    reply.push_str(&delta);
                    emit(EventKind::TextDelta { text: delta });
                }
                // None are offered in this chat-only session, so a call here can't be acted on.
                Ok(StreamEvent::ToolCall(call)) => tracing::warn!(tool = %call.name, "unexpected tool call"),
                Ok(StreamEvent::Usage(usage)) => {
                    self.learn(&usage, request_chars);
                    if let Some(tokens) = usage.prompt_tokens {
                        let used = TokenCount::Exact { tokens, tokenizer: "reported by the provider".to_owned() };
                        emit(EventKind::ContextMeasured { used, limit });
                    }
                    emit(EventKind::UsageReported { usage });
                }
                Ok(StreamEvent::Finished(reason)) => {
                    outcome = match reason {
                        FinishReason::Stop => TurnOutcome::Completed,
                        FinishReason::Cancelled => TurnOutcome::Interrupted,
                        FinishReason::Length => TurnOutcome::Failed {
                            reason: "The response was cut off because it reached the length limit.".to_owned(),
                        },
                        FinishReason::Other(other) => {
                            TurnOutcome::Failed { reason: format!("The model stopped early: {other}") }
                        }
                    };
                }
                Err(error) => outcome = TurnOutcome::Failed { reason: error.to_string() },
            }
        }
        // Whatever arrived stays in the conversation, so a stopped reply can still be discussed.
        if !reply.is_empty() {
            self.messages.push(Message::assistant(reply, Vec::new()));
        }
        emit(EventKind::TurnFinished { turn, outcome });
    }

    /// Runs a turn with tools: the model may call tools, which run (after approval where needed) and
    /// feed their results back, looping until the model is done. Local Ollama models only, for now.
    #[allow(clippy::too_many_arguments)]
    async fn agent_turn(
        &mut self,
        text: String,
        images: Vec<ImageData>,
        workspace: &Workspace,
        config: &AgentConfig,
        hub: &mut McpHub,
        permissions: &mut Permissions,
        checkpoints: &mut Checkpoints,
        approvals: &mut UnboundedReceiver<(ApprovalId, ApprovalDecision)>,
        queued: &Queued,
        background: &mut crate::background::BackgroundTasks,
        cancel: CancellationToken,
        mut emit: impl FnMut(EventKind) + Send,
    ) {
        let turn = TurnId::new();
        self.compactions_this_turn = 0;
        emit(EventKind::TurnStarted { turn, text: text.clone() });
        self.turn_marks.push((self.messages.len(), checkpoints.turn_count()));
        checkpoints.begin_turn(&text);
        let text = self.with_notes(text);
        let mut system = AGENT_SYSTEM_PROMPT.to_owned();
        // Stable for the session, so it doesn't disturb the prompt cache.
        system.push_str(&format!("\n\nThe project folder is {}.", workspace.root().display()));
        if let Some(home) = std::env::var_os("HOME") {
            system.push_str(&format!(" The home folder (~) is {}.", std::path::Path::new(&home).display()));
        }
        let mut parts = vec![(ComponentKind::System, system)];
        parts.extend(self.instructions.clone());
        parts.extend(config.extra_system.iter().cloned());
        self.set_system_parts(&parts);
        self.messages.push(Message::user_with_images(text, images));
        let limit = self.window.window.unwrap_or(0);
        let options = self.options();
        let mut outcome = TurnOutcome::Completed;

        'turn: for step in 0..config.max_steps {
            if cancel.is_cancelled() {
                outcome = TurnOutcome::Interrupted;
                break;
            }
            // The mode decides which tools are even offered (plan mode: reading ones and `plan`).
            let tools = permissions.filter_tools(self.offered_tools(&config.tools));
            // Every request is checked before it goes out; a long turn is compacted as it grows.
            if let Err(reason) = self.fit(&tools, Some(workspace), false, &cancel, &mut emit).await {
                if step == 0 {
                    self.messages.pop();
                }
                outcome = TurnOutcome::Failed { reason };
                break;
            }
            emit(EventKind::ContextLedger { ledger: self.ledger(&tools) });
            if cancel.is_cancelled() {
                outcome = TurnOutcome::Interrupted;
                break;
            }
            emit(EventKind::StateChanged { state: AgentState::EvaluatingPrompt });
            let request_chars = compact::chars_of(&self.messages, &tools);
            let request = ChatRequest {
                model: self.model.clone(),
                messages: self.messages.clone(),
                tools: tools.clone(),
                options: options.clone(),
            };
            let mut stream = self.provider.stream(request, cancel.clone());
            let mut reply = String::new();
            let mut calls: Vec<ToolCall> = Vec::new();
            let mut state_is_text = false;
            let mut thinking = false;
            let mut finish = FinishReason::Stop;
            let mut cancelled = false;
            while let Some(event) = stream.next().await {
                match event {
                    Ok(StreamEvent::ReasoningDelta(delta)) => {
                        if !thinking {
                            thinking = true;
                            emit(EventKind::StateChanged { state: AgentState::Thinking });
                        }
                        emit(EventKind::ReasoningDelta { text: delta });
                    }
                    Ok(StreamEvent::TextDelta(delta)) => {
                        if !state_is_text {
                            state_is_text = true;
                            emit(EventKind::StateChanged { state: AgentState::Responding });
                        }
                        reply.push_str(&delta);
                        emit(EventKind::TextDelta { text: delta });
                    }
                    Ok(StreamEvent::ToolCall(call)) => calls.push(call),
                    Ok(StreamEvent::Usage(usage)) => {
                        self.learn(&usage, request_chars);
                        if let Some(tokens) = usage.prompt_tokens {
                            let used = TokenCount::Exact { tokens, tokenizer: "reported by the provider".to_owned() };
                            emit(EventKind::ContextMeasured { used, limit });
                        }
                        emit(EventKind::UsageReported { usage });
                    }
                    Ok(StreamEvent::Finished(reason)) => {
                        cancelled = matches!(reason, FinishReason::Cancelled);
                        finish = reason;
                    }
                    Err(error) => {
                        outcome = TurnOutcome::Failed { reason: error.to_string() };
                        break 'turn;
                    }
                }
            }
            drop(stream);
            if cancelled {
                outcome = TurnOutcome::Interrupted;
                break;
            }
            self.messages.push(Message::assistant(reply.clone(), calls.clone()));
            if calls.is_empty() {
                outcome = finish_outcome(&finish);
                break;
            }

            for call in calls {
                emit(EventKind::ToolCallRequested { call: call.clone() });
                if call.name == crate::todo::TOOL {
                    let (text, success) = match crate::todo::parse(&call) {
                        Ok(items) => {
                            self.todos = items;
                            emit(EventKind::TodoUpdated { items: self.todos.clone() });
                            (format!("Task list updated:\n{}", crate::todo::describe(&self.todos)), true)
                        }
                        Err(error) => (format!("Error: {error}"), false),
                    };
                    emit(EventKind::ToolOutput { call_id: call.id.clone(), text: text.clone() });
                    let summary = if success { format!("{} tasks", self.todos.len()) } else { "invalid".to_owned() };
                    emit(EventKind::ToolFinished { call_id: call.id.clone(), success, summary });
                    self.messages.push(Message::tool_result(&call, text));
                    continue;
                }
                if call.name == crate::subagent::TOOL {
                    let (text, success) = self
                        .run_task(
                            &call,
                            workspace,
                            config,
                            hub,
                            permissions,
                            checkpoints,
                            approvals,
                            background,
                            &cancel,
                            &mut emit,
                        )
                        .await;
                    // The helper's steps already showed as rows of their own; its row shows the outcome.
                    let shown = text.split_once("\n\n").map_or(text.as_str(), |(_, outcome)| outcome);
                    emit(EventKind::ToolOutput { call_id: call.id.clone(), text: shown.to_owned() });
                    let summary = if success { "task done" } else { "task failed" }.to_owned();
                    emit(EventKind::ToolFinished { call_id: call.id.clone(), success, summary });
                    self.messages.push(Message::tool_result(&call, crate::subagent::cap_result(&text)));
                    continue;
                }
                if call.name == crate::background::OUTPUT_TOOL || call.name == crate::background::KILL_TOOL {
                    let (text, success) = background.run_tool(&call);
                    emit(EventKind::ToolOutput { call_id: call.id.clone(), text: text.clone() });
                    let summary = if success { "done" } else { "unknown task" }.to_owned();
                    emit(EventKind::ToolFinished { call_id: call.id.clone(), success, summary });
                    self.messages.push(Message::tool_result(&call, text));
                    continue;
                }
                if call.name == crate::memory::TOOL && !crate::memory::writes(&call) {
                    let (text, success, summary) = match &config.memory {
                        Some(memory) => {
                            let read_only = memory.read_only || permissions.mode == PermissionMode::Plan;
                            let (text, success, summary, _) = crate::memory::run(&memory.places, read_only, &call);
                            (text, success, summary)
                        }
                        None => ("Error: memory is off for this session.".to_owned(), false, "off".to_owned()),
                    };
                    emit(EventKind::ToolOutput { call_id: call.id.clone(), text: text.clone() });
                    emit(EventKind::ToolFinished { call_id: call.id.clone(), success, summary });
                    self.messages.push(Message::tool_result(&call, text));
                    continue;
                }
                // A save or forget that can't happen is answered without asking the user.
                if call.name == crate::memory::TOOL
                    && let Some(reason) = match &config.memory {
                        None => Some("Error: memory is off for this session.".to_owned()),
                        Some(memory) if memory.read_only || permissions.mode == PermissionMode::Plan => Some(
                            "Error: memory is read-only here (plan mode, a helper task, or the user set it so). \
                             Nothing was saved."
                                .to_owned(),
                        ),
                        Some(_) => None,
                    }
                {
                    emit(EventKind::ToolOutput { call_id: call.id.clone(), text: reason.clone() });
                    emit(EventKind::ToolFinished {
                        call_id: call.id.clone(),
                        success: false,
                        summary: "refused".to_owned(),
                    });
                    self.messages.push(Message::tool_result(&call, reason));
                    continue;
                }
                if call.name == crate::plan_tool::TOOL {
                    let (text, success) = crate::plan_tool::run(&config.plan_path, &call);
                    emit(EventKind::ToolOutput { call_id: call.id.clone(), text: text.clone() });
                    let summary = if success { "plan written" } else { "invalid" }.to_owned();
                    emit(EventKind::ToolFinished { call_id: call.id.clone(), success, summary });
                    self.messages.push(Message::tool_result(&call, text));
                    continue;
                }
                if call.name == crate::skill_tool::TOOL {
                    let (text, success) = crate::skill_tool::run(&config.skills, &call);
                    if success
                        && call.arguments.get("path").is_none()
                        && let Some(name) = call.arguments.get("name").and_then(|v| v.as_str())
                        && let Some(skill) = config.skills.iter().find(|s| s.name == name)
                    {
                        self.invoked_skills.retain(|(known, _)| known != name);
                        self.invoked_skills.push((name.to_owned(), skill.path.clone()));
                    }
                    emit(EventKind::ToolOutput { call_id: call.id.clone(), text: text.clone() });
                    let summary = if success { "loaded" } else { "unknown skill" }.to_owned();
                    emit(EventKind::ToolFinished { call_id: call.id.clone(), success, summary });
                    self.messages.push(Message::tool_result(&call, text));
                    continue;
                }
                if call.name == compact::READ_RESULT {
                    let text = self.read_result(&call);
                    let success = !text.starts_with("Error:");
                    emit(EventKind::ToolOutput { call_id: call.id.clone(), text: text.clone() });
                    let summary = if success { "restored" } else { "unknown handle" }.to_owned();
                    emit(EventKind::ToolFinished { call_id: call.id.clone(), success, summary });
                    self.messages.push(Message::tool_result(&call, text));
                    continue;
                }
                // The user's pre-tool hooks see the call first: they can block it, change it or add context.
                let mut call = call;
                let mut hook_context: Vec<String> = Vec::new();
                let mut blocked_by_hook: Option<String> = None;
                if !config.hooks.is_empty() {
                    let payload = json!({
                        "event": "pre_tool_use",
                        "project": workspace.root(),
                        "mode": permissions.mode.label(),
                        "tool": {"name": call.name, "arguments": call.arguments, "call_id": call.id},
                    });
                    let grants = permissions.sandbox_grants();
                    let outcomes = config.hooks.run(
                        crate::hooks::HookEvent::PreToolUse,
                        Some(&call.name),
                        &payload,
                        Some(workspace),
                        &grants,
                    );
                    for outcome in outcomes {
                        emit(EventKind::HookRan {
                            name: outcome.name.clone(),
                            event: "pre_tool_use".to_owned(),
                            ok: outcome.ok,
                            summary: outcome.summary.clone(),
                        });
                        if let Some(reason) = outcome.block {
                            blocked_by_hook.get_or_insert(format!("the hook {} blocked this: {reason}", outcome.name));
                        }
                        if let Some(updated) = outcome.updated_input {
                            call.arguments = updated;
                        }
                        if let Some(context) = outcome.add_context {
                            hook_context.push(format!("[hook {}] {context}", outcome.name));
                        }
                    }
                }
                if let Some(reason) = blocked_by_hook {
                    emit(EventKind::ToolFinished {
                        call_id: call.id.clone(),
                        success: false,
                        summary: "blocked by a hook".to_owned(),
                    });
                    self.messages.push(Message::tool_result(&call, reason));
                    continue;
                }
                let reach = reach(&call, workspace, &config.custom);
                let decision = permissions.decide(&call, &reach);
                let result = if let Decision::Refuse(reason) = decision {
                    emit(EventKind::ToolFinished {
                        call_id: call.id.clone(),
                        success: false,
                        summary: "not in this mode".to_owned(),
                    });
                    reason
                } else if decision == Decision::Run {
                    let grants = permissions.sandbox_grants();
                    if call.name == crate::memory::TOOL {
                        run_memory(&call, config.memory.as_ref(), &mut emit)
                    } else {
                        run_or_background(
                            &call,
                            &config.custom,
                            workspace,
                            hub,
                            checkpoints,
                            &grants,
                            &cancel,
                            &mut emit,
                            background,
                        )
                        .await
                    }
                } else {
                    let approval = ApprovalId::new();
                    let (description, diff) = if let Some(tool) = config.custom.get(&call.name) {
                        (
                            format!(
                                "Run this custom tool {}:\n{}",
                                where_it_runs(tool.network),
                                resolve_custom_command(tool, &call.arguments).join(" ")
                            ),
                            None,
                        )
                    } else if is_mcp_call(&call.name) {
                        (describe_mcp_call(&call), None)
                    } else if call.name == crate::memory::TOOL {
                        (crate::memory::describe(&call), None)
                    } else {
                        describe_tool(&call, workspace, &reach)
                    };
                    let always = match &reach {
                        Reach::Ask { grant: Some(_), label, .. } => label.clone(),
                        _ => None,
                    };
                    emit(EventKind::StateChanged { state: AgentState::AwaitingApproval });
                    emit(EventKind::ApprovalRequired { approval, call_id: call.id.clone(), description, diff, always });
                    match await_decision(approvals, approval, &cancel).await {
                        None => {
                            outcome = TurnOutcome::Interrupted;
                            break 'turn;
                        }
                        Some(ApprovalDecision::Deny) => {
                            emit(EventKind::ApprovalResolved { approval, decision: ApprovalDecision::Deny });
                            emit(EventKind::ToolFinished {
                                call_id: call.id.clone(),
                                success: false,
                                summary: "denied".to_owned(),
                            });
                            "The user denied this action. Don't try it again; consider another approach.".to_owned()
                        }
                        Some(decision) => {
                            emit(EventKind::ApprovalResolved { approval, decision });
                            if let Reach::Ask { grant: Some(grant), .. } = &reach {
                                match decision {
                                    ApprovalDecision::AllowForSession => permissions.allow(grant.clone()),
                                    ApprovalDecision::AllowForProject => permissions.allow_for_project(grant.clone()),
                                    _ => {}
                                }
                            }
                            let grants = permissions.sandbox_grants();
                            if call.name == crate::memory::TOOL {
                                run_memory(&call, config.memory.as_ref(), &mut emit)
                            } else {
                                run_or_background(
                                    &call,
                                    &config.custom,
                                    workspace,
                                    hub,
                                    checkpoints,
                                    &grants,
                                    &cancel,
                                    &mut emit,
                                    background,
                                )
                                .await
                            }
                        }
                    }
                };
                // Post-tool hooks see the result and may add to it.
                let mut result = result;
                if !config.hooks.is_empty() {
                    let payload = json!({
                        "event": "post_tool_use",
                        "project": workspace.root(),
                        "mode": permissions.mode.label(),
                        "tool": {"name": call.name, "arguments": call.arguments, "call_id": call.id},
                        "output": {"content": result},
                    });
                    let grants = permissions.sandbox_grants();
                    let outcomes = config.hooks.run(
                        crate::hooks::HookEvent::PostToolUse,
                        Some(&call.name),
                        &payload,
                        Some(workspace),
                        &grants,
                    );
                    for outcome in outcomes {
                        emit(EventKind::HookRan {
                            name: outcome.name.clone(),
                            event: "post_tool_use".to_owned(),
                            ok: outcome.ok,
                            summary: outcome.summary.clone(),
                        });
                        if let Some(context) = outcome.add_context {
                            hook_context.push(format!("[hook {}] {context}", outcome.name));
                        }
                    }
                }
                if !hook_context.is_empty() {
                    result.push_str("\n\n");
                    result.push_str(&hook_context.join("\n"));
                }
                if matches!(call.name.as_str(), "read" | "write" | "edit") {
                    let path = target_of(&call.name, &call.arguments);
                    for file in crate::instructions::nested_for(
                        workspace.root(),
                        std::path::Path::new(&path),
                        &mut self.nested_loaded,
                        self.instructions.is_some() || config.project_trusted,
                    ) {
                        self.notes.push(format!(
                            "instructions from {} apply to files under its folder:\n{}",
                            file.path.display(),
                            file.text.trim()
                        ));
                    }
                    self.note_file(path);
                }
                self.messages.push(Message::tool_result(&call, result));
            }

            // A message typed meanwhile joins the conversation here, before the next request.
            let delivered: Vec<(String, Vec<ImageData>)> =
                queued.lock().map(|mut queue| queue.drain(..).collect()).unwrap_or_default();
            for (text, images) in delivered {
                let text = self.with_notes(text);
                emit(EventKind::QueuedDelivered { text: text.clone() });
                self.messages.push(Message::user_with_images(text, images));
            }

            if step + 1 == config.max_steps {
                outcome = TurnOutcome::Failed {
                    reason: "Reached the step limit for one turn. Ask it to continue.".to_owned(),
                };
            }
        }
        emit(EventKind::TurnFinished { turn, outcome });
    }
}

/// Runs an approved `memory` save or forget, and shows it as the tool's row.
fn run_memory(call: &ToolCall, memory: Option<&MemoryConfig>, emit: &mut impl FnMut(EventKind)) -> String {
    let (text, success, summary) = match memory {
        Some(memory) => {
            let (text, success, summary, _) = crate::memory::run(&memory.places, memory.read_only, call);
            (text, success, summary)
        }
        None => ("Error: memory is off for this session.".to_owned(), false, "off".to_owned()),
    };
    emit(EventKind::ToolOutput { call_id: call.id.clone(), text: text.clone() });
    emit(EventKind::ToolFinished { call_id: call.id.clone(), success, summary });
    text
}

/// A shell call with `background: true` starts a task instead of running to completion; everything
/// else runs as usual.
#[allow(clippy::too_many_arguments)]
async fn run_or_background(
    call: &ToolCall,
    custom: &std::collections::HashMap<String, CustomTool>,
    workspace: &Workspace,
    hub: &mut McpHub,
    checkpoints: &mut Checkpoints,
    grants: &[sandbox::Grant],
    cancel: &CancellationToken,
    emit: &mut impl FnMut(EventKind),
    background: &mut crate::background::BackgroundTasks,
) -> String {
    if call.name == "shell" && crate::background::wants_background(&call.arguments) {
        let command = call.arguments.get("command").and_then(|v| v.as_str()).unwrap_or_default();
        let network = recode_tools::wants_network(&call.arguments);
        checkpoints.untracked(&format!("background: {command}"));
        let (text, success) = match background.spawn(workspace, command, network, grants) {
            Ok(id) => (
                format!(
                    "Started background task {id} for `{command}`. It keeps running while you work; read its output \
                     with task_output {{\"id\": \"{id}\"}} and stop it with task_kill. The user sees it under /tasks."
                ),
                true,
            ),
            Err(error) => (format!("Error: couldn't start the command in the background: {error}"), false),
        };
        emit(EventKind::ToolOutput { call_id: call.id.clone(), text: text.clone() });
        let summary = if success { "running in the background" } else { "didn't start" }.to_owned();
        emit(EventKind::ToolFinished { call_id: call.id.clone(), success, summary });
        return text;
    }
    run_and_report(call, custom, workspace, hub, checkpoints, grants, cancel, emit).await
}

/// Runs a tool and reports it to the interface, returning the text for the model.
#[allow(clippy::too_many_arguments)]
async fn run_and_report(
    call: &ToolCall,
    custom: &std::collections::HashMap<String, CustomTool>,
    workspace: &Workspace,
    hub: &mut McpHub,
    checkpoints: &mut Checkpoints,
    grants: &[sandbox::Grant],
    cancel: &CancellationToken,
    emit: &mut impl FnMut(EventKind),
) -> String {
    emit(EventKind::StateChanged { state: AgentState::RunningTool { name: call.name.clone() } });
    // File tools are checkpointed so the turn can be undone; anything else is noted as not undoable.
    let changed_file =
        matches!(call.name.as_str(), "write" | "edit" | "delete").then(|| target_of(&call.name, &call.arguments));
    match &changed_file {
        Some(path) => checkpoints.before_change(workspace, path),
        None if call.name == "shell" => {
            checkpoints.untracked(&format!("shell: {}", target_of("shell", &call.arguments)))
        }
        None if custom.contains_key(&call.name) => checkpoints.untracked(&format!("custom tool {}", call.name)),
        None if is_mcp_call(&call.name) && call.name != LIST_MCP_TOOLS => {
            checkpoints.untracked(describe_mcp_call(call).lines().next().unwrap_or_default());
        }
        None => {}
    }
    let output = execute_tool(call, custom, workspace, hub, grants, cancel).await;
    if let Some(path) = &changed_file {
        checkpoints.after_change(workspace, path);
    }
    emit(EventKind::ToolOutput { call_id: call.id.clone(), text: output.content.clone() });
    emit(EventKind::ToolFinished { call_id: call.id.clone(), success: output.success, summary: output.summary });
    output.content
}

const MAX_AGENT_STEPS: usize = 24;

const AGENT_NOTICE: &str = "This model can use tools: it reads and searches this project, and can reach files anywhere on your computer and your MCP servers. You're asked before it reads outside the project, changes a file or runs a command (commands run in a sandbox). Ctrl+Z undoes a turn's file changes. Type / for commands and @ to mention a file. The conversation is saved; re-code --continue picks it up later.";

/// Everything the agent turn needs beyond the conversation: the tool schemas to offer, the custom
/// tools and how to run them, and any skill instructions to inject for this turn.
struct AgentConfig {
    tools: Vec<ToolDefinition>,
    custom: std::collections::HashMap<String, CustomTool>,
    /// Extra system text by part, after re-code's own instructions.
    extra_system: Vec<(ComponentKind, String)>,
    /// Usable skills, for the `skill` tool.
    skills: Vec<Skill>,
    /// When the request began with `/<skill>`, the request with that prefix removed.
    invoked_text: Option<String>,
    /// Whether the project folder is trusted, so its subdirectory instruction files may load.
    project_trusted: bool,
    /// Where plan mode's `plan` tool writes.
    plan_path: std::path::PathBuf,
    /// The most tool-call steps in one turn.
    max_steps: usize,
    /// The user's and the project's hooks.
    hooks: std::sync::Arc<crate::hooks::Hooks>,
    /// How to open another conversation with this model, for the `task` tool; `None` means no tasks.
    spawn_provider: Option<ProviderFactory>,
    /// 0 for the user's conversation, 1 inside a task (tasks don't nest).
    depth: u8,
    /// Where memories live and whether this turn may change them; `None` when memory is off.
    memory: Option<MemoryConfig>,
}

/// The `memory` tool's reach for one turn.
#[derive(Clone)]
struct MemoryConfig {
    places: crate::memory::Places,
    read_only: bool,
}

/// Resolves a custom tool's command into an argv, substituting `{param}` with the given arguments.
/// Never goes through a shell.
fn resolve_custom_command(tool: &CustomTool, args: &serde_json::Value) -> Vec<String> {
    let tokens = recode_core::extensions::split_command(&tool.command).unwrap_or_default();
    tokens
        .into_iter()
        .map(|token| {
            let mut out = token;
            for parameter in &tool.parameters {
                let value = args.get(parameter).and_then(|v| v.as_str()).unwrap_or("");
                out = out.replace(&format!("{{{parameter}}}"), value);
            }
            out
        })
        .collect()
}

/// Runs a resolved custom-tool command in the sandbox (no shell), capturing its output.
fn run_custom(
    argv: &[String],
    network: bool,
    workspace: &Workspace,
    grants: &[sandbox::Grant],
    cancel: &AtomicBool,
) -> recode_tools::ToolOutput {
    if argv.is_empty() {
        return recode_tools::ToolOutput::error("the custom tool has no command");
    }
    let policy = Policy { network };
    recode_tools::command_output(
        &sandbox::run(workspace, argv, policy, grants, cancel, recode_tools::SHELL_TIMEOUT),
        policy,
    )
}

/// A plain-language description of a pending MCP call for the approval card.
fn describe_mcp_call(call: &ToolCall) -> String {
    if call.name == LIST_MCP_TOOLS {
        return "List the tools offered by the connected MCP servers".to_owned();
    }
    let (server, tool) = if call.name == CALL_MCP_TOOL {
        (
            call.arguments.get("server").and_then(|v| v.as_str()).unwrap_or("?").to_owned(),
            call.arguments.get("tool").and_then(|v| v.as_str()).unwrap_or("?").to_owned(),
        )
    } else {
        match call.name.strip_prefix(MCP_DIRECT_PREFIX).and_then(|rest| rest.split_once("__")) {
            Some((server, tool)) => (server.to_owned(), tool.to_owned()),
            None => (call.name.clone(), String::new()),
        }
    };
    let args = call.arguments.get("arguments").unwrap_or(&call.arguments);
    let args = serde_json::to_string(args).unwrap_or_default();
    let args: String = args.chars().take(400).collect();
    format!("Call the MCP tool {tool} on server {server} with:\n{args}")
}

fn mcp_output(result: Result<String, String>) -> recode_tools::ToolOutput {
    match result {
        Ok(text) => {
            let shown: String = text.chars().take(16 * 1024).collect();
            recode_tools::ToolOutput { success: true, content: shown, summary: "ok".to_owned(), diff: None }
        }
        Err(error) => recode_tools::ToolOutput::error(error),
    }
}

/// Servers with more tools than this are listed by name only, so a listing always fits a local
/// model's context (REA has over 200 tools; RustRover over 80).
const MCP_NAMES_ONLY_ABOVE: usize = 30;
/// At most this many tools are shown for a search.
const MCP_QUERY_LIMIT: usize = 25;

/// What a `list_mcp_tools` call asked for.
#[derive(Debug, Default, PartialEq, Eq)]
struct ListRequest {
    server: Option<String>,
    query: Option<String>,
    tool: Option<String>,
}

impl ListRequest {
    fn from(arguments: &serde_json::Value) -> Self {
        let text = |key: &str| {
            arguments.get(key).and_then(|v| v.as_str()).map(str::trim).filter(|v| !v.is_empty()).map(str::to_owned)
        };
        Self { server: text("server"), query: text("query"), tool: text("tool") }
    }
}

fn short(text: &str, limit: usize) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    if line.chars().count() <= limit {
        return line.to_owned();
    }
    format!("{}…", line.chars().take(limit).collect::<String>())
}

/// What `list_mcp_tools` returns, kept small enough for a local model's context: one tool in full;
/// or the tools matching a search; or, per server, signatures with a short summary — names only for a
/// big server. Unreachable servers are named once at the end.
fn list_output(listing: &recode_extensions::hub::Listing, request: &ListRequest) -> recode_tools::ToolOutput {
    let mut content = String::new();
    if let Some(name) = &request.tool {
        let found = listing.tools.iter().find(|entry| &entry.tool.name == name);
        let Some(entry) = found else {
            return recode_tools::ToolOutput::error(format!(
                "There's no tool called {name}{}. Call list_mcp_tools without `tool` to see the names.",
                request.server.as_deref().map(|s| format!(" on {s}")).unwrap_or_default()
            ));
        };
        let schema = serde_json::to_string_pretty(&entry.tool.input_schema).unwrap_or_default();
        let schema: String = schema.chars().take(6000).collect();
        content.push_str(&format!(
            "{} on server {}\n\n{}\n\nArguments (JSON schema):\n{schema}\n",
            entry.tool.signature(),
            entry.server,
            entry.tool.description.trim()
        ));
        return recode_tools::ToolOutput { success: true, content, summary: format!("{name} in full"), diff: None };
    }
    if let Some(query) = &request.query {
        let words: Vec<String> = query.to_lowercase().split_whitespace().map(str::to_owned).collect();
        let mut matches: Vec<_> = listing
            .tools
            .iter()
            .map(|entry| {
                let name = entry.tool.name.to_lowercase();
                let description = entry.tool.description.to_lowercase();
                let score: usize = words
                    .iter()
                    .map(|w| usize::from(name.contains(w.as_str())) * 3 + usize::from(description.contains(w.as_str())))
                    .sum();
                (score, entry)
            })
            .filter(|(score, _)| *score > 0)
            .collect();
        matches.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
        for (_, entry) in matches.iter().take(MCP_QUERY_LIMIT) {
            content.push_str(&format!(
                "- {} on {} — {}\n",
                entry.tool.signature(),
                entry.server,
                short(&entry.tool.description, 140)
            ));
        }
        if matches.is_empty() {
            content.push_str(&format!("No MCP tool matches \"{query}\". Try other words, or list without a query.\n"));
        } else if matches.len() > MCP_QUERY_LIMIT {
            content
                .push_str(&format!("…and {} more; narrow the search to see them.\n", matches.len() - MCP_QUERY_LIMIT));
        }
    } else {
        let mut servers: Vec<&str> = listing.tools.iter().map(|entry| entry.server.as_str()).collect();
        servers.dedup();
        for server in servers {
            let tools: Vec<_> = listing.tools.iter().filter(|entry| entry.server == server).collect();
            if tools.len() > MCP_NAMES_ONLY_ABOVE {
                let names: Vec<&str> = tools.iter().map(|entry| entry.tool.name.as_str()).collect();
                content.push_str(&format!(
                    "Server {server} has {} tools: {}.\nIt's large, so search it with `query` (for example query: \"files\") \
                     or give `tool` to see one tool's arguments.\n",
                    tools.len(),
                    names.join(", ")
                ));
            } else {
                content.push_str(&format!("Server {server}:\n"));
                for entry in tools {
                    content.push_str(&format!(
                        "- {} — {}\n",
                        entry.tool.signature(),
                        short(&entry.tool.description, 120)
                    ));
                }
            }
        }
    }
    if !listing.unavailable.is_empty() {
        let names: Vec<&str> = listing.unavailable.iter().map(|(name, _)| name.as_str()).collect();
        let reasons: Vec<String> =
            listing.unavailable.iter().map(|(name, reason)| format!("{name}: {}", short(reason, 120))).collect();
        content.push_str(&format!("Not reachable right now: {} ({}).\n", names.join(", "), reasons.join("; ")));
    }
    if listing.tools.is_empty() && listing.unavailable.is_empty() {
        content.push_str("(no MCP tools available)");
    }
    let summary = match listing.unavailable.len() {
        0 => format!("{} tools", listing.tools.len()),
        n => format!("{} tools, {n} unreachable", listing.tools.len()),
    };
    recode_tools::ToolOutput { success: !listing.tools.is_empty(), content, summary, diff: None }
}

/// Runs a tool off the async runtime so a slow command never stalls the interface. MCP calls run on
/// the async runtime directly (the hub owns the connections), so they also never block the UI.
async fn execute_tool(
    call: &ToolCall,
    custom: &std::collections::HashMap<String, CustomTool>,
    workspace: &Workspace,
    hub: &mut McpHub,
    grants: &[sandbox::Grant],
    cancel: &CancellationToken,
) -> recode_tools::ToolOutput {
    if call.name == LIST_MCP_TOOLS {
        let request = ListRequest::from(&call.arguments);
        let listing = hub.list(request.server.as_deref()).await;
        return list_output(&listing, &request);
    }
    if call.name == CALL_MCP_TOOL {
        let server = call.arguments.get("server").and_then(|v| v.as_str()).unwrap_or_default().to_owned();
        let tool = call.arguments.get("tool").and_then(|v| v.as_str()).unwrap_or_default().to_owned();
        let args = call.arguments.get("arguments").cloned().unwrap_or_else(|| json!({}));
        return mcp_output(hub.call(&server, &tool, args).await);
    }
    if let Some(rest) = call.name.strip_prefix(MCP_DIRECT_PREFIX) {
        let (server, tool) = rest.split_once("__").unwrap_or((rest, ""));
        return mcp_output(hub.call(server, tool, call.arguments.clone()).await);
    }
    let ws = workspace.clone();
    // Blocking tools run on their own thread; an interrupt sets this flag, which stops a running command.
    let stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    let grants = grants.to_vec();
    let job = if let Some(tool) = custom.get(&call.name) {
        let argv = resolve_custom_command(tool, &call.arguments);
        let network = tool.network;
        tokio::task::spawn_blocking(move || run_custom(&argv, network, &ws, &grants, &flag))
    } else {
        let name = call.name.clone();
        let args = call.arguments.clone();
        tokio::task::spawn_blocking(move || recode_tools::run_cancellable(&name, &args, &ws, &flag, &grants))
    };
    tokio::pin!(job);
    let joined = tokio::select! {
        joined = &mut job => joined,
        () = cancel.cancelled() => {
            stop.store(true, Ordering::Relaxed);
            job.await
        }
    };
    joined.unwrap_or_else(|_| recode_tools::ToolOutput::error("the tool crashed"))
}

/// A plain-language description of a pending tool call, plus a preview diff for edits and writes.
fn describe_tool(call: &ToolCall, workspace: &Workspace, reach: &Reach) -> (String, Option<String>) {
    let target = match (tool_path(call), workspace) {
        (Some(path), workspace) => workspace.locate(&path).map_or(path, |location| location.display()),
        (None, _) => target_of(&call.name, &call.arguments),
    };
    let mut description = match call.name.as_str() {
        "edit" | "write" => format!("Apply this change to {target}"),
        "delete" => format!("Delete {target}"),
        "read" => format!("Read {target}"),
        "list" => format!("List the folder {target}"),
        "grep" => format!("Search {target} for {}", target_of("grep", &call.arguments)),
        "shell" => {
            format!("Run this command {}:\n{target}", where_it_runs(recode_tools::wants_network(&call.arguments)))
        }
        other => format!("Run {other} on {target}"),
    };
    if let Reach::Ask { outside, sensitive, .. } = reach {
        if outside.is_some() {
            description.push_str("\nThis is outside the project folder.");
        }
        if *sensitive {
            description.push_str(
                "\nThis looks like it holds secrets (keys or credentials). Its contents would go to the model, \
                 so this is asked every time.",
            );
        }
    }
    (description, recode_tools::preview(&call.name, &call.arguments, workspace))
}

/// Where a command will run, in the words the approval card uses.
fn where_it_runs(network: bool) -> String {
    match sandbox::isolation() {
        sandbox::Isolation::Bubblewrap if network => "in the sandbox, with network access".to_owned(),
        sandbox::Isolation::Bubblewrap => "in the sandbox, without network".to_owned(),
        sandbox::Isolation::Unavailable { reason } => {
            format!("WITHOUT a sandbox ({reason}) — it can change anything your user can. Always-allow isn't offered")
        }
    }
}

/// Waits for the decision on a specific approval, ignoring others. Returns None if interrupted.
async fn await_decision(
    approvals: &mut UnboundedReceiver<(ApprovalId, ApprovalDecision)>,
    wanted: ApprovalId,
    cancel: &CancellationToken,
) -> Option<ApprovalDecision> {
    loop {
        tokio::select! {
            biased;
            () = cancel.cancelled() => return None,
            message = approvals.recv() => match message {
                Some((id, decision)) if id == wanted => return Some(decision),
                Some(_) => {}
                None => return None,
            },
        }
    }
}

/// The session panel's Tools line for the current model and extensions.
fn tool_status(manager: &Manager, active: bool, declared: bool, has_workspace: bool) -> UiEvent {
    let snapshot = manager.extensions.snapshot();
    let custom = snapshot.tools.iter().filter(|tool| tool.enabled).count();
    // Servers whose last check failed (an IDE that isn't open) are counted, not listed as if available.
    let mut down = 0;
    let mut mcp: Vec<String> = usable_servers(&snapshot.servers)
        .into_iter()
        .filter_map(|name| match manager.probes.get(&name) {
            Some(McpProbe::Connected { tools, .. }) => Some(format!("{name} ({})", tools.len())),
            Some(McpProbe::Failed { .. }) => {
                down += 1;
                None
            }
            None => Some(name),
        })
        .collect();
    if down > 0 {
        mcp.push(format!("{down} not running"));
    }
    let (active, summary) = tool_summary(active, declared, has_workspace, custom, &mcp);
    UiEvent::ToolStatus { active, summary }
}

/// Composes the one-line tool-availability summary for the session panel.
fn tool_summary(
    active: bool,
    tools_declared: bool,
    has_workspace: bool,
    custom: usize,
    mcp: &[String],
) -> (bool, String) {
    if active {
        let builtin = recode_tools::TOOL_NAMES.len();
        let mut summary = format!("{builtin} built-in");
        if custom > 0 {
            summary.push_str(&format!(" · {custom} custom"));
        }
        if !mcp.is_empty() {
            summary.push_str(&format!(" · MCP: {}", mcp.join(", ")));
        }
        return (true, summary);
    }
    let reason = if !has_workspace {
        "Off — re-code isn't in a project folder."
    } else if !tools_declared {
        "Off — this model doesn't support tools."
    } else {
        "Off."
    };
    (false, reason.to_owned())
}

fn finish_outcome(finish: &FinishReason) -> TurnOutcome {
    match finish {
        FinishReason::Stop => TurnOutcome::Completed,
        FinishReason::Cancelled => TurnOutcome::Interrupted,
        FinishReason::Length => {
            TurnOutcome::Failed { reason: "The response was cut off because it reached the length limit.".to_owned() }
        }
        FinishReason::Other(other) => TurnOutcome::Failed { reason: format!("The model stopped early: {other}") },
    }
}

/// The runtime report in the interface's terms: strings and flags, no runtime types.
fn runtimes_info(report: &recode_runtime::Report) -> RuntimesInfo {
    use recode_runtime::{GpuAccess, Ownership, RuntimeState, ServedBy};
    let engines = report
        .engines
        .iter()
        .map(|engine| {
            let version = engine.version.as_deref().unwrap_or("didn't answer");
            let rootless = if engine.rootless == Some(true) { " rootless" } else { "" };
            format!("{} {version}{rootless}", engine.engine.command())
        })
        .collect();
    let runtimes = report
        .runtimes
        .iter()
        .map(|runtime| {
            let (served_by, image, gpu) = match &runtime.served_by {
                Some(ServedBy::Container { engine, name, pod, image, gpu }) => {
                    let pod = pod.as_deref().map(|p| format!(" in pod `{p}`")).unwrap_or_default();
                    let gpu = match gpu {
                        GpuAccess::Yes { devices } => format!("GPU attached ({})", devices.join(", ")),
                        GpuAccess::No => "No GPU devices given to the container.".to_owned(),
                        GpuAccess::Unknown => "GPU access unknown.".to_owned(),
                    };
                    (Some(format!("{} container `{name}`{pod}", engine.command())), Some(image.clone()), Some(gpu))
                }
                Some(ServedBy::SystemdUnit { unit, user, file }) => {
                    let scope = if *user { "user" } else { "system" };
                    let file =
                        file.as_deref().map(|f| format!(" ({})", doctor::runtime::display_path(f))).unwrap_or_default();
                    (Some(format!("systemd {scope} unit {unit}{file}")), None, None)
                }
                Some(ServedBy::Process { pid, exe }) => (Some(format!("process {pid} ({exe})")), None, None),
                None => (None, None, None),
            };
            let state = match &runtime.state {
                RuntimeState::Running { version } => RuntimeRowState::Running { version: version.clone() },
                RuntimeState::InstalledStopped { start } => RuntimeRowState::Stopped {
                    start_command: start.as_ref().map(recode_runtime::StartMethod::command_line),
                    needs_root: start.as_ref().is_some_and(recode_runtime::StartMethod::needs_root),
                },
                RuntimeState::ActiveNotAnswering { error } => RuntimeRowState::NotAnswering { error: error.clone() },
            };
            RuntimeRow {
                id: runtime.id.clone(),
                name: runtime.kind.name().to_owned(),
                supported: runtime.kind.supported(),
                state,
                url: runtime.url.clone(),
                served_by,
                image,
                gpu,
                yours: runtime.ownership == Ownership::UserManaged,
                notes: runtime.notes.clone(),
            }
        })
        .collect();
    let pods = report
        .pods
        .iter()
        .map(|pod| {
            // Published ports belong to the pod; podman repeats them on every member.
            let ports = report
                .containers
                .iter()
                .find(|c| c.pod.as_deref() == Some(pod.name.as_str()))
                .map(|c| c.ports.iter().map(|p| p.host_port).collect())
                .unwrap_or_default();
            PodRow { name: pod.name.clone(), running: pod.running, ports }
        })
        .collect();
    let containers = report
        .containers
        .iter()
        .filter(|c| !c.is_infra)
        .map(|c| ContainerRow {
            name: c.name.clone(),
            image: c.image.clone(),
            status: c.status.clone(),
            running: c.running,
            pod: c.pod.clone(),
            ports: c.ports.iter().map(|p| p.host_port).collect(),
            runs: c.runs.map(|k| k.name().to_owned()),
        })
        .collect();
    RuntimesInfo {
        engines,
        runtimes,
        pods,
        containers,
        unidentified: report.unidentified.iter().map(|u| format!("{}: {}", u.url, u.summary)).collect(),
        warnings: report.warnings.clone(),
        problems: report.problems.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const RECORDED_TEXT: &str = include_str!("../../providers/tests/fixtures/ollama-0.40.14/chat_text.ndjson");
    const RECORDED_LENGTH: &str = include_str!("../../providers/tests/fixtures/ollama-0.40.14/chat_length.ndjson");

    async fn server(body: &str) -> (MockServer, Chat) {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .and(body_partial_json(serde_json::json!({"options": {"num_ctx": 4096}})))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;
        let mut chat = Chat::new(OllamaProvider::new(&server.uri()).unwrap(), 4096);
        chat.model = "m".to_owned();
        (server, chat)
    }

    async fn turn(chat: &mut Chat, text: &str) -> Vec<EventKind> {
        let mut events = Vec::new();
        chat.run_turn(text.to_owned(), CancellationToken::new(), |event| events.push(event)).await;
        events
    }

    fn outcome(events: &[EventKind]) -> TurnOutcome {
        match events.last() {
            Some(EventKind::TurnFinished { outcome, .. }) => outcome.clone(),
            other => panic!("the turn didn't finish: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_turn_streams_reports_exact_usage_and_remembers_the_reply() {
        let (_server, mut chat) = server(RECORDED_TEXT).await;
        let events = turn(&mut chat, "hi").await;
        assert_eq!(outcome(&events), TurnOutcome::Completed);
        assert!(events.iter().any(|event| matches!(event, EventKind::TextDelta { .. })));
        // An estimate comes first, then the provider's figure replaces it.
        assert!(matches!(events[1], EventKind::ContextMeasured { used: TokenCount::Estimated { .. }, .. }));
        assert!(
            events
                .iter()
                .any(|event| matches!(event, EventKind::ContextMeasured { used: TokenCount::Exact { .. }, .. }))
        );
        assert_eq!(chat.messages.len(), 3, "system, user, assistant");
    }

    #[tokio::test]
    async fn a_cut_off_response_is_reported_rather_than_looking_finished() {
        let (_server, mut chat) = server(RECORDED_LENGTH).await;
        assert!(matches!(outcome(&turn(&mut chat, "hi").await), TurnOutcome::Failed { .. }));
    }

    #[tokio::test]
    async fn an_oversized_request_is_never_sent() {
        let server = MockServer::start().await;
        Mock::given(path("/api/chat")).respond_with(ResponseTemplate::new(200)).expect(0).mount(&server).await;
        let mut chat = Chat::new(OllamaProvider::new(&server.uri()).unwrap(), 4096);
        let before = chat.messages.len();
        let events = turn(&mut chat, &"x".repeat(100_000)).await;
        assert!(matches!(outcome(&events), TurnOutcome::Failed { reason } if reason.contains("wasn't sent")));
        assert_eq!(chat.messages.len(), before, "the rejected message doesn't stay in the conversation");
    }

    #[tokio::test]
    async fn a_failure_reaches_the_user_with_the_servers_reason() {
        let server = MockServer::start().await;
        Mock::given(path("/api/chat"))
            .respond_with(ResponseTemplate::new(404).set_body_string("{\"error\":\"model 'm' not found\"}"))
            .mount(&server)
            .await;
        let mut chat = Chat::new(OllamaProvider::new(&server.uri()).unwrap(), 4096);
        chat.model = "m".to_owned();
        let events = turn(&mut chat, "hi").await;
        assert!(matches!(outcome(&events), TurnOutcome::Failed { reason } if reason.contains("not found")));
    }

    #[tokio::test]
    async fn cancelling_ends_the_turn_as_interrupted() {
        let server = MockServer::start().await;
        Mock::given(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_delay(std::time::Duration::from_secs(60)))
            .mount(&server)
            .await;
        let mut chat = Chat::new(OllamaProvider::new(&server.uri()).unwrap(), 4096);
        chat.model = "m".to_owned();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let mut events = Vec::new();
        chat.run_turn("hi".to_owned(), cancel, |event| events.push(event)).await;
        assert_eq!(outcome(&events), TurnOutcome::Interrupted);
    }

    #[test]
    fn image_path_tokens_are_extracted_and_others_kept() {
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("pic.png");
        std::fs::write(&png, b"\x89PNG fake").unwrap();
        let text = format!("look at @{} and @not-an-image and @{}/missing.png", png.display(), dir.path().display());
        let (clean, images) = take_image_attachments(&text);
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].media_type, "image/png");
        assert_eq!(images[0].label, "pic.png");
        assert!(clean.contains("@not-an-image"), "non-image @ token kept: {clean}");
        assert!(!clean.contains("pic.png"), "image token removed: {clean}");
    }

    #[test]
    fn custom_command_substitutes_parameters_without_a_shell() {
        let tool =
            recode_core::extensions::parse_tool_form("s", "d", "rg {query} {path}", "query, path", true).unwrap();
        let argv = resolve_custom_command(&tool, &serde_json::json!({"query": "foo bar", "path": "src"}));
        // Each token stays one argv element — no shell word-splitting of "foo bar".
        assert_eq!(argv, ["rg", "foo bar", "src"]);
    }

    #[test]
    fn tool_summary_states_availability_or_the_reason() {
        assert_eq!(tool_summary(true, true, true, 0, &[]), (true, "8 built-in".to_owned()));
        assert_eq!(tool_summary(true, true, true, 2, &[]).1, "8 built-in · 2 custom");
        assert!(!tool_summary(false, true, false, 0, &[]).0);
        assert!(tool_summary(false, true, false, 0, &[]).1.contains("project folder"));
        assert!(tool_summary(false, false, true, 0, &[]).1.contains("doesn't support tools"));
        let mcp = ["rustrover (83)".to_owned(), "rea".to_owned()];
        assert_eq!(tool_summary(true, true, true, 0, &mcp).1, "8 built-in · MCP: rustrover (83), rea");
    }

    #[test]
    fn mcp_calls_are_recognised_and_discovery_is_automatic() {
        assert!(is_mcp_call(LIST_MCP_TOOLS));
        assert!(is_mcp_call(CALL_MCP_TOOL));
        assert!(is_mcp_call("mcp__rea__open_binary"));
        assert!(!is_mcp_call("read"));
        assert!(!is_mcp_call("my_custom_tool"));

        let (_dir, ws) = scratch();
        let perms = Permissions::default();
        // Listing is harmless discovery, so it never asks.
        assert!(perms.auto(&reach(&call(LIST_MCP_TOOLS, json!({})), &ws, &HashMap::new())));
        // Calling a tool acts, so it asks until allowed.
        let open = call(CALL_MCP_TOOL, json!({"server": "rea", "tool": "open_binary"}));
        assert!(!perms.auto(&reach(&open, &ws, &HashMap::new())));
        assert!(!perms.auto(&reach(&call("mcp__rea__open_binary", json!({})), &ws, &HashMap::new())));
    }

    fn scratch() -> (tempfile::TempDir, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(dir.path()).unwrap();
        (dir, workspace)
    }

    fn call(name: &str, arguments: serde_json::Value) -> ToolCall {
        ToolCall { id: "1".to_owned(), name: name.to_owned(), arguments, extra: None }
    }

    /// Asks as the agent loop does, and says "always" when it's offered.
    fn ask_always(perms: &mut Permissions, ws: &Workspace, tool: &ToolCall) -> Option<String> {
        let reach = reach(tool, ws, &HashMap::new());
        let Reach::Ask { grant: Some(grant), label, .. } = reach else { return None };
        perms.allow(grant);
        label
    }

    fn runs_freely(perms: &Permissions, ws: &Workspace, tool: &ToolCall) -> bool {
        perms.auto(&reach(tool, ws, &HashMap::new()))
    }

    #[test]
    fn allowing_an_mcp_tool_is_keyed_by_server_and_tool() {
        let (_dir, ws) = scratch();
        let mut perms = Permissions::default();
        let a = call(CALL_MCP_TOOL, json!({"server": "rea", "tool": "open_binary", "arguments": {"path": "/a"}}));
        let b = call(CALL_MCP_TOOL, json!({"server": "rea", "tool": "open_binary", "arguments": {"path": "/b"}}));
        let other = call(CALL_MCP_TOOL, json!({"server": "rea", "tool": "close_binary"}));
        assert_eq!(ask_always(&mut perms, &ws, &a).as_deref(), Some("always open_binary on rea"));
        // Same server+tool with different arguments is still allowed; a different tool is not.
        assert!(runs_freely(&perms, &ws, &b));
        assert!(!runs_freely(&perms, &ws, &other));
    }

    #[test]
    fn always_editing_the_project_does_not_allow_editing_anywhere_else() {
        let (_dir, ws) = scratch();
        let elsewhere = tempfile::tempdir().unwrap();
        let outside = elsewhere.path().join("notes.md").to_string_lossy().into_owned();
        let mut perms = Permissions::default();
        assert!(runs_freely(&perms, &ws, &call("read", json!({"path": "src/main.rs"}))), "project reads are free");
        let edit = call("write", json!({"path": "src/main.rs", "content": "x"}));
        assert_eq!(ask_always(&mut perms, &ws, &edit).as_deref(), Some("always edit in this project"));
        assert!(runs_freely(&perms, &ws, &call("edit", json!({"path": "other.rs", "old": "a", "new": "b"}))));
        // The old bug: "always" on one write allowed every later write, anywhere.
        assert!(!runs_freely(&perms, &ws, &call("write", json!({"path": outside, "content": "x"}))));
        // Deleting isn't covered by accepting edits.
        assert!(!runs_freely(&perms, &ws, &call("delete", json!({"path": "src/main.rs"}))));
    }

    #[test]
    fn a_folder_outside_is_allowed_with_its_subfolders_only_and_reading_is_not_writing() {
        let (_dir, ws) = scratch();
        let home = tempfile::tempdir().unwrap();
        let notes = home.path().join("notes");
        std::fs::create_dir_all(notes.join("2026")).unwrap();
        std::fs::create_dir_all(home.path().join("other")).unwrap();
        let path = |p: &std::path::Path| p.to_string_lossy().into_owned();
        let mut perms = Permissions::default();
        let read = call("read", json!({"path": path(&notes.join("todo.md"))}));
        assert!(!runs_freely(&perms, &ws, &read), "outside reads ask");
        let label = ask_always(&mut perms, &ws, &read).unwrap();
        assert!(label.starts_with("always read in") && label.ends_with("notes"), "{label}");
        assert!(runs_freely(&perms, &ws, &call("read", json!({"path": path(&notes.join("2026/jan.md"))}))));
        assert!(runs_freely(&perms, &ws, &call("list", json!({"path": path(&notes)}))));
        assert!(!runs_freely(&perms, &ws, &call("read", json!({"path": path(&home.path().join("other/x.md"))}))));
        let write = call("write", json!({"path": path(&notes.join("todo.md")), "content": "x"}));
        assert!(!runs_freely(&perms, &ws, &write), "a read grant doesn't allow changes");
        ask_always(&mut perms, &ws, &write);
        assert!(runs_freely(&perms, &ws, &write));
        // Deleting outside the project is asked every time.
        let delete = call("delete", json!({"path": path(&notes.join("todo.md"))}));
        assert_eq!(ask_always(&mut perms, &ws, &delete), None);
        assert!(!runs_freely(&perms, &ws, &delete));
        // What was allowed reaches commands too.
        assert_eq!(perms.sandbox_grants().len(), 2);
    }

    #[test]
    fn secrets_are_asked_every_time_even_inside_the_project() {
        let (_dir, ws) = scratch();
        let mut perms = Permissions::default();
        let env = call("read", json!({"path": ".env"}));
        assert!(!runs_freely(&perms, &ws, &env), "reading .env sends secrets to the model");
        assert_eq!(ask_always(&mut perms, &ws, &env), None, "and can't be allowed always");
        let key = call("read", json!({"path": "~/.ssh/id_ed25519"}));
        assert_eq!(ask_always(&mut perms, &ws, &key), None);
        let Reach::Ask { sensitive, outside, .. } = reach(&key, &ws, &HashMap::new()) else { panic!() };
        assert!(sensitive && outside.is_some());
        // Allowing the whole home folder doesn't open ~/.ssh either.
        perms.allow(Grant::Folder { dir: std::env::var_os("HOME").unwrap().into(), write: true });
        assert!(!runs_freely(&perms, &ws, &key));
    }

    #[test]
    fn meta_tools_appear_only_with_a_server_and_describe_the_servers() {
        let tools = mcp_meta_tools(&["rea".to_owned(), "rustrover".to_owned()]);
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, LIST_MCP_TOOLS);
        assert!(tools[0].description.contains("rea"));
        assert!(tools[0].description.contains("rustrover"));
        assert_eq!(tools[1].name, CALL_MCP_TOOL);
    }

    fn listing(server: &str, count: usize) -> recode_extensions::hub::Listing {
        let tools = (0..count)
            .map(|n| recode_extensions::hub::HubTool {
                server: server.to_owned(),
                tool: recode_core::extensions::McpToolInfo {
                    name: if n == 7 { "get_project_modules".to_owned() } else { format!("tool_{n}") },
                    description: format!("Does thing {n}.\nA much longer explanation that only the full view shows."),
                    input_schema: json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}),
                },
            })
            .collect();
        recode_extensions::hub::Listing { tools, unavailable: vec![("goland".into(), "connection refused".into())] }
    }

    #[test]
    fn a_large_server_is_listed_by_name_and_can_be_searched_or_opened() {
        let big = listing("rustrover", 83);
        let overview = list_output(&big, &ListRequest::default()).content;
        assert!(overview.contains("has 83 tools"), "{overview}");
        assert!(!overview.contains("longer explanation") && !overview.contains("path: string"), "names only");
        assert!(overview.len() < 2000, "small enough for any context: {} chars", overview.len());
        assert!(overview.contains("Not reachable right now: goland"));

        let search =
            list_output(&big, &ListRequest { query: Some("modules".into()), ..ListRequest::default() }).content;
        assert!(search.starts_with("- get_project_modules(path: string) on rustrover"), "{search}");

        let one = ListRequest {
            server: Some("rustrover".into()),
            tool: Some("get_project_modules".into()),
            ..Default::default()
        };
        let full = list_output(&big, &one).content;
        assert!(full.contains("longer explanation") && full.contains("\"required\""), "{full}");
        let missing = ListRequest { tool: Some("nope".into()), ..Default::default() };
        assert!(!list_output(&big, &missing).success);
    }

    #[test]
    fn a_small_server_shows_signatures_and_short_summaries() {
        let small = listing("echo", 3);
        let overview = list_output(&small, &ListRequest::default()).content;
        assert!(overview.contains("- tool_0(path: string) — Does thing 0."), "{overview}");
        assert!(!overview.contains("longer explanation"));
    }

    #[test]
    fn describe_mcp_call_names_the_server_and_tool() {
        let call = ToolCall {
            id: "1".to_owned(),
            name: CALL_MCP_TOOL.to_owned(),
            arguments: json!({"server": "rea", "tool": "open_binary", "arguments": {"path": "/bin/ls"}}),
            extra: None,
        };
        let text = describe_mcp_call(&call);
        assert!(text.contains("open_binary"), "{text}");
        assert!(text.contains("rea"), "{text}");
        assert!(text.contains("/bin/ls"), "{text}");

        let direct = ToolCall {
            id: "2".to_owned(),
            name: "mcp__rea__close_binary".to_owned(),
            arguments: json!({}),
            extra: None,
        };
        assert!(describe_mcp_call(&direct).contains("close_binary"));
    }

    #[test]
    fn fit_estimates_agree_with_what_was_measured_on_this_gpu() {
        // Figures from docs/spikes/2026-11-06-ollama.md, with about 14 GiB free.
        let fit = |size| estimate_fit(Some(size), 0.0, Some(14.0), Some(70.0));
        assert_eq!(fit(1.9), GpuFit::FitsGpu);
        assert_eq!(fit(12.8), GpuFit::FitsGpu, "gpt-oss:20b ran entirely on the GPU");
        assert_eq!(fit(12.6), GpuFit::FitsGpu, "cydonia:24b ran entirely on the GPU");
        assert!(matches!(fit(16.5), GpuFit::PartlyCpu { .. }), "the 27B models ran partly on the CPU");
        // The context's cache counts too: 12.8 GiB of weights plus a 1.75 GiB cache no longer fit in 14.
        assert!(
            matches!(estimate_fit(Some(12.8), 1.75, Some(14.0), Some(70.0)), GpuFit::PartlyCpu { over_gib } if over_gib > 1.0)
        );
    }

    #[test]
    fn fit_is_unknown_without_gpu_figures_and_large_models_are_flagged() {
        assert_eq!(estimate_fit(Some(5.0), 0.0, None, Some(70.0)), GpuFit::Unknown);
        assert_eq!(estimate_fit(None, 0.0, Some(14.0), Some(70.0)), GpuFit::Unknown);
        assert_eq!(estimate_fit(Some(100.0), 0.0, Some(14.0), Some(20.0)), GpuFit::TooLarge);
    }

    /// A model's `write` (an Ollama stream in the recorded 0.40.14 wire format, with a synthetic call)
    /// changes a real file through the agent loop; undo puts it back, and the model is told on the next
    /// turn rather than left believing its edit still stands.
    #[tokio::test]
    async fn an_agent_edit_is_checkpointed_undone_and_the_model_is_told() {
        const WRITE_CALL: &str = concat!(
            r#"{"model":"m","created_at":"2026-11-06T18:48:11Z","message":{"role":"assistant","content":"","tool_calls":[{"id":"call_1","function":{"index":0,"name":"write","arguments":{"path":"notes.txt","content":"agent text\n"}}}]},"done":false}"#,
            "\n",
            r#"{"model":"m","created_at":"2026-11-06T18:48:11Z","message":{"role":"assistant","content":""},"done":true,"done_reason":"stop","prompt_eval_count":10,"eval_count":5}"#,
            "\n"
        );
        let server = MockServer::start().await;
        Mock::given(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_string(WRITE_CALL))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_string(RECORDED_TEXT))
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes.txt"), "mine\n").unwrap();
        let workspace = Workspace::new(dir.path()).unwrap();
        let mut chat = Chat::new(OllamaProvider::new(&server.uri()).unwrap(), 4096);
        chat.model = "m".to_owned();
        let config = AgentConfig {
            tools: tool_definitions(),
            custom: HashMap::new(),
            extra_system: Vec::new(),
            project_trusted: true,
            plan_path: std::env::temp_dir().join("re-code-test-plan.md"),
            max_steps: MAX_AGENT_STEPS,
            hooks: std::sync::Arc::default(),
            spawn_provider: None,
            depth: 0,
            memory: None,
            skills: Vec::new(),
            invoked_text: None,
        };
        let mut hub = McpHub::new(Vec::new());
        let mut permissions = Permissions::default();
        let mut checkpoints = Checkpoints::default();

        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut events = Vec::new();
        chat.agent_turn(
            "rewrite my notes".to_owned(),
            Vec::new(),
            &workspace,
            &config,
            &mut hub,
            &mut permissions,
            &mut checkpoints,
            &mut rx,
            &std::sync::Mutex::new(std::collections::VecDeque::new()),
            &mut crate::background::BackgroundTasks::default(),
            CancellationToken::new(),
            |event| {
                if let EventKind::ApprovalRequired { approval, .. } = &event {
                    let _ = tx.send((*approval, ApprovalDecision::AllowOnce));
                }
                events.push(event);
            },
        )
        .await;
        assert_eq!(outcome(&events), TurnOutcome::Completed);
        assert_eq!(std::fs::read_to_string(dir.path().join("notes.txt")).unwrap(), "agent text\n");

        let report = checkpoints.undo(&workspace).expect("the turn changed a file");
        assert_eq!(report.restored, vec!["notes.txt"]);
        assert_eq!(std::fs::read_to_string(dir.path().join("notes.txt")).unwrap(), "mine\n");

        // As the driver does: the next request starts by telling the model what happened.
        chat.notes.push(format!("the user undid the file changes from an earlier turn. {}", report.describe()));
        let _ = turn(&mut chat, "what now?").await;
        let last_user = chat.messages.iter().rev().find(|m| m.role == recode_core::message::Role::User).unwrap();
        assert!(last_user.content.starts_with("[re-code: the user undid"), "{}", last_user.content);
        assert!(last_user.content.ends_with("what now?"));
        assert!(chat.notes.is_empty(), "a note is delivered once");
    }

    /// A long agent turn: one request, then many tool results of `size` characters each.
    fn long_agent_history(chat: &mut Chat, results: usize, size: usize) {
        chat.messages.push(Message::user("make the tests pass"));
        for step in 0..results {
            let call = ToolCall {
                id: format!("c{step}"),
                name: "read".to_owned(),
                arguments: json!({"path": format!("src/file{step}.rs")}),
                extra: None,
            };
            chat.messages.push(Message::assistant("", vec![call.clone()]));
            chat.messages.push(Message::tool_result(&call, format!("content of file {step}\n{}", "x".repeat(size))));
        }
    }

    /// The failure a user hit on a 32k window: a few whole-file reads in the latest steps were bigger
    /// than the window, compaction kept them whole, and from then on every message was refused.
    #[tokio::test]
    async fn huge_recent_results_are_cut_to_fit_and_the_conversation_carries_on() {
        let server = MockServer::start().await;
        Mock::given(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_string(RECORDED_TEXT))
            .mount(&server)
            .await;
        let mut chat = Chat::new(OllamaProvider::new(&server.uri()).unwrap(), 8192);
        chat.model = "m".to_owned();
        let file: String = (1..=3000).map(|n| format!("fn line_{n}() {{}}\n")).collect();
        chat.messages.push(Message::user("review my code"));
        for step in 0..4 {
            let call = ToolCall {
                id: format!("c{step}"),
                name: "read".into(),
                arguments: json!({"path": "src/app.rs"}),
                extra: None,
            };
            chat.messages.push(Message::assistant("", vec![call.clone()]));
            chat.messages.push(Message::tool_result(&call, file.clone()));
        }
        chat.messages.push(Message::user("ok so what do you think?"));
        let mut events = Vec::new();
        let fitted =
            chat.fit(&tool_definitions(), None, false, &CancellationToken::new(), &mut |e| events.push(e)).await;
        assert_eq!(fitted, Ok(()), "{events:?}");
        let budget = chat.window.budget().unwrap();
        let tools = chat.offered_tools(&tool_definitions());
        assert!(compact::estimate_with(&chat.messages, &tools, chat.calibration.ratio()) <= budget);
        // Every result is behind a handle now, and the whole text pages back in parts that fit.
        let cap = chat.result_cap_chars();
        let handle = chat.elided.iter().find(|(_, text)| text.len() == file.len()).map(|(h, _)| h.clone()).unwrap();
        let call = |offset: usize| ToolCall {
            id: "x".into(),
            name: compact::READ_RESULT.into(),
            arguments: json!({"handle": handle, "offset": offset}),
            extra: None,
        };
        let first = chat.read_result(&call(1));
        assert!(first.starts_with("fn line_1() {}") && first.chars().count() <= cap, "{}", &first[..80]);
        let next: usize = first.rsplit("offset ").next().unwrap().trim_end_matches(" for more]").parse().unwrap();
        let second = chat.read_result(&call(next));
        assert!(second.starts_with(&format!("fn line_{next}() {{}}")), "{}", &second[..80]);
        let last = chat.read_result(&call(3000));
        assert_eq!(last, "fn line_3000() {}\n");
    }

    #[tokio::test]
    async fn old_tool_results_are_shortened_without_a_model_call_and_can_be_read_back() {
        let server = MockServer::start().await;
        Mock::given(path("/api/chat")).respond_with(ResponseTemplate::new(200)).expect(0).mount(&server).await;
        let mut chat = Chat::new(OllamaProvider::new(&server.uri()).unwrap(), 8192);
        chat.model = "m".to_owned();
        // An 8k window keeps 1k for the reply and compacts at nine tenths of the rest (~6.4k tokens).
        assert_eq!(chat.window.trigger, Some(6452));
        long_agent_history(&mut chat, 14, 1200);
        let mut events = Vec::new();
        chat.fit(&[], None, false, &CancellationToken::new(), &mut |event| events.push(event)).await.unwrap();
        assert!(events.is_empty(), "below the trigger nothing happens: {events:?}");
        long_agent_history(&mut chat, 4, 1200);

        chat.fit(&[], None, false, &CancellationToken::new(), &mut |event| events.push(event)).await.unwrap();
        let Some(EventKind::Compacted { before_tokens, after_tokens, elided, summarised, problem, .. }) =
            events.iter().find(|e| matches!(e, EventKind::Compacted { .. })).cloned()
        else {
            panic!("no compaction reported: {events:?}");
        };
        assert_eq!((elided, summarised, problem), (14, 0, None), "shortening alone was enough");
        assert!(after_tokens < before_tokens / 2);
        // read_result is offered only once something was shortened, and brings the original back.
        assert!(chat.offered_tools(&tool_definitions()).iter().any(|t| t.name == compact::READ_RESULT));
        let call = ToolCall {
            id: "x".into(),
            name: compact::READ_RESULT.into(),
            arguments: json!({"handle": "r1"}),
            extra: None,
        };
        assert!(chat.read_result(&call).starts_with("content of file 0\n"));
        // The shortened history is what gets saved, so read_result still works after a restart.
        assert_eq!(chat.saved().elided.len(), 14);
    }

    #[tokio::test]
    async fn instruction_files_ride_in_the_system_prompt_and_show_in_the_ledger() {
        let (_server, mut chat) = server(RECORDED_TEXT).await;
        chat.instructions = Some((
            ComponentKind::ProjectInstructions,
            "Instructions from AGENTS.md (project):\nAnswer in French.".to_owned(),
        ));
        let events = turn(&mut chat, "hello").await;
        assert_eq!(outcome(&events), TurnOutcome::Completed);
        assert!(chat.messages[0].content.contains("Answer in French."), "{}", chat.messages[0].content);
        assert!(chat.messages[0].content.starts_with(SYSTEM_PROMPT), "re-code's own prompt comes first");
        let ledger = chat.ledger(&[]);
        let part = ledger.components.iter().find(|c| c.kind == ComponentKind::ProjectInstructions).unwrap();
        assert!(part.tokens > 0);
        // Without instructions the part is gone again.
        chat.instructions = None;
        let _ = turn(&mut chat, "again").await;
        assert!(!chat.messages[0].content.contains("French"));
    }

    #[tokio::test]
    async fn a_task_runs_in_its_own_conversation_and_only_its_answer_comes_back() {
        const TASK_CALL: &str = concat!(
            r#"{"model":"m","created_at":"2026-11-06T18:48:11Z","message":{"role":"assistant","content":"","tool_calls":[{"id":"call_1","function":{"index":0,"name":"task","arguments":{"description":"survey","prompt":"List the modules in src."}}}]},"done":false}"#,
            "\n",
            r#"{"model":"m","created_at":"2026-11-06T18:48:11Z","message":{"role":"assistant","content":""},"done":true,"done_reason":"stop","prompt_eval_count":10,"eval_count":5}"#,
            "\n"
        );
        let server = MockServer::start().await;
        Mock::given(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_string(TASK_CALL))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_string(RECORDED_TEXT))
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(dir.path()).unwrap();
        let mut chat = Chat::new(OllamaProvider::new(&server.uri()).unwrap(), 4096);
        chat.model = "m".to_owned();
        let uri = server.uri();
        let factory: ProviderFactory = Arc::new(move || {
            OllamaProvider::new(&uri).map(|p| Box::new(p) as Box<dyn Provider>).map_err(|e| e.to_string())
        });
        let mut tools = tool_definitions();
        tools.push(crate::subagent::tool_definition());
        let config = AgentConfig {
            tools,
            custom: HashMap::new(),
            extra_system: Vec::new(),
            project_trusted: true,
            plan_path: std::env::temp_dir().join("re-code-test-plan.md"),
            max_steps: MAX_AGENT_STEPS,
            hooks: std::sync::Arc::default(),
            spawn_provider: Some(factory),
            depth: 0,
            memory: None,
            skills: Vec::new(),
            invoked_text: None,
        };
        let (_tx, mut rx) = mpsc::unbounded_channel();
        let mut events = Vec::new();
        chat.agent_turn(
            "survey the code".to_owned(),
            Vec::new(),
            &workspace,
            &config,
            &mut McpHub::new(Vec::new()),
            &mut Permissions::default(),
            &mut Checkpoints::default(),
            &mut rx,
            &std::sync::Mutex::new(std::collections::VecDeque::new()),
            &mut crate::background::BackgroundTasks::default(),
            CancellationToken::new(),
            |event| events.push(event),
        )
        .await;
        assert_eq!(outcome(&events), TurnOutcome::Completed);
        let output = events.iter().find_map(|e| match e {
            EventKind::ToolOutput { call_id, text } if call_id == "call_1" => Some(text.clone()),
            _ => None,
        });
        let output = output.expect("the task reported");
        assert!(output.starts_with("Result:\n") && output.contains("Hello there friend."), "{output}");
        assert!(events.iter().any(|e| matches!(e, EventKind::ToolFinished { call_id, success: true, summary, .. } if call_id == "call_1" && summary == "task done")));
        // Only the parent's own turn appears as a turn; the task's start and finish stayed inside the tool row.
        assert_eq!(events.iter().filter(|e| matches!(e, EventKind::TurnStarted { .. })).count(), 1);
        // The parent's history holds the task's answer as a tool result, not its conversation.
        let result = chat.messages.iter().find(|m| m.role == recode_core::message::Role::Tool).unwrap();
        assert!(result.content.contains("Hello there friend."));
        assert!(
            !chat.messages.iter().any(|m| m.content.contains("List the modules in src.")),
            "the task's prompt stays out"
        );
    }

    #[tokio::test]
    async fn a_refused_call_gets_one_row_not_two() {
        const WRITE_CALL: &str = concat!(
            r#"{"model":"m","created_at":"2026-11-06T18:48:11Z","message":{"role":"assistant","content":"","tool_calls":[{"id":"call_1","function":{"index":0,"name":"write","arguments":{"path":"src/new.rs","content":"fn main() {}"}}}]},"done":false}"#,
            "\n",
            r#"{"model":"m","created_at":"2026-11-06T18:48:11Z","message":{"role":"assistant","content":""},"done":true,"done_reason":"stop","prompt_eval_count":10,"eval_count":5}"#,
            "\n"
        );
        let server = MockServer::start().await;
        Mock::given(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_string(WRITE_CALL))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_string(RECORDED_TEXT))
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(dir.path()).unwrap();
        let mut chat = Chat::new(OllamaProvider::new(&server.uri()).unwrap(), 4096);
        let config = AgentConfig {
            tools: tool_definitions(),
            custom: HashMap::new(),
            extra_system: Vec::new(),
            project_trusted: true,
            plan_path: std::env::temp_dir().join("re-code-test-plan.md"),
            max_steps: MAX_AGENT_STEPS,
            hooks: std::sync::Arc::default(),
            spawn_provider: None,
            depth: 0,
            memory: None,
            skills: Vec::new(),
            invoked_text: None,
        };
        let mut permissions = Permissions::default();
        permissions.mode = PermissionMode::Plan;
        let (_tx, mut rx) = mpsc::unbounded_channel();
        let mut events = Vec::new();
        chat.agent_turn(
            "write it".to_owned(),
            Vec::new(),
            &workspace,
            &config,
            &mut McpHub::new(Vec::new()),
            &mut permissions,
            &mut Checkpoints::default(),
            &mut rx,
            &std::sync::Mutex::new(std::collections::VecDeque::new()),
            &mut crate::background::BackgroundTasks::default(),
            CancellationToken::new(),
            |event| events.push(event),
        )
        .await;
        assert_eq!(outcome(&events), TurnOutcome::Completed);
        let requested = events.iter().filter(|e| matches!(e, EventKind::ToolCallRequested { .. })).count();
        assert_eq!(requested, 1, "one row per call, even when it's refused");
        assert!(events.iter().any(|e| matches!(e, EventKind::ToolFinished { call_id, success: false, summary, .. } if call_id == "call_1" && summary == "not in this mode")));
        assert!(!dir.path().join("src/new.rs").exists());
    }

    #[tokio::test]
    async fn the_model_saves_a_memory_and_plan_mode_keeps_it_read_only() {
        const SAVE_CALL: &str = concat!(
            r#"{"model":"m","created_at":"2026-11-06T18:48:11Z","message":{"role":"assistant","content":"","tool_calls":[{"id":"call_1","function":{"index":0,"name":"memory","arguments":{"action":"save","name":"fedora-commands","description":"Which package manager to use","type":"feedback","body":"Always dnf, never apt."}}}]},"done":false}"#,
            "\n",
            r#"{"model":"m","created_at":"2026-11-06T18:48:11Z","message":{"role":"assistant","content":""},"done":true,"done_reason":"stop","prompt_eval_count":10,"eval_count":5}"#,
            "\n"
        );
        let data = tempfile::tempdir().unwrap();
        let places = crate::memory::Places::new(data.path(), "/home/u/project");
        for plan_mode in [false, true] {
            let server = MockServer::start().await;
            Mock::given(path("/api/chat"))
                .respond_with(ResponseTemplate::new(200).set_body_string(SAVE_CALL))
                .up_to_n_times(1)
                .with_priority(1)
                .mount(&server)
                .await;
            Mock::given(path("/api/chat"))
                .respond_with(ResponseTemplate::new(200).set_body_string(RECORDED_TEXT))
                .mount(&server)
                .await;
            let dir = tempfile::tempdir().unwrap();
            let workspace = Workspace::new(dir.path()).unwrap();
            let mut chat = Chat::new(OllamaProvider::new(&server.uri()).unwrap(), 4096);
            let mut tools = tool_definitions();
            tools.push(crate::memory::tool_definition(crate::memory::Mode::On));
            let config = AgentConfig {
                tools,
                custom: HashMap::new(),
                extra_system: Vec::new(),
                project_trusted: true,
                plan_path: std::env::temp_dir().join("re-code-test-plan.md"),
                max_steps: MAX_AGENT_STEPS,
                hooks: std::sync::Arc::default(),
                spawn_provider: None,
                depth: 0,
                memory: Some(MemoryConfig { places: places.clone(), read_only: false }),
                skills: Vec::new(),
                invoked_text: None,
            };
            let mut permissions = Permissions::default();
            if plan_mode {
                permissions.mode = PermissionMode::Plan;
            }
            // A save asks first; this user said "always" earlier in the session.
            let save =
                ToolCall { id: "c".into(), name: "memory".into(), arguments: json!({"action": "save"}), extra: None };
            assert!(matches!(reach(&save, &workspace, &HashMap::new()), Reach::Ask { grant: Some(_), .. }));
            let read =
                ToolCall { id: "c".into(), name: "memory".into(), arguments: json!({"action": "read"}), extra: None };
            assert_eq!(reach(&read, &workspace, &HashMap::new()), Reach::Free);
            permissions.allow(Grant::Exact("memory:write".into()));
            let (_tx, mut rx) = mpsc::unbounded_channel();
            let mut events = Vec::new();
            chat.agent_turn(
                "remember that I use dnf".to_owned(),
                Vec::new(),
                &workspace,
                &config,
                &mut McpHub::new(Vec::new()),
                &mut permissions,
                &mut Checkpoints::default(),
                &mut rx,
                &std::sync::Mutex::new(std::collections::VecDeque::new()),
                &mut crate::background::BackgroundTasks::default(),
                CancellationToken::new(),
                |event| events.push(event),
            )
            .await;
            assert_eq!(outcome(&events), TurnOutcome::Completed);
            let finished = events.iter().find_map(|e| match e {
                EventKind::ToolFinished { call_id, success, summary } if call_id == "call_1" => {
                    Some((*success, summary.clone()))
                }
                _ => None,
            });
            if plan_mode {
                assert_eq!(finished, Some((false, "refused".to_owned())));
            } else {
                assert_eq!(finished, Some((true, "remembered · project".to_owned())));
                let memories = crate::memory::list(&places);
                assert_eq!(memories.len(), 1);
                let part = crate::memory::system_part(&memories, crate::memory::Mode::On).unwrap();
                assert!(part.contains("- fedora-commands (feedback): Which package manager to use"), "{part}");
                std::fs::remove_file(&memories[0].path).unwrap();
            }
        }
        assert!(crate::memory::list(&places).is_empty(), "plan mode saved nothing");
    }

    #[test]
    fn export_names_carry_the_date_and_time() {
        // 2026-11-07 14:32 UTC.
        assert_eq!(
            export_file_name(recode_core::ids::UnixMillis(1_794_061_920_000)),
            "re-code-export-20261107-1432.md"
        );
        assert_eq!(export_file_name(recode_core::ids::UnixMillis(0)), "re-code-export-19700101-0000.md");
    }

    #[tokio::test]
    async fn rewinding_truncates_to_a_request_and_can_keep_a_summary() {
        let (server, mut chat) = server(RECORDED_TEXT).await;
        let _ = turn(&mut chat, "first request").await;
        let _ = turn(&mut chat, "second request").await;
        let _ = turn(&mut chat, "third request").await;
        assert_eq!(chat.turn_marks.len(), 3);
        let before_third = chat.turn_marks[2].0;
        let (keep, summary) = chat.rewind(2, false, &CancellationToken::new()).await.unwrap();
        assert_eq!((keep, summary), (0, None));
        assert_eq!(chat.messages.len(), before_third);
        assert!(!chat.messages.iter().any(|m| m.content.contains("third request")));
        assert!(chat.messages.iter().any(|m| m.content.contains("second request")));
        assert_eq!(chat.turn_marks.len(), 2);
        // Summarising keeps the model's words in place of the removed part (the mock answers with text).
        let (_, summary) = chat.rewind(1, true, &CancellationToken::new()).await.unwrap();
        assert!(summary.is_some());
        assert!(chat.messages.last().unwrap().content.starts_with(compact::SUMMARY_PREFIX));
        assert!(!chat.messages.iter().any(|m| m.content == "second request"));
        assert!(chat.rewind(7, false, &CancellationToken::new()).await.is_err());
        drop(server);
    }

    #[test]
    fn idle_compaction_waits_the_set_minutes_and_only_when_over_half_full() {
        let now = std::time::Instant::now();
        assert_eq!(idle_deadline(now, None, true), None, "off by default");
        assert_eq!(idle_deadline(now, Some(15), false), None, "nothing to gain below half");
        assert_eq!(idle_deadline(now, Some(15), true), Some(now + std::time::Duration::from_secs(900)));
    }

    #[tokio::test]
    async fn reported_usage_calibrates_the_estimate_and_a_cut_prompt_is_noticed() {
        let server = MockServer::start().await;
        let mut chat = Chat::new(OllamaProvider::new(&server.uri()).unwrap(), 8192);
        chat.model = "m".to_owned();
        let usage = |prompt: u64, cached: Option<u64>| recode_core::usage::ReportedUsage {
            prompt_tokens: Some(prompt),
            cached_prompt_tokens: cached,
            ..recode_core::usage::ReportedUsage::default()
        };
        // Three plausible samples: 4 chars per token from now on.
        for _ in 0..3 {
            chat.learn(&usage(1000, None), 4000);
        }
        assert_eq!(chat.calibration.ratio(), 4.0);
        assert!(chat.warnings.is_empty() && !chat.force_compact_next);
        // Cached tokens make the figure meaningless for learning, so it's skipped.
        chat.learn(&usage(10, Some(990)), 4000);
        assert_eq!(chat.calibration.samples, 3);
        // 30k characters can't be 1,200 tokens: the runtime dropped something.
        chat.learn(&usage(1200, None), 30_000);
        assert!(chat.force_compact_next, "the next request compacts first");
        assert_eq!(chat.warnings.len(), 1);
        assert!(chat.warnings[0].contains("probably dropped"), "{}", chat.warnings[0]);
        assert_eq!(chat.calibration.samples, 3, "a cut sample isn't learnt from");
        // The flag makes the next fit compact even below the trigger.
        long_agent_history(&mut chat, 6, 500);
        let mut events = Vec::new();
        chat.fit(&[], None, false, &CancellationToken::new(), &mut |event| events.push(event)).await.unwrap();
        assert!(events.iter().any(|e| matches!(e, EventKind::Compacted { .. })), "{events:?}");
        assert!(!chat.force_compact_next);
        // A cloud model is never suspected: its runtime doesn't truncate silently.
        let (window, _) = resolve_window(None, "cloud/m", Some(100_000), false, 16384);
        chat.set_backend(Box::new(OpenAiProvider::new(&server.uri(), "k").unwrap()), "cloud/m".into(), window);
        chat.learn(&usage(1200, None), 30_000);
        assert_eq!(chat.warnings.len(), 1);
    }

    #[tokio::test]
    async fn after_a_summary_recent_files_and_skills_come_back_and_thrashing_stops_the_turn() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("small.rs"), "fn main() {}\n").unwrap();
        std::fs::write(dir.path().join("huge.rs"), "x".repeat(40_000)).unwrap();
        std::fs::create_dir_all(dir.path().join("skills/greet")).unwrap();
        std::fs::write(dir.path().join("skills/greet/SKILL.md"), "---\nname: greet\ndescription: d\n---\nSay ahoy.\n")
            .unwrap();
        let workspace = Workspace::new(dir.path()).unwrap();
        let server = MockServer::start().await;
        let mut chat = Chat::new(OllamaProvider::new(&server.uri()).unwrap(), 8192);
        chat.model = "m".to_owned();
        chat.note_file("small.rs".into());
        chat.note_file("huge.rs".into());
        chat.note_file("gone.rs".into());
        chat.note_file("/etc/hostname".into());
        chat.note_file("small.rs".into());
        assert_eq!(chat.recent_files.last().map(String::as_str), Some("small.rs"), "latest last, no duplicates");
        chat.invoked_skills.push(("greet".into(), dir.path().join("skills/greet/SKILL.md").display().to_string()));
        chat.reinject(Some(&workspace), 3.0);
        let notes = chat.notes.join("\n---\n");
        assert!(notes.contains("small.rs as it is now:\nfn main() {}"), "{notes}");
        assert!(notes.contains("huge.rs; it's too long to repeat"), "{notes}");
        assert!(notes.contains("gone.rs; it no longer exists"), "{notes}");
        assert!(notes.contains("/etc/hostname; read it again"), "outside files by path only: {notes}");
        assert!(notes.contains("skill `greet`; its instructions again:\nSay ahoy."), "{notes}");

        // Three summaries in one turn mean a step that can't fit: the turn stops with an explanation.
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(OPENAI_SUMMARY_SSE))
            .mount(&server)
            .await;
        let (window, _) = resolve_window(None, "cloud/m", Some(2048), false, 16384);
        chat.set_backend(Box::new(OpenAiProvider::new(&server.uri(), "k").unwrap()), "cloud/m".into(), window);
        chat.notes.clear();
        long_agent_history(&mut chat, 10, 500);
        chat.compactions_this_turn = 2;
        let mut events = Vec::new();
        let result = chat.fit(&[], Some(&workspace), false, &CancellationToken::new(), &mut |e| events.push(e)).await;
        assert!(result.unwrap_err().contains("three times"));
        assert_eq!(chat.compaction_records.len(), 1, "the record is kept for /compactions");
        assert_eq!(chat.compaction_records[0].0.seq, 1);
        assert!(!chat.compaction_records[0].1.is_empty(), "the replaced messages travel with the report");
    }

    const OPENAI_SUMMARY_SSE: &str = "data: {\"choices\":[{\"delta\":{\"content\":\"Objective: build it. Findings: summarised.\"},\"finish_reason\":null}]}\n\n\
data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
data: [DONE]\n\n";

    #[tokio::test]
    async fn a_cloud_model_is_compacted_at_its_listed_limit_and_left_alone_without_one() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(OPENAI_SUMMARY_SSE))
            .mount(&server)
            .await;
        let cloud = || Box::new(OpenAiProvider::new(&server.uri(), "k").unwrap()) as Box<dyn Provider>;
        let mut chat = Chat::new(OllamaProvider::new(&server.uri()).unwrap(), 4096);

        // The listing says 2k: that is the window, with no num_ctx sent anywhere.
        let (window, _) = resolve_window(None, "cloud/m", Some(2048), false, 16384);
        assert_eq!((window.window, window.local_num_ctx), (Some(2048), None));
        chat.set_backend(cloud(), "cloud/m".to_owned(), window);
        assert_eq!(chat.options(), ProviderOptions::None);
        long_agent_history(&mut chat, 10, 500);
        let mut events = Vec::new();
        chat.fit(&[], None, false, &CancellationToken::new(), &mut |event| events.push(event)).await.unwrap();
        let Some(EventKind::Compacted { elided, summarised, problem, .. }) =
            events.iter().find(|e| matches!(e, EventKind::Compacted { .. })).cloned()
        else {
            panic!("a cloud model was not compacted: {events:?}");
        };
        assert_eq!(problem, None);
        assert!(elided > 0 && summarised > 0, "shortened {elided}, summarised {summarised}");
        assert!(chat.messages.iter().any(|m| m.content.contains("Findings: summarised.")));

        // No limit in the listing and no setting: nothing happens on its own, but /compact still works.
        let mut chat = Chat::new(OllamaProvider::new(&server.uri()).unwrap(), 4096);
        let (window, _) = resolve_window(None, "cloud/other", None, false, 16384);
        assert_eq!((window.window, window.trigger), (None, None));
        chat.set_backend(cloud(), "cloud/other".to_owned(), window);
        long_agent_history(&mut chat, 10, 500);
        let mut events = Vec::new();
        chat.fit(&[], None, false, &CancellationToken::new(), &mut |event| events.push(event)).await.unwrap();
        assert!(events.is_empty(), "{events:?}");
        chat.fit(&[], None, true, &CancellationToken::new(), &mut |event| events.push(event)).await.unwrap();
        assert!(events.iter().any(|e| matches!(e, EventKind::Compacted { elided, .. } if *elided > 0)), "{events:?}");

        // A pinned size works without a listing.
        let setting = WindowSetting { size: recode_core::context::Size::Tokens(4096), ..WindowSetting::default() };
        let window = ContextWindow::resolve(&setting, None, false, 16384);
        assert_eq!(window.window, Some(4096));
        assert!(window.trigger.is_some());
    }

    #[tokio::test]
    async fn older_messages_are_summarised_by_the_model_when_shortening_is_not_enough() {
        let (server, mut chat) = server(RECORDED_TEXT).await;
        chat.messages.push(Message::user("build the parser"));
        for n in 0..20 {
            chat.messages.push(Message::assistant(format!("step {n}: {}", "y".repeat(300)), Vec::new()));
            chat.messages.push(Message::user(format!("and then {n}: {}", "z".repeat(300))));
        }
        let events = turn(&mut chat, "what's left?").await;
        assert_eq!(outcome(&events), TurnOutcome::Completed);
        assert!(events.iter().any(|e| matches!(e, EventKind::Compacted { summarised, .. } if *summarised > 20)));
        assert_eq!(chat.messages[1].content, "build the parser", "the objective is kept verbatim");
        assert!(chat.messages[2].content.contains("Hello there friend."), "the model's summary replaced the middle");
        assert!(chat.messages.iter().any(|m| m.content == "what's left?"), "the new request is kept");
        // One request for the summary, one for the reply.
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_failed_summary_keeps_the_session_intact_and_sends_nothing_oversized() {
        let server = MockServer::start().await;
        Mock::given(path("/api/chat")).respond_with(ResponseTemplate::new(500)).mount(&server).await;
        let mut chat = Chat::new(OllamaProvider::new(&server.uri()).unwrap(), 4096);
        chat.model = "m".to_owned();
        chat.messages.push(Message::user("build the parser"));
        for n in 0..40 {
            chat.messages.push(Message::assistant(format!("step {n}: {}", "y".repeat(300)), Vec::new()));
            chat.messages.push(Message::user(format!("and then {n}")));
        }
        let before = chat.messages.clone();
        let events = turn(&mut chat, "continue").await;
        assert!(matches!(outcome(&events), TurnOutcome::Failed { reason } if reason.contains("wasn't sent")));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, EventKind::Compacted { problem: Some(p), .. } if p.contains("didn't work")))
        );
        assert_eq!(chat.messages, before, "the conversation is unchanged and the rejected message isn't kept");
        // Only the summary was attempted; the oversized request itself never went out.
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn renaming_a_provider_keeps_its_key_and_clearing_removes_it() {
        let store = crate::secrets::tests::MemoryStore::default();
        assert_eq!(store_provider_key(&store, None, "zen", Some("sk-1")).await, None);
        // Renamed without typing a key: the key follows the new name.
        assert_eq!(store_provider_key(&store, Some("zen"), "opencode", None).await, None);
        assert_eq!(store.get("opencode").await.unwrap().as_deref(), Some("sk-1"));
        assert_eq!(store.get("zen").await.unwrap(), None, "nothing left under the old name");
        // A blank key clears it.
        assert_eq!(store_provider_key(&store, Some("opencode"), "opencode", Some("  ")).await, None);
        assert_eq!(store.get("opencode").await.unwrap(), None);
    }

    #[tokio::test]
    async fn the_todo_tool_updates_the_task_list_and_survives_saving() {
        const TODO_CALL: &str = concat!(
            r#"{"model":"m","created_at":"2026-11-06T18:48:11Z","message":{"role":"assistant","content":"","tool_calls":[{"id":"call_1","function":{"index":0,"name":"todo","arguments":{"items":[{"text":"Read the code","status":"done"},{"text":"Fix it","status":"in_progress"}]}}}]},"done":false}"#,
            "\n",
            r#"{"model":"m","created_at":"2026-11-06T18:48:11Z","message":{"role":"assistant","content":""},"done":true,"done_reason":"stop","prompt_eval_count":10,"eval_count":5}"#,
            "\n"
        );
        let server = MockServer::start().await;
        Mock::given(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_string(TODO_CALL))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_string(RECORDED_TEXT))
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(dir.path()).unwrap();
        let mut chat = Chat::new(OllamaProvider::new(&server.uri()).unwrap(), 4096);
        chat.model = "m".to_owned();
        let config = AgentConfig {
            tools: tool_definitions(),
            custom: HashMap::new(),
            extra_system: Vec::new(),
            project_trusted: true,
            plan_path: std::env::temp_dir().join("re-code-test-plan.md"),
            max_steps: MAX_AGENT_STEPS,
            hooks: std::sync::Arc::default(),
            spawn_provider: None,
            depth: 0,
            memory: None,
            skills: Vec::new(),
            invoked_text: None,
        };
        assert!(
            chat.offered_tools(&config.tools).iter().any(|t| t.name == crate::todo::TOOL),
            "offered with the agent's tools"
        );
        let (_tx, mut rx) = mpsc::unbounded_channel();
        let mut events = Vec::new();
        let queued: Queued =
            std::sync::Mutex::new(std::collections::VecDeque::from([("also update the docs".to_owned(), Vec::new())]));
        chat.agent_turn(
            "fix the parser".to_owned(),
            Vec::new(),
            &workspace,
            &config,
            &mut McpHub::new(Vec::new()),
            &mut Permissions::default(),
            &mut Checkpoints::default(),
            &mut rx,
            &queued,
            &mut crate::background::BackgroundTasks::default(),
            CancellationToken::new(),
            |event| events.push(event),
        )
        .await;
        assert_eq!(outcome(&events), TurnOutcome::Completed);
        // A message queued while the model worked was delivered after the tool batch, before the next request.
        let delivered: Vec<&String> = events
            .iter()
            .filter_map(|e| match e {
                EventKind::QueuedDelivered { text } => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(delivered, ["also update the docs"]);
        let position = chat.messages.iter().position(|m| m.content == "also update the docs").unwrap();
        assert!(
            matches!(chat.messages[position - 1].role, recode_core::message::Role::Tool),
            "right after the tool result"
        );
        let updated = events.iter().find_map(|e| match e {
            EventKind::TodoUpdated { items } => Some(items.clone()),
            _ => None,
        });
        let items = updated.expect("the task list was announced");
        assert_eq!(items.len(), 2);
        assert_eq!(items[1].status, recode_core::protocol::TodoStatus::InProgress);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, EventKind::ToolFinished { success: true, summary, .. } if summary == "2 tasks"))
        );
        let saved = chat.saved();
        assert_eq!(saved.todos, items, "kept with the conversation");
        let mut again = Chat::new(OllamaProvider::new(&server.uri()).unwrap(), 4096);
        again.restore(saved);
        assert_eq!(again.todos.len(), 2);
    }

    #[test]
    fn a_shell_approval_says_where_it_runs_and_network_is_its_own_permission() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(dir.path()).unwrap();
        let call = |network: bool| ToolCall {
            id: "1".to_owned(),
            name: "shell".to_owned(),
            arguments: json!({"command": "cargo fetch", "network": network}),
            extra: None,
        };
        let none = HashMap::new();
        let (offline, _) = describe_tool(&call(false), &workspace, &reach(&call(false), &workspace, &none));
        let (online, _) = describe_tool(&call(true), &workspace, &reach(&call(true), &workspace, &none));
        if sandbox::isolation().is_sandboxed() {
            assert!(offline.contains("in the sandbox, without network"), "{offline}");
            assert!(online.contains("in the sandbox, with network access"), "{online}");
        } else {
            assert!(offline.contains("WITHOUT a sandbox"), "{offline}");
            assert!(matches!(reach(&call(false), &workspace, &none), Reach::Ask { grant: None, .. }));
        }
        // Allowing a command offline doesn't allow it with the network on.
        let mut perms = Permissions::default();
        if let Reach::Ask { grant: Some(grant), .. } = reach(&call(false), &workspace, &none) {
            perms.allow(grant);
            assert!(perms.auto(&reach(&call(false), &workspace, &none)));
        }
        assert!(!perms.auto(&reach(&call(true), &workspace, &none)));
    }

    /// Live: a real model writes the compaction summary. Prints it, since its quality is a judgement.
    #[tokio::test]
    #[ignore = "needs a running Ollama with the model"]
    async fn live_compaction_summarises_with_a_real_model() {
        let model = std::env::var("RECODE_LIVE_MODEL").unwrap_or_else(|_| "gpt-oss:20b".to_owned());
        let mut chat = Chat::local(OllamaProvider::new("http://localhost:11434").unwrap(), 8192);
        chat.model = model;
        chat.messages.push(Message::user("Add a config file parser to the CLI in src/config.rs"));
        let facts = [
            "I looked at src/main.rs: arguments are parsed with clap 4.",
            "Decision: use the toml crate rather than serde_yaml, because the existing files are TOML.",
            "Created src/config.rs with a Config struct and Config::load(path).",
            "cargo test failed: error[E0433]: failed to resolve: use of undeclared crate `toml`.",
            "Added toml = \"0.8\" to Cargo.toml; the tests now pass.",
            "Still to do: wire Config::load into main.rs and document the file in README.md.",
        ];
        for n in 0..30 {
            chat.messages
                .push(Message::assistant(format!("{} {}", facts[n % facts.len()], "detail ".repeat(40)), Vec::new()));
            chat.messages.push(Message::user(format!("ok, continue ({n})")));
        }
        let mut events = Vec::new();
        chat.fit(&[], None, true, &CancellationToken::new(), &mut |event| events.push(event)).await.unwrap();
        let compacted = events.iter().find_map(|e| match e {
            EventKind::Compacted { before_tokens, after_tokens, summarised, problem, .. } => {
                Some((*before_tokens, *after_tokens, *summarised, problem.clone()))
            }
            _ => None,
        });
        let (before, after, summarised, problem) = compacted.expect("a compaction was reported");
        eprintln!("BEFORE~{before} AFTER~{after} SUMMARISED={summarised} PROBLEM={problem:?}");
        eprintln!("SUMMARY:\n{}", chat.messages[2].content);
        assert_eq!(problem, None);
        assert!(summarised > 20 && after < before / 2);
        assert_eq!(chat.messages[1].content, "Add a config file parser to the CLI in src/config.rs");
    }

    /// Live: a real model uses the sandboxed `shell` (with its `network` parameter in the schema) and
    /// `write`, and the write is checkpointed so it can be undone. Run with `--ignored`.
    #[tokio::test]
    #[ignore = "needs a running Ollama with a tool-capable model"]
    async fn live_agent_runs_a_sandboxed_command_and_its_edit_can_be_undone() {
        let model = std::env::var("RECODE_LIVE_MODEL").unwrap_or_else(|_| "gpt-oss:20b".to_owned());
        let dir = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(dir.path()).unwrap();
        let mut chat = Chat::local(OllamaProvider::new("http://localhost:11434").unwrap(), 8192);
        chat.model = model;
        let config = AgentConfig {
            tools: tool_definitions(),
            custom: HashMap::new(),
            extra_system: Vec::new(),
            project_trusted: true,
            plan_path: std::env::temp_dir().join("re-code-test-plan.md"),
            max_steps: MAX_AGENT_STEPS,
            hooks: std::sync::Arc::default(),
            spawn_provider: None,
            depth: 0,
            memory: None,
            skills: Vec::new(),
            invoked_text: None,
        };
        let mut hub = McpHub::new(Vec::new());
        let mut permissions = Permissions::default();
        let mut checkpoints = Checkpoints::default();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut events = Vec::new();
        chat.agent_turn(
            "Do exactly two things. First, use the shell tool to run: uname -s. Second, use the write tool to \
             create hello.txt containing the single word sandboxed. Then reply done."
                .to_owned(),
            Vec::new(),
            &workspace,
            &config,
            &mut hub,
            &mut permissions,
            &mut checkpoints,
            &mut rx,
            &std::sync::Mutex::new(std::collections::VecDeque::new()),
            &mut crate::background::BackgroundTasks::default(),
            CancellationToken::new(),
            |event| {
                if let EventKind::ApprovalRequired { approval, description, .. } = &event {
                    eprintln!("APPROVAL: {description}");
                    let _ = tx.send((*approval, ApprovalDecision::AllowOnce));
                }
                if let EventKind::ToolOutput { text, .. } = &event {
                    eprintln!("OUTPUT: {}", text.lines().take(3).collect::<Vec<_>>().join(" | "));
                }
                events.push(event);
            },
        )
        .await;
        let ran_shell = events.iter().any(|event| {
            matches!(event, EventKind::ToolOutput { text, .. } if text.starts_with("exit code 0") && text.contains("Linux"))
        });
        assert!(ran_shell, "the sandboxed shell call didn't succeed:\n{events:#?}");
        assert!(dir.path().join("hello.txt").exists(), "the model didn't write the file:\n{events:#?}");
        let report = checkpoints.undo(&workspace).expect("the turn is undoable");
        assert_eq!(report.removed, vec!["hello.txt"]);
        assert!(report.untracked.iter().any(|what| what.contains("uname")), "{report:?}");
        assert!(!dir.path().join("hello.txt").exists());
    }

    /// Live: a real model reads and changes a file outside its project folder, with the user's
    /// approval on each step, and undo restores it.
    #[tokio::test]
    #[ignore = "needs a running Ollama with a tool-capable model"]
    async fn live_agent_reads_and_edits_a_file_outside_the_project() {
        let model = std::env::var("RECODE_LIVE_MODEL").unwrap_or_else(|_| "gpt-oss:20b".to_owned());
        let project = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let notes = elsewhere.path().canonicalize().unwrap().join("notes.md");
        std::fs::write(&notes, "# Shopping\n- milk\n").unwrap();
        let workspace = Workspace::new(project.path()).unwrap();
        let mut chat = Chat::local(OllamaProvider::new("http://localhost:11434").unwrap(), 8192);
        chat.model = model;
        let config = AgentConfig {
            tools: tool_definitions(),
            custom: HashMap::new(),
            extra_system: Vec::new(),
            project_trusted: true,
            plan_path: std::env::temp_dir().join("re-code-test-plan.md"),
            max_steps: MAX_AGENT_STEPS,
            hooks: std::sync::Arc::default(),
            spawn_provider: None,
            depth: 0,
            memory: None,
            skills: Vec::new(),
            invoked_text: None,
        };
        let mut checkpoints = Checkpoints::default();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut cards = Vec::new();
        let mut trace = Vec::new();
        chat.agent_turn(
            format!("Add \"- eggs\" to the shopping list in {}.", notes.display()),
            Vec::new(),
            &workspace,
            &config,
            &mut McpHub::new(Vec::new()),
            &mut Permissions::default(),
            &mut checkpoints,
            &mut rx,
            &std::sync::Mutex::new(std::collections::VecDeque::new()),
            &mut crate::background::BackgroundTasks::default(),
            CancellationToken::new(),
            |event| match event {
                EventKind::ApprovalRequired { approval, description, always, .. } => {
                    cards.push(format!("{description} [a: {always:?}]"));
                    let _ = tx.send((approval, ApprovalDecision::AllowOnce));
                }
                EventKind::ToolCallRequested { call } => trace.push(format!("call {} {}", call.name, call.arguments)),
                EventKind::ToolFinished { success, summary, .. } => trace.push(format!("  -> {success} {summary}")),
                EventKind::TurnFinished { outcome, .. } => trace.push(format!("turn {outcome:?}")),
                _ => {}
            },
        )
        .await;
        eprintln!("TRACE:\n{}\nCARDS:\n{}", trace.join("\n"), cards.join("\n---\n"));
        let after = std::fs::read_to_string(&notes).unwrap();
        assert!(after.contains("eggs") && after.contains("milk"), "the file wasn't changed as asked: {after}");
        assert!(cards.iter().any(|card| card.contains("outside the project")), "the card must say it's outside");
        let report = checkpoints.undo(&workspace).expect("the outside edit is undoable");
        assert_eq!(report.restored.len(), 1, "{report:?}");
        assert_eq!(std::fs::read_to_string(&notes).unwrap(), "# Shopping\n- milk\n");
    }

    /// Live: a real model uses the user's running RustRover through its MCP server, read live from
    /// Claude Code's config. Only read-only RustRover tools are approved; anything else is denied, so
    /// the test can't change a project.
    #[tokio::test]
    #[ignore = "needs Ollama and RustRover running with its MCP server"]
    async fn live_agent_uses_the_running_rustrover_mcp() {
        const READ_ONLY: &[&str] = &[
            "get_project_modules",
            "get_project_dependencies",
            "get_all_open_file_paths",
            "list_directory_tree",
            "get_repositories",
            "get_run_configurations",
            "search_file",
            "search_text",
            "find_files_by_name_keyword",
        ];
        let home = std::env::var_os("HOME").unwrap();
        let servers = recode_extensions::claude::servers(&std::path::Path::new(&home).join(".claude.json"), None)
            .into_iter()
            .filter(|server| server.name == "rustrover")
            .collect::<Vec<_>>();
        assert_eq!(servers.len(), 1, "rustrover should be in ~/.claude.json");
        let mut hub = McpHub::new(servers);
        let model = std::env::var("RECODE_LIVE_MODEL").unwrap_or_else(|_| "gpt-oss:20b".to_owned());
        let dir = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(dir.path()).unwrap();
        let mut chat = Chat::local(OllamaProvider::new("http://localhost:11434").unwrap(), 16384);
        chat.model = model;
        let mut tools = tool_definitions();
        tools.extend(mcp_meta_tools(&hub.server_names()));
        let config = AgentConfig {
            tools,
            custom: HashMap::new(),
            extra_system: Vec::new(),
            project_trusted: true,
            plan_path: std::env::temp_dir().join("re-code-test-plan.md"),
            max_steps: MAX_AGENT_STEPS,
            hooks: std::sync::Arc::default(),
            spawn_provider: None,
            depth: 0,
            memory: None,
            skills: Vec::new(),
            invoked_text: None,
        };
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut events = Vec::new();
        let mut calls: HashMap<String, ToolCall> = HashMap::new();
        chat.agent_turn(
            "Use the rustrover MCP server to find out which modules the open project has. First call \
             list_mcp_tools with server rustrover, then call the right tool with call_mcp_tool. Report what it returned."
                .to_owned(),
            Vec::new(),
            &workspace,
            &config,
            &mut hub,
            &mut Permissions::default(),
            &mut Checkpoints::default(),
            &mut rx,
            &std::sync::Mutex::new(std::collections::VecDeque::new()),
            &mut crate::background::BackgroundTasks::default(),
            CancellationToken::new(),
            |event| {
                if let EventKind::ToolCallRequested { call } = &event {
                    calls.insert(call.id.clone(), call.clone());
                }
                if let EventKind::ApprovalRequired { approval, call_id, .. } = &event {
                    let tool = calls.get(call_id).and_then(|c| c.arguments.get("tool")).and_then(|t| t.as_str());
                    let safe = tool.is_some_and(|tool| READ_ONLY.contains(&tool));
                    eprintln!("APPROVAL for {tool:?}: {}", if safe { "allowed" } else { "denied" });
                    let decision = if safe { ApprovalDecision::AllowOnce } else { ApprovalDecision::Deny };
                    let _ = tx.send((*approval, decision));
                }
                if let EventKind::ToolOutput { text, .. } = &event {
                    eprintln!("OUTPUT: {}", text.chars().take(240).collect::<String>().replace('\n', " | "));
                }
                events.push(event);
            },
        )
        .await;
        hub.shutdown().await;
        let trace: Vec<String> = events
            .iter()
            .filter_map(|e| match e {
                EventKind::ToolCallRequested { call } => Some(format!("call {} {}", call.name, call.arguments)),
                EventKind::ToolFinished { success, summary, .. } => Some(format!("  -> {success} {summary}")),
                EventKind::Compacted { before_tokens, after_tokens, .. } => {
                    Some(format!("compacted {before_tokens}->{after_tokens}"))
                }
                EventKind::TurnFinished { outcome, .. } => Some(format!("turn {outcome:?}")),
                _ => None,
            })
            .collect();
        eprintln!("TRACE:\n{}", trace.join("\n"));
        let listed =
            events.iter().any(|e| matches!(e, EventKind::ToolCallRequested { call } if call.name == LIST_MCP_TOOLS));
        assert!(listed, "the model never listed RustRover's tools");
        let succeeded = events.iter().any(|e| {
            matches!(e, EventKind::ToolFinished { call_id, success: true, .. }
                if calls.get(call_id).is_some_and(|c| c.name == CALL_MCP_TOOL))
        });
        assert!(succeeded, "no RustRover tool call succeeded");
    }

    /// End-to-end against a live Ollama tool-capable model and a real stdio MCP server: the model must
    /// discover the MCP tool (`list_mcp_tools`), then call it (`call_mcp_tool`), and the server's text
    /// must come back. This proves the headline fix — the model can actually reach MCP — on the real
    /// agent loop, not a mock. Model defaults to gpt-oss:20b (a verified tool-caller); override with
    /// RECODE_LIVE_MODEL. Run: `cargo test -p recode-app -- --ignored live_agent_uses_an_mcp_tool`
    #[tokio::test]
    #[ignore = "needs a running Ollama with a tool-capable model"]
    async fn live_agent_uses_an_mcp_tool_end_to_end() {
        // A tiny real MCP server (POSIX shell) exposing one tool that transforms its input, so the
        // result could only have come from an actual MCP round-trip.
        const MCP_SERVER: &str = r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -nE 's/.*"id":([0-9]+).*/\1/p')
  case "$line" in
    *'"initialize"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"demo-srv","version":"0.1.0"}}}\n' "$id" ;;
    *'"tools/list"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"shout","description":"Returns the given text wrapped in a recognisable marker","inputSchema":{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}}]}}\n' "$id" ;;
    *'"tools/call"'*)
      text=$(printf '%s' "$line" | sed -nE 's/.*"text":"([^"]*)".*/\1/p')
      printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"MCP-MARKER[%s]"}]}}\n' "$id" "$text" ;;
  esac
done
"#;
        let model = std::env::var("RECODE_LIVE_MODEL").unwrap_or_else(|_| "gpt-oss:20b".to_owned());
        let dir = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(dir.path()).unwrap();

        let provider = OllamaProvider::new("http://localhost:11434").unwrap();
        let mut chat = Chat::local(provider, 8192);
        chat.model = model.clone();

        let srv = McpServer {
            name: "demo".to_owned(),
            transport: recode_core::extensions::McpTransport::Stdio {
                command: "sh".to_owned(),
                args: vec!["-c".to_owned(), MCP_SERVER.to_owned()],
                env: BTreeMap::new(),
            },
            enabled: true,
            expose_to_agent: false,
            external: None,
            tools: None,
            sandbox: recode_core::extensions::McpSandbox::default(),
        };
        let mut hub = McpHub::new(vec![srv]);

        let mut tools = tool_definitions();
        tools.extend(mcp_meta_tools(&hub.server_names()));
        let config = AgentConfig {
            tools,
            custom: HashMap::new(),
            extra_system: Vec::new(),
            project_trusted: true,
            plan_path: std::env::temp_dir().join("re-code-test-plan.md"),
            max_steps: MAX_AGENT_STEPS,
            hooks: std::sync::Arc::default(),
            spawn_provider: None,
            depth: 0,
            memory: None,
            skills: Vec::new(),
            invoked_text: None,
        };

        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut permissions = Permissions::default();
        let cancel = CancellationToken::new();

        let mut events = Vec::new();
        chat.agent_turn(
            "Use the \"demo\" MCP server to transform the exact text `hello world`. First call \
             list_mcp_tools to see what it offers, then invoke the tool with call_mcp_tool, passing \
             {\"text\": \"hello world\"} as its arguments. Then tell me exactly what it returned."
                .to_owned(),
            Vec::new(),
            &workspace,
            &config,
            &mut hub,
            &mut permissions,
            &mut Checkpoints::default(),
            &mut rx,
            &std::sync::Mutex::new(std::collections::VecDeque::new()),
            &mut crate::background::BackgroundTasks::default(),
            cancel,
            |event| {
                // No human is here, so approve every MCP call as it's requested.
                if let EventKind::ApprovalRequired { approval, .. } = &event {
                    let _ = tx.send((*approval, ApprovalDecision::AllowOnce));
                }
                events.push(event);
            },
        )
        .await;
        hub.shutdown().await;

        let called = |name: &str| {
            events.iter().any(|event| matches!(event, EventKind::ToolCallRequested { call } if call.name == name))
        };
        assert!(called(LIST_MCP_TOOLS), "the model never discovered MCP tools:\n{events:#?}");
        assert!(called(CALL_MCP_TOOL), "the model never called an MCP tool:\n{events:#?}");

        let got_result = events.iter().any(
            |event| matches!(event, EventKind::ToolOutput { text, .. } if text.contains("MCP-MARKER[hello world]")),
        );
        assert!(got_result, "the MCP server's result never came back through the hub:\n{events:#?}");
    }
}
