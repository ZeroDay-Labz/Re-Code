//! Installing a skill from a hub: ask what to fetch, wait, show the preview the way Hermes Agent
//! does (name, label, pinned commit, what it declares, the scan), and take the decision. Also the hub
//! search overlay.

use crate::app::{Action, App, Overlay, ToastLevel};
use crate::theme::{self, Token};
use crate::view::{boxed, overlay_area};
use crate::widgets::{chip, legend};
use crate::wrap::{truncate, wrap_plain};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use recode_core::extensions::{
    ExtensionCommand, InstallOutcome, InstallPreview, PluginListing, PluginOutcome, PluginPreview, SkillCandidate,
    SkillSearchHit,
};

/// What the install window installs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InstallKind {
    #[default]
    Skill,
    Plugin,
    /// Following a Claude Code marketplace (the result is a home message, not a preview).
    Marketplace,
}

/// The install overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallState {
    /// What's being installed, as typed.
    pub tap: String,
    pub kind: InstallKind,
    pub phase: InstallPhase,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallPhase {
    /// Typing what to install.
    Ask,
    Fetching,
    Preview {
        preview: Box<InstallPreview>,
        /// Reading `SKILL.md` instead of the summary, with the scroll offset.
        reading: Option<usize>,
    },
    PluginPreview(Box<PluginPreview>),
    Choose {
        candidates: Vec<SkillCandidate>,
        note: Option<String>,
        selected: usize,
    },
    UpToDate {
        name: String,
        sha: String,
    },
    Failed(String),
}

impl App {
    /// `/skill install [what]`, `i` on the Skills tab, or the palette: ask, or start fetching at once.
    pub fn open_install(&mut self, tap: Option<String>) -> Vec<Action> {
        if self.demo {
            self.toast("That isn't available in the demo recording.", ToastLevel::Info);
            return Vec::new();
        }
        match tap.map(|t| t.trim().to_owned()).filter(|t| !t.is_empty()) {
            Some(tap) => self.start_install(tap),
            None => {
                self.overlay = Some(Overlay::Install(InstallState {
                    tap: String::new(),
                    kind: InstallKind::Skill,
                    phase: InstallPhase::Ask,
                }));
                Vec::new()
            }
        }
    }

    fn start_install(&mut self, tap: String) -> Vec<Action> {
        self.overlay = Some(Overlay::Install(InstallState {
            tap: tap.clone(),
            kind: InstallKind::Skill,
            phase: InstallPhase::Fetching,
        }));
        vec![Action::Extension(ExtensionCommand::StageSkill(tap))]
    }

    /// `/plugin install [what]`, `i` on the Plugins tab, or the palette.
    pub fn open_plugin_install(&mut self, tap: Option<String>) -> Vec<Action> {
        if self.demo {
            self.toast("That isn't available in the demo recording.", ToastLevel::Info);
            return Vec::new();
        }
        match tap.map(|t| t.trim().to_owned()).filter(|t| !t.is_empty()) {
            Some(tap) => self.start_plugin_install(tap),
            None => {
                self.overlay = Some(Overlay::Install(InstallState {
                    tap: String::new(),
                    kind: InstallKind::Plugin,
                    phase: InstallPhase::Ask,
                }));
                Vec::new()
            }
        }
    }

    fn start_plugin_install(&mut self, tap: String) -> Vec<Action> {
        self.overlay = Some(Overlay::Install(InstallState {
            tap: tap.clone(),
            kind: InstallKind::Plugin,
            phase: InstallPhase::Fetching,
        }));
        vec![Action::Extension(ExtensionCommand::StagePlugin(tap))]
    }

    /// `m` on the Plugins tab or `/plugin marketplace add`: ask which marketplace to follow.
    pub fn open_marketplace_add(&mut self) -> Vec<Action> {
        if self.demo {
            self.toast("That isn't available in the demo recording.", ToastLevel::Info);
            return Vec::new();
        }
        self.overlay = Some(Overlay::Install(InstallState {
            tap: String::new(),
            kind: InstallKind::Marketplace,
            phase: InstallPhase::Ask,
        }));
        Vec::new()
    }

    /// `u` on the Plugins tab or `/plugin update <name>`.
    pub fn update_plugin(&mut self, name: &str) -> Vec<Action> {
        if self.demo {
            self.toast("That isn't available in the demo recording.", ToastLevel::Info);
            return Vec::new();
        }
        let name = name.trim().to_owned();
        if name.is_empty() {
            self.toast("Name the plugin to update: /plugin update <name>.", ToastLevel::Info);
            return Vec::new();
        }
        self.overlay = Some(Overlay::Install(InstallState {
            tap: format!("update {name}"),
            kind: InstallKind::Plugin,
            phase: InstallPhase::Fetching,
        }));
        vec![Action::Extension(ExtensionCommand::UpdatePlugin(name))]
    }

    /// `/plugin browse [marketplace]` or `b` on the Plugins tab.
    pub fn browse_plugins(&mut self, name: &str) -> Vec<Action> {
        if self.demo {
            self.toast("That isn't available in the demo recording.", ToastLevel::Info);
            return Vec::new();
        }
        let name = name.trim();
        let name = if name.is_empty() { "hermes" } else { name };
        self.overlay = Some(Overlay::PluginBrowse {
            title: name.to_owned(),
            listings: Vec::new(),
            filter: String::new(),
            selected: 0,
            loading: true,
            error: None,
        });
        vec![Action::Extension(ExtensionCommand::BrowseMarketplace(name.to_owned()))]
    }

    /// `/plugin search <words>` or `s` on the Plugins tab.
    pub fn open_plugin_search(&mut self, query: Option<String>) -> Vec<Action> {
        if self.demo {
            self.toast("That isn't available in the demo recording.", ToastLevel::Info);
            return Vec::new();
        }
        let query = query.map(|q| q.trim().to_owned()).unwrap_or_default();
        let loading = !query.is_empty();
        self.overlay = Some(Overlay::PluginBrowse {
            title: if query.is_empty() { "search".to_owned() } else { format!("search: {query}") },
            listings: Vec::new(),
            filter: if loading { String::new() } else { query.clone() },
            selected: 0,
            loading,
            error: None,
        });
        if loading { vec![Action::Extension(ExtensionCommand::SearchPlugins(query))] } else { Vec::new() }
    }

    /// The driver finished staging a plugin.
    pub fn plugin_outcome(&mut self, outcome: PluginOutcome) {
        let waiting = matches!(
            &self.overlay,
            Some(Overlay::Install(InstallState { kind: InstallKind::Plugin, phase: InstallPhase::Fetching, .. }))
        );
        if !waiting {
            if let PluginOutcome::Preview(preview) = outcome {
                self.pending_actions.push(Action::Extension(ExtensionCommand::DiscardInstall(preview.staging)));
            }
            return;
        }
        let Some(Overlay::Install(state)) = &mut self.overlay else { return };
        state.phase = match outcome {
            PluginOutcome::Preview(preview) => InstallPhase::PluginPreview(preview),
            PluginOutcome::UpToDate { name, sha } => InstallPhase::UpToDate { name, sha },
            PluginOutcome::Failed { tap, error } => {
                state.tap = tap;
                InstallPhase::Failed(error)
            }
        };
    }

    /// The driver answered a browse or a search.
    pub fn plugin_listings(&mut self, title: &str, listings: Vec<PluginListing>, error: Option<String>) {
        if let Some(Overlay::PluginBrowse {
            title: current,
            listings: shown,
            selected,
            loading,
            error: shown_error,
            ..
        }) = &mut self.overlay
            && *current == title
        {
            *loading = false;
            *selected = 0;
            *shown_error = if listings.is_empty() && error.is_none() {
                Some(format!("Nothing found for {title}."))
            } else {
                error
            };
            *shown = listings;
        }
    }

    /// Keys while a marketplace or search listing is open.
    pub(crate) fn plugin_browse_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let Some(Overlay::PluginBrowse { title, listings, filter, selected, loading, error }) = &mut self.overlay
        else {
            return Vec::new();
        };
        let shown = filtered_listings(listings, filter);
        match key.code {
            KeyCode::Esc => self.overlay = None,
            KeyCode::Up => *selected = selected.saturating_sub(1),
            KeyCode::Down => *selected = (*selected + 1).min(shown.len().saturating_sub(1)),
            KeyCode::PageUp => *selected = selected.saturating_sub(8),
            KeyCode::PageDown => *selected = (*selected + 8).min(shown.len().saturating_sub(1)),
            KeyCode::Backspace => {
                filter.pop();
                *selected = 0;
            }
            KeyCode::Char(c) if !ctrl => {
                filter.push(c);
                *selected = 0;
            }
            KeyCode::Enter => {
                if let Some(listing) = shown.get(*selected) {
                    match &listing.unsupported {
                        Some(why) => *error = Some(format!("{} can't be installed: {why}.", listing.name)),
                        None => {
                            let tap = listing.tap.clone();
                            return self.start_plugin_install(tap);
                        }
                    }
                } else if title.starts_with("search") && !filter.trim().is_empty() && !*loading {
                    // A search window with no results yet: Enter searches what was typed.
                    let query = filter.trim().to_owned();
                    *title = format!("search: {query}");
                    *loading = true;
                    *error = None;
                    filter.clear();
                    return vec![Action::Extension(ExtensionCommand::SearchPlugins(query))];
                }
            }
            _ => {}
        }
        Vec::new()
    }

    /// `u` on the Skills tab or `/skill update <name>`: fetch the newest version of an installed skill.
    pub fn update_skill(&mut self, name: &str) -> Vec<Action> {
        if self.demo {
            self.toast("That isn't available in the demo recording.", ToastLevel::Info);
            return Vec::new();
        }
        let name = name.trim().to_owned();
        if name.is_empty() {
            self.toast("Name the skill to update: /skill update <name>.", ToastLevel::Info);
            return Vec::new();
        }
        self.overlay = Some(Overlay::Install(InstallState {
            tap: format!("update {name}"),
            kind: InstallKind::Skill,
            phase: InstallPhase::Fetching,
        }));
        vec![Action::Extension(ExtensionCommand::UpdateSkill(name))]
    }

    /// `/skill search [words]` or `s` on the Skills tab.
    pub fn open_skill_search(&mut self, query: Option<String>) -> Vec<Action> {
        if self.demo {
            self.toast("That isn't available in the demo recording.", ToastLevel::Info);
            return Vec::new();
        }
        let query = query.map(|q| q.trim().to_owned()).unwrap_or_default();
        let searching = !query.is_empty();
        self.overlay = Some(Overlay::SkillSearch {
            query: query.clone(),
            hits: Vec::new(),
            selected: 0,
            searching,
            error: None,
            note: None,
        });
        if searching { vec![Action::Extension(ExtensionCommand::SearchSkills(query))] } else { Vec::new() }
    }

    /// The driver finished staging. When the install window was closed meanwhile, a staged skill is
    /// discarded rather than left behind.
    pub fn install_outcome(&mut self, outcome: InstallOutcome) {
        let waiting =
            matches!(&self.overlay, Some(Overlay::Install(InstallState { phase: InstallPhase::Fetching, .. })));
        if !waiting {
            if let InstallOutcome::Preview(preview) = outcome {
                self.pending_actions.push(Action::Extension(ExtensionCommand::DiscardInstall(preview.staging)));
            }
            return;
        }
        let Some(Overlay::Install(state)) = &mut self.overlay else { return };
        state.phase = match outcome {
            InstallOutcome::Preview(preview) => InstallPhase::Preview { preview, reading: None },
            InstallOutcome::Choose { tap, candidates, note } => {
                state.tap = tap;
                InstallPhase::Choose { candidates, note, selected: 0 }
            }
            InstallOutcome::UpToDate { name, sha } => InstallPhase::UpToDate { name, sha },
            InstallOutcome::Failed { tap, error } => {
                state.tap = tap;
                InstallPhase::Failed(error)
            }
        };
    }

    /// The driver answered a hub search.
    pub fn skill_search_result(
        &mut self,
        query: &str,
        hits: Vec<SkillSearchHit>,
        note: Option<String>,
        error: Option<String>,
    ) {
        if let Some(Overlay::SkillSearch {
            query: current,
            hits: shown,
            selected,
            searching,
            error: shown_error,
            note: shown_note,
        }) = &mut self.overlay
            && current.trim() == query
        {
            *searching = false;
            *selected = 0;
            *shown_error = if hits.is_empty() && error.is_none() {
                Some(format!("Nothing in the Hermes index matches \"{query}\"."))
            } else {
                error
            };
            *shown_note = note;
            *shown = hits;
        }
    }

    /// Keys while the install window is open.
    pub(crate) fn install_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let Some(Overlay::Install(state)) = &mut self.overlay else { return Vec::new() };
        match &mut state.phase {
            InstallPhase::Ask => match key.code {
                KeyCode::Esc => self.overlay = None,
                KeyCode::Backspace => {
                    state.tap.pop();
                }
                KeyCode::Char(c) if !ctrl => state.tap.push(c),
                KeyCode::Enter => {
                    let tap = state.tap.trim().to_owned();
                    if !tap.is_empty() {
                        return match state.kind {
                            InstallKind::Skill => self.start_install(tap),
                            InstallKind::Plugin => self.start_plugin_install(tap),
                            InstallKind::Marketplace => {
                                self.overlay = None;
                                vec![Action::Extension(ExtensionCommand::AddMarketplace(tap))]
                            }
                        };
                    }
                }
                _ => {}
            },
            InstallPhase::PluginPreview(preview) => {
                let staging = preview.staging.clone();
                let dangerous = preview.plugin.parts.iter().any(
                    |part| matches!(part, recode_core::extensions::PluginPart::Skill { scan, .. } if scan.blocks()),
                );
                match key.code {
                    KeyCode::Char('y') | KeyCode::Enter if !dangerous => {
                        self.overlay = None;
                        return vec![Action::Extension(ExtensionCommand::CommitPlugin { staging, enable: true })];
                    }
                    KeyCode::Char('y') | KeyCode::Enter => {
                        self.toast(
                            "A skill in it was flagged as dangerous, so the plugin can only be kept off.",
                            ToastLevel::Warning,
                        );
                    }
                    KeyCode::Char('k') => {
                        self.overlay = None;
                        return vec![Action::Extension(ExtensionCommand::CommitPlugin { staging, enable: false })];
                    }
                    KeyCode::Char('n') | KeyCode::Esc => {
                        self.overlay = None;
                        return vec![Action::Extension(ExtensionCommand::DiscardInstall(staging))];
                    }
                    _ => {}
                }
            }
            InstallPhase::Fetching => {
                if key.code == KeyCode::Esc {
                    self.overlay = None;
                }
            }
            InstallPhase::Preview { preview, reading } => {
                let staging = preview.staging.clone();
                let dangerous = preview.scan.blocks();
                let lines = preview.skill_text.lines().count();
                match key.code {
                    KeyCode::Esc if reading.is_some() => *reading = None,
                    KeyCode::Char('r') => *reading = if reading.is_some() { None } else { Some(0) },
                    KeyCode::Up if reading.is_some() => *reading = reading.map(|o| o.saturating_sub(1)),
                    KeyCode::Down if reading.is_some() => {
                        *reading = reading.map(|o| (o + 1).min(lines.saturating_sub(1)))
                    }
                    KeyCode::PageUp if reading.is_some() => *reading = reading.map(|o| o.saturating_sub(10)),
                    KeyCode::PageDown if reading.is_some() => {
                        *reading = reading.map(|o| (o + 10).min(lines.saturating_sub(1)));
                    }
                    KeyCode::Char('y') | KeyCode::Enter if !dangerous => {
                        self.overlay = None;
                        return vec![Action::Extension(ExtensionCommand::CommitInstall { staging, enable: true })];
                    }
                    KeyCode::Char('y') | KeyCode::Enter => {
                        self.toast(
                            "The scan found something dangerous, so it can only be kept off.",
                            ToastLevel::Warning,
                        );
                    }
                    KeyCode::Char('k') => {
                        self.overlay = None;
                        return vec![Action::Extension(ExtensionCommand::CommitInstall { staging, enable: false })];
                    }
                    KeyCode::Char('n') | KeyCode::Esc => {
                        self.overlay = None;
                        return vec![Action::Extension(ExtensionCommand::DiscardInstall(staging))];
                    }
                    _ => {}
                }
            }
            InstallPhase::Choose { candidates, selected, .. } => match key.code {
                KeyCode::Esc => self.overlay = None,
                KeyCode::Up => *selected = selected.saturating_sub(1),
                KeyCode::Down => *selected = (*selected + 1).min(candidates.len().saturating_sub(1)),
                KeyCode::Enter => {
                    if let Some(candidate) = candidates.get(*selected) {
                        let tap = candidate.tap.clone();
                        return self.start_install(tap);
                    }
                }
                _ => {}
            },
            InstallPhase::UpToDate { .. } | InstallPhase::Failed(_) => match key.code {
                KeyCode::Esc | KeyCode::Enter => self.overlay = None,
                KeyCode::Char('i') => {
                    state.tap.clear();
                    state.phase = InstallPhase::Ask;
                }
                _ => {}
            },
        }
        Vec::new()
    }

    /// Keys while the hub search is open.
    pub(crate) fn skill_search_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let Some(Overlay::SkillSearch { query, hits, selected, searching, error, .. }) = &mut self.overlay else {
            return Vec::new();
        };
        match key.code {
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
                    let tap = hit.tap.clone();
                    return self.start_install(tap);
                } else if !query.trim().is_empty() && !*searching {
                    *searching = true;
                    *error = None;
                    return vec![Action::Extension(ExtensionCommand::SearchSkills(query.trim().to_owned()))];
                }
            }
            _ => {}
        }
        Vec::new()
    }
}

const ASK_HELP: &str = "official/security/godmode · a name from the Hermes index · owner/repo/folder[@ref] · a GitHub, skills.sh or SKILL.md link · a folder on this computer. Nothing fetched is run: it's read, scanned and shown first.";
const ASK_PLUGIN_HELP: &str = "A name from the Hermes catalog (better-session-appearance) · name@marketplace · owner/repo[/folder][@ref] · a GitHub link · a folder on this computer. Its skills and MCP servers will work here; Python code, desktop parts and hooks are listed, never run.";
const ASK_MARKETPLACE_HELP: &str = "A Claude Code marketplace: owner/repo[@ref] (anthropics/claude-plugins-official), a GitHub link, or a folder holding .claude-plugin/marketplace.json. Its manifest is read at a pinned commit; nothing is installed yet.";

/// Draws the install window in whatever phase it's in.
pub fn draw_install(frame: &mut Frame<'_>, area: Rect, state: &InstallState) {
    match &state.phase {
        InstallPhase::Ask => {
            let rect = overlay_area(area, 80, 10);
            let inner = usize::from(rect.width).saturating_sub(4);
            let (prompt, help) = match state.kind {
                InstallKind::Skill => ("Install  ", ASK_HELP),
                InstallKind::Plugin => ("Install  ", ASK_PLUGIN_HELP),
                InstallKind::Marketplace => ("Follow  ", ASK_MARKETPLACE_HELP),
            };
            let mut lines = vec![Line::from(vec![
                Span::styled(prompt, theme::muted()),
                Span::raw(state.tap.clone()),
                Span::styled("▏", theme::accent()),
            ])];
            lines.push(Line::default());
            lines.extend(wrap_plain(help, theme::muted(), inner));
            lines.push(Line::default());
            let action = if state.kind == InstallKind::Marketplace { "read it" } else { "fetch" };
            lines.push(legend(&[("enter", action), ("esc", "cancel")], inner));
            boxed(frame, rect, title_of(state.kind), lines);
        }
        InstallPhase::Fetching => {
            let rect = overlay_area(area, 70, 6);
            let inner = usize::from(rect.width).saturating_sub(4);
            let mut lines = wrap_plain(&format!("Fetching {}…", state.tap), Style::new(), inner);
            let bare = !state.tap.contains('/') || state.tap.starts_with("hermes/");
            if bare && !state.tap.starts_with("update ") && state.kind == InstallKind::Skill {
                lines.extend(wrap_plain(
                    "The Hermes index is about 42 MB; it's fetched once a day and kept.",
                    theme::muted(),
                    inner,
                ));
            }
            lines.push(Line::default());
            lines.push(legend(&[("esc", "cancel")], inner));
            boxed(frame, rect, title_of(state.kind), lines);
        }
        InstallPhase::PluginPreview(preview) => draw_plugin_preview(frame, area, preview),
        InstallPhase::Preview { preview, reading: Some(offset) } => draw_reading(frame, area, preview, *offset),
        InstallPhase::Preview { preview, reading: None } => draw_preview(frame, area, preview),
        InstallPhase::Choose { candidates, note, selected } => {
            draw_choose(frame, area, &state.tap, candidates, note.as_deref(), *selected);
        }
        InstallPhase::UpToDate { name, sha } => {
            let rect = overlay_area(area, 70, 6);
            let inner = usize::from(rect.width).saturating_sub(4);
            let short = sha.get(..8).unwrap_or(sha);
            let mut lines = wrap_plain(
                &format!("{name} is already at the newest commit ({short})."),
                Style::new().fg(theme::color(Token::Success)),
                inner,
            );
            lines.push(Line::default());
            lines.push(legend(&[("esc", "close")], inner));
            boxed(frame, rect, &format!("Update {name}"), lines);
        }
        InstallPhase::Failed(error) => {
            let rect = overlay_area(area, 76, 9);
            let inner = usize::from(rect.width).saturating_sub(4);
            let mut lines = wrap_plain(&state.tap, theme::muted(), inner);
            lines.extend(wrap_plain(error, Style::new().fg(theme::color(Token::Danger)), inner));
            lines.push(Line::default());
            lines.push(legend(&[("i", "try another"), ("esc", "close")], inner));
            boxed(frame, rect, title_of(state.kind), lines);
        }
    }
}

fn title_of(kind: InstallKind) -> &'static str {
    match kind {
        InstallKind::Skill => "Install a skill",
        InstallKind::Plugin => "Install a plugin",
        InstallKind::Marketplace => "Follow a marketplace",
    }
}

/// Hermes' plugin summary: `name [tier] pinned @ sha · maintained by …`, the capability sentence, then
/// what runs here and what doesn't, then the question.
fn draw_plugin_preview(frame: &mut Frame<'_>, area: Rect, preview: &PluginPreview) {
    let plugin = &preview.plugin;
    let rect = overlay_area(area, 84, 20);
    let inner = usize::from(rect.width).saturating_sub(4);
    let dangerous = plugin
        .parts
        .iter()
        .any(|part| matches!(part, recode_core::extensions::PluginPart::Skill { scan, .. } if scan.blocks()));
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut head = vec![Span::styled(plugin.name.clone(), Style::new().add_modifier(Modifier::BOLD)), Span::raw("  ")];
    head.push(chip(plugin.label.clone(), label_color(&plugin.label)));
    if plugin.source.sha.len() >= 8 {
        head.push(Span::styled(format!("  pinned @ {}", &plugin.source.sha[..8]), theme::muted()));
    }
    lines.push(Line::from(head));
    if let Some(maintainer) = &plugin.maintainer {
        lines.extend(wrap_plain(&format!("maintained by {maintainer}"), theme::muted(), inner));
    }
    if !plugin.description.is_empty() {
        lines.extend(wrap_plain(&plugin.description, Style::new(), inner));
    }
    let files = if preview.files == 1 { "1 file".to_owned() } else { format!("{} files", preview.files) };
    let version = plugin.version.as_deref().map(|v| format!(" · version {v}")).unwrap_or_default();
    lines.extend(wrap_plain(
        &format!("{} · {files} · {}{version}", plugin.capability_sentence(), human_size(preview.bytes)),
        theme::muted(),
        inner,
    ));
    for part in plugin.parts.iter().take(8) {
        let runs = part.runs_here();
        let style = if runs { Style::new() } else { theme::muted() };
        let dangerous_part = matches!(part, recode_core::extensions::PluginPart::Skill { scan, .. } if scan.blocks());
        let style = if dangerous_part { Style::new().fg(theme::color(Token::Danger)) } else { style };
        lines.extend(wrap_plain(&format!("{}{}", if runs { "• " } else { "· " }, part.describe()), style, inner));
    }
    if plugin.parts.len() > 8 {
        lines.push(Line::styled(format!("  …and {} more", plugin.parts.len() - 8), theme::muted()));
    }
    if let Some(replacing) = &preview.replaces {
        let from = replacing.sha.get(..8).unwrap_or(&replacing.sha);
        let text = if replacing.changed.is_empty() {
            format!("Update from {from}: the files are the same.")
        } else {
            format!("Update from {from}: {}", replacing.changed.join(", "))
        };
        lines.extend(wrap_plain(&text, Style::new().fg(theme::color(Token::Attention)), inner));
    }
    if let Some(note) = &preview.note {
        lines.extend(wrap_plain(note, theme::muted(), inner));
    }
    lines.push(Line::from(vec![
        Span::styled("Location  ", theme::muted()),
        Span::raw(truncate(&preview.location, inner.saturating_sub(10))),
    ]));
    lines.push(Line::default());
    let question = if dangerous { "Keep it, off?" } else { "Enable now?" };
    lines.push(Line::styled(question, Style::new().add_modifier(Modifier::BOLD)));
    let keys: &[(&str, &str)] = if dangerous {
        &[("k", "keep off"), ("n", "discard")]
    } else {
        &[("y", "yes"), ("k", "keep off"), ("n", "discard")]
    };
    lines.push(legend(keys, inner));
    let title =
        if preview.replaces.is_some() { format!("Update {}", plugin.name) } else { "Install a plugin".to_owned() };
    let wanted = u16::try_from(lines.len() + 2).unwrap_or(20).min(30);
    let rect = overlay_area(area, 84, wanted.max(8));
    boxed(frame, rect, &title, lines);
}

/// Listings whose name or description contain every word of the filter.
fn filtered_listings<'a>(listings: &'a [PluginListing], filter: &str) -> Vec<&'a PluginListing> {
    let words: Vec<String> = filter.split_whitespace().map(str::to_lowercase).collect();
    listings
        .iter()
        .filter(|listing| {
            let hay = format!("{} {} {}", listing.name, listing.description, listing.label).to_lowercase();
            words.iter().all(|word| hay.contains(word.as_str()))
        })
        .collect()
}

/// A marketplace's plugins (or a search's): a filter line, then rows with label and version.
#[allow(clippy::too_many_arguments)]
pub fn draw_plugin_browse(
    frame: &mut Frame<'_>,
    area: Rect,
    title: &str,
    listings: &[PluginListing],
    filter: &str,
    selected: usize,
    loading: bool,
    error: Option<&str>,
) {
    let shown = filtered_listings(listings, filter);
    let rows = u16::try_from(shown.len().min(9) * 2).unwrap_or(18);
    let rect = overlay_area(area, 96, rows + 8);
    let inner = usize::from(rect.width).saturating_sub(4);
    let heading = if title == "hermes" {
        "Hermes plugin catalog".to_owned()
    } else if let Some(query) = title.strip_prefix("search: ") {
        format!("Plugins matching \"{query}\"")
    } else if title == "search" {
        "Search plugins".to_owned()
    } else {
        format!("Marketplace {title}")
    };
    let mut lines = vec![Line::from(vec![
        Span::styled(
            if title.starts_with("search") && listings.is_empty() { "Search  " } else { "Filter  " },
            theme::muted(),
        ),
        Span::raw(filter.to_owned()),
        Span::styled("▏", theme::accent()),
        Span::styled(
            if listings.is_empty() { String::new() } else { format!("   {} of {}", shown.len(), listings.len()) },
            theme::muted(),
        ),
    ])];
    lines.push(Line::default());
    if loading {
        lines.push(Line::styled("Fetching the listing…", theme::muted()));
    } else if let Some(error) = error {
        lines.extend(wrap_plain(error, Style::new().fg(theme::color(Token::Danger)), inner));
    } else if listings.is_empty() {
        lines.push(Line::styled(
            "Type words and press Enter. Results come from the Hermes catalog and the marketplaces you follow.",
            theme::muted(),
        ));
    }
    let first = selected.saturating_sub(8);
    for (index, listing) in shown.iter().enumerate().skip(first).take(9) {
        let base = if index == selected {
            Style::new().bg(theme::color(Token::UserBg)).add_modifier(Modifier::BOLD)
        } else {
            Style::new()
        };
        let version = listing.version.as_deref().map(|v| format!("  {v}")).unwrap_or_default();
        lines.push(Line::from(vec![
            Span::styled(if index == selected { "❯ " } else { "  " }, theme::accent().patch(base)),
            Span::styled(truncate(&format!("{}{version}", listing.name), inner.saturating_sub(16)), base),
            Span::raw("  "),
            chip(listing.label.clone(), label_color(&listing.label)),
        ]));
        let detail = match &listing.unsupported {
            Some(why) => format!("can't be installed: {why}"),
            None => listing.description.clone(),
        };
        let style =
            if listing.unsupported.is_some() { Style::new().fg(theme::color(Token::Danger)) } else { theme::muted() };
        lines.push(Line::styled(format!("    {}", truncate(&detail, inner.saturating_sub(4))), style));
    }
    lines.push(Line::default());
    let keys: &[(&str, &str)] = if shown.is_empty() {
        &[("enter", "search"), ("esc", "cancel")]
    } else {
        &[("enter", "install"), ("↑↓", "choose"), ("type", "to filter"), ("esc", "cancel")]
    };
    lines.push(legend(keys, inner));
    boxed(frame, rect, &heading, lines);
}

/// The Hermes-style summary: `godmode  [official] pinned @ 4134cbef`, what it declares, the scan,
/// where it goes, and the question.
fn draw_preview(frame: &mut Frame<'_>, area: Rect, preview: &InstallPreview) {
    let rect = overlay_area(area, 84, 20);
    let inner = usize::from(rect.width).saturating_sub(4);
    let dangerous = preview.scan.blocks();
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut head = vec![Span::styled(preview.name.clone(), Style::new().add_modifier(Modifier::BOLD))];
    head.push(Span::raw("  "));
    head.push(chip(preview.source.label.clone(), label_color(&preview.source.label)));
    if preview.source.sha.len() >= 8 {
        head.push(Span::styled(format!("  pinned @ {}", &preview.source.sha[..8]), theme::muted()));
    }
    lines.push(Line::from(head));
    if !preview.description.is_empty() {
        lines.extend(wrap_plain(&preview.description, Style::new(), inner));
    }
    let count = preview.files.len();
    let files = if count == 1 { "1 file".to_owned() } else { format!("{count} files") };
    let facts = format!(
        "{} · {files} · {} · ~{} tokens when loaded",
        preview.declares.summary(),
        human_size(preview.bytes),
        compact_count(preview.estimated_tokens)
    );
    lines.extend(wrap_plain(&facts, theme::muted(), inner));
    if let Some(problem) = &preview.problem {
        lines.extend(wrap_plain(
            &format!("It will stay off: {problem}"),
            Style::new().fg(theme::color(Token::Danger)),
            inner,
        ));
    }
    let findings = preview.scan.findings();
    if findings.is_empty() {
        lines.push(Line::styled("scan: clean", Style::new().fg(theme::color(Token::Success))));
    } else {
        let level = if dangerous { "dangerous (it can't be enabled)" } else { "caution" };
        for (index, finding) in findings.iter().take(4).enumerate() {
            let place =
                if finding.line > 0 { format!("{}:{}", finding.file, finding.line) } else { finding.file.clone() };
            let excerpt = if finding.excerpt.is_empty() { String::new() } else { format!(" ({})", finding.excerpt) };
            let text = if index == 0 {
                format!("scan: {level} — {place} {}{excerpt}", finding.reason)
            } else {
                format!("      {place} {}{excerpt}", finding.reason)
            };
            lines.extend(wrap_plain(&text, Style::new().fg(theme::color(Token::Danger)), inner));
        }
        if findings.len() > 4 {
            lines.push(Line::styled(format!("      …and {} more", findings.len() - 4), theme::muted()));
        }
    }
    if let Some(replacing) = &preview.replaces {
        let from = replacing.sha.get(..8).unwrap_or(&replacing.sha);
        let text = if replacing.changed.is_empty() {
            format!("Update from {from}: the files are the same.")
        } else {
            format!("Update from {from}: {}", replacing.changed.join(", "))
        };
        lines.extend(wrap_plain(&text, Style::new().fg(theme::color(Token::Attention)), inner));
    }
    if let Some(note) = &preview.note {
        lines.extend(wrap_plain(note, theme::muted(), inner));
    }
    lines.push(Line::from(vec![
        Span::styled("Location  ", theme::muted()),
        Span::raw(truncate(&preview.location, inner.saturating_sub(10))),
    ]));
    lines.push(Line::default());
    let question = if dangerous { "Keep it, off?" } else { "Enable now?" };
    lines.push(Line::styled(question, Style::new().add_modifier(Modifier::BOLD)));
    let keys: &[(&str, &str)] = if dangerous {
        &[("k", "keep off"), ("n", "discard"), ("r", "read SKILL.md")]
    } else {
        &[("y", "yes"), ("k", "keep off"), ("n", "discard"), ("r", "read SKILL.md")]
    };
    lines.push(legend(keys, inner));
    let title =
        if preview.replaces.is_some() { format!("Update {}", preview.name) } else { "Install a skill".to_owned() };
    let wanted = u16::try_from(lines.len() + 2).unwrap_or(20).min(28);
    let rect = overlay_area(area, 84, wanted.max(8));
    boxed(frame, rect, &title, lines);
}

/// `SKILL.md` as fetched, scrolling.
fn draw_reading(frame: &mut Frame<'_>, area: Rect, preview: &InstallPreview, offset: usize) {
    let rect = overlay_area(area, 100, area.height.saturating_sub(2));
    let inner = usize::from(rect.width).saturating_sub(4);
    let body = usize::from(rect.height).saturating_sub(3);
    let all: Vec<Line<'static>> = preview
        .skill_text
        .lines()
        .flat_map(|line| if line.is_empty() { vec![Line::default()] } else { wrap_plain(line, Style::new(), inner) })
        .collect();
    let offset = offset.min(all.len().saturating_sub(body));
    let mut lines: Vec<Line<'static>> = all.into_iter().skip(offset).take(body).collect();
    lines.push(legend(&[("↑↓", "scroll"), ("r", "back to the summary"), ("esc", "back")], inner));
    boxed(frame, rect, &format!("SKILL.md · {}", preview.name), lines);
}

fn draw_choose(
    frame: &mut Frame<'_>,
    area: Rect,
    tap: &str,
    candidates: &[SkillCandidate],
    note: Option<&str>,
    selected: usize,
) {
    let rows = u16::try_from(candidates.len().min(8) * 2).unwrap_or(16);
    let rect = overlay_area(area, 90, rows + 7);
    let inner = usize::from(rect.width).saturating_sub(4);
    let mut lines = wrap_plain(&format!("{tap} holds {} skills. Which one?", candidates.len()), Style::new(), inner);
    if let Some(note) = note {
        lines.extend(wrap_plain(note, theme::muted(), inner));
    }
    lines.push(Line::default());
    let first = selected.saturating_sub(7);
    for (index, candidate) in candidates.iter().enumerate().skip(first).take(8) {
        let base = if index == selected {
            Style::new().bg(theme::color(Token::UserBg)).add_modifier(Modifier::BOLD)
        } else {
            Style::new()
        };
        lines.push(Line::from(vec![
            Span::styled(if index == selected { "❯ " } else { "  " }, theme::accent().patch(base)),
            Span::styled(truncate(&candidate.name, inner.saturating_sub(2)), base),
        ]));
        let detail = if candidate.description.is_empty() {
            candidate.path.clone()
        } else {
            format!("{}  ·  {}", candidate.description, candidate.path)
        };
        lines.push(Line::styled(format!("    {}", truncate(&detail, inner.saturating_sub(4))), theme::muted()));
    }
    lines.push(Line::default());
    lines.push(legend(&[("enter", "fetch this one"), ("↑↓", "choose"), ("esc", "cancel")], inner));
    boxed(frame, rect, "Install a skill", lines);
}

/// What the search window shows.
pub struct SearchView<'a> {
    pub query: &'a str,
    pub hits: &'a [SkillSearchHit],
    pub selected: usize,
    pub searching: bool,
    pub error: Option<&'a str>,
    pub note: Option<&'a str>,
}

/// The hub search: a query line, then hits with their label and where they live.
pub fn draw_skill_search(frame: &mut Frame<'_>, area: Rect, view: &SearchView<'_>) {
    let SearchView { query, hits, selected, searching, error, note } = *view;
    let rows = u16::try_from(hits.len().min(8) * 2).unwrap_or(16);
    let rect = overlay_area(area, 90, rows + 8);
    let inner = usize::from(rect.width).saturating_sub(4);
    let mut lines = vec![Line::from(vec![
        Span::styled("Search  ", theme::muted()),
        Span::raw(query.to_owned()),
        Span::styled("▏", theme::accent()),
    ])];
    lines.push(Line::default());
    if searching {
        lines.push(Line::styled("Searching the Hermes index…", theme::muted()));
    } else if let Some(error) = error {
        lines.extend(wrap_plain(error, Style::new().fg(theme::color(Token::Danger)), inner));
    } else if hits.is_empty() {
        lines.push(Line::styled(
            "Type part of a skill's name and press Enter. Results come from the Hermes Agent skills index (100k skills, fetched daily).",
            theme::muted(),
        ));
    }
    if let Some(note) = note {
        lines.extend(wrap_plain(note, theme::muted(), inner));
    }
    let first = selected.saturating_sub(7);
    for (index, hit) in hits.iter().enumerate().skip(first).take(8) {
        let base = if index == selected {
            Style::new().bg(theme::color(Token::UserBg)).add_modifier(Modifier::BOLD)
        } else {
            Style::new()
        };
        lines.push(Line::from(vec![
            Span::styled(if index == selected { "❯ " } else { "  " }, theme::accent().patch(base)),
            Span::styled(truncate(&hit.name, inner.saturating_sub(14)), base),
            Span::raw("  "),
            chip(hit.label.clone(), label_color(&hit.label)),
        ]));
        let place = if hit.path.is_empty() { hit.repo.clone() } else { format!("{}/{}", hit.repo, hit.path) };
        let detail = if hit.description.is_empty() { place } else { format!("{}  ·  {place}", hit.description) };
        lines.push(Line::styled(format!("    {}", truncate(&detail, inner.saturating_sub(4))), theme::muted()));
    }
    lines.push(Line::default());
    let keys: &[(&str, &str)] = if hits.is_empty() {
        &[("enter", "search"), ("esc", "cancel")]
    } else {
        &[("enter", "install"), ("↑↓", "choose"), ("type", "to search again"), ("esc", "cancel")]
    };
    lines.push(legend(keys, inner));
    boxed(frame, rect, "Search skill hubs", lines);
}

/// A colour per label: official and builtin stand out, the rest are neutral.
fn label_color(label: &str) -> ratatui::style::Color {
    match label {
        "official" | "builtin" => theme::color(Token::Success),
        "local" | "file" => theme::color(Token::Muted),
        _ => theme::color(Token::Accent),
    }
}

/// "1.2 MB" and the like.
fn human_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{} kB", bytes.div_ceil(1024))
    } else {
        format!("{bytes} bytes")
    }
}

/// "3.4k" for a count.
fn compact_count(count: u64) -> String {
    if count >= 10_000 {
        format!("{}k", count / 1000)
    } else if count >= 1000 {
        format!("{}.{}k", count / 1000, count % 1000 / 100)
    } else {
        count.to_string()
    }
}
