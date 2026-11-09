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
use std::collections::BTreeSet;

/// What the install window installs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InstallKind {
    #[default]
    Skill,
    Plugin,
    /// Following a Claude Code marketplace (the result is a home message, not a preview).
    Marketplace,
}

/// One row of a batch install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchItem {
    pub tap: String,
    pub outcome: BatchOutcome,
    pub checked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchOutcome {
    Fetching,
    Skill(Box<InstallPreview>),
    Plugin(Box<PluginPreview>),
    UpToDate(String),
    Failed(String),
}

impl BatchItem {
    fn installable(&self) -> bool {
        matches!(self.outcome, BatchOutcome::Skill(_) | BatchOutcome::Plugin(_))
    }

    fn dangerous(&self) -> bool {
        match &self.outcome {
            BatchOutcome::Skill(preview) => preview.scan.blocks(),
            BatchOutcome::Plugin(preview) => {
                preview.plugin.parts.iter().any(
                    |part| matches!(part, recode_core::extensions::PluginPart::Skill { scan, .. } if scan.blocks()),
                )
            }
            _ => false,
        }
    }

    fn staging(&self) -> Option<String> {
        match &self.outcome {
            BatchOutcome::Skill(preview) => Some(preview.staging.clone()),
            BatchOutcome::Plugin(preview) => Some(preview.staging.clone()),
            _ => None,
        }
    }
}

/// Whether pasted text is a skill file (front matter with a name and description) rather than a link or a name.
#[must_use]
pub fn looks_like_skill_text(text: &str) -> bool {
    let trimmed = text.trim_start();
    trimmed.starts_with("---") && trimmed.contains("\nname:") && trimmed.contains("description:")
}

/// Splits pasted text into things to install: one per line, and links separated by spaces.
#[must_use]
pub fn taps_in(text: &str) -> Vec<String> {
    let mut taps = Vec::new();
    for line in text.lines() {
        let line = line.trim().trim_start_matches(['-', '*', '•']).trim();
        if line.is_empty() {
            continue;
        }
        if line.contains("://") && line.contains(char::is_whitespace) {
            taps.extend(line.split_whitespace().filter(|w| w.contains("://")).map(str::to_owned));
        } else {
            taps.push(line.to_owned());
        }
    }
    taps.dedup();
    taps
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
        /// Ticked candidates; Enter with any ticked stages them all.
        checked: Vec<bool>,
    },
    /// Several things staged at once: a checklist to install from.
    Batch {
        items: Vec<BatchItem>,
        selected: usize,
    },
    /// A dangerous verdict's findings, read, and the question whether to enable it anyway.
    AcceptDangerous(Box<InstallPreview>),
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
            ticked: BTreeSet::new(),
            category: None,
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
            ticked: BTreeSet::new(),
            category: None,
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
        let Some(Overlay::PluginBrowse { title, listings, filter, selected, loading, error, ticked, category }) =
            &mut self.overlay
        else {
            return Vec::new();
        };
        let shown = filtered_listings(listings, filter, category.as_deref());
        match key.code {
            KeyCode::Esc => self.overlay = None,
            KeyCode::Up => *selected = selected.saturating_sub(1),
            KeyCode::Down => *selected = (*selected + 1).min(shown.len().saturating_sub(1)),
            KeyCode::PageUp => *selected = selected.saturating_sub(8),
            KeyCode::PageDown => *selected = (*selected + 8).min(shown.len().saturating_sub(1)),
            KeyCode::Left | KeyCode::Right => {
                let names: Vec<String> = categories(listings).into_iter().map(|(name, _)| name).collect();
                if !names.is_empty() {
                    // Position 0 is "all"; the rest are the categories in order of size.
                    let at = category.as_deref().and_then(|c| names.iter().position(|n| n == c)).map_or(0, |i| i + 1);
                    let count = names.len() + 1;
                    let next = if key.code == KeyCode::Right { (at + 1) % count } else { (at + count - 1) % count };
                    *category = if next == 0 { None } else { Some(names[next - 1].clone()) };
                    *selected = 0;
                }
            }
            KeyCode::Tab => {
                if let Some(listing) = shown.get(*selected)
                    && listing.unsupported.is_none()
                    && !ticked.remove(&listing.tap)
                {
                    ticked.insert(listing.tap.clone());
                }
            }
            KeyCode::Char('a') if ctrl => {
                let installable: Vec<&str> =
                    shown.iter().filter(|l| l.unsupported.is_none()).map(|l| l.tap.as_str()).collect();
                if installable.iter().all(|tap| ticked.contains(*tap)) {
                    for tap in installable {
                        ticked.remove(tap);
                    }
                } else {
                    ticked.extend(installable.into_iter().map(str::to_owned));
                }
            }
            KeyCode::Backspace => {
                filter.pop();
                *selected = 0;
            }
            KeyCode::Char(c) if !ctrl => {
                filter.push(c);
                *selected = 0;
            }
            KeyCode::Enter if !ticked.is_empty() => {
                // Keep the listing order rather than the set's.
                let taps: Vec<String> =
                    listings.iter().filter(|l| ticked.contains(&l.tap)).map(|l| l.tap.clone()).collect();
                return self.start_batch(taps, InstallKind::Plugin);
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

    /// Stages several taps at once and shows the checklist as they land.
    fn start_batch(&mut self, mut taps: Vec<String>, kind: InstallKind) -> Vec<Action> {
        if taps.len() > MAX_BATCH {
            self.toast(
                format!(
                    "Taking the first {MAX_BATCH} of {}: each one is a GitHub listing, and GitHub allows 60 an hour without a GITHUB_TOKEN.",
                    taps.len()
                ),
                ToastLevel::Info,
            );
            taps.truncate(MAX_BATCH);
        }
        let items = taps
            .iter()
            .map(|tap| BatchItem { tap: tap.clone(), outcome: BatchOutcome::Fetching, checked: true })
            .collect();
        self.overlay = Some(Overlay::Install(InstallState {
            tap: format!("{} items", taps.len()),
            kind,
            phase: InstallPhase::Batch { items, selected: 0 },
        }));
        match kind {
            InstallKind::Plugin => vec![Action::Extension(ExtensionCommand::StagePluginBatch(taps))],
            _ => vec![Action::Extension(ExtensionCommand::StageSkillBatch(taps))],
        }
    }

    /// Pasted text while the install prompt is open: a skill file is staged as one, several lines or
    /// links become a batch, and one thing is typed into the prompt.
    pub(crate) fn paste_into_install(&mut self, text: &str) -> Vec<Action> {
        let Some(Overlay::Install(state)) = &mut self.overlay else { return Vec::new() };
        if state.phase != InstallPhase::Ask {
            return Vec::new();
        }
        let kind = state.kind;
        if kind == InstallKind::Skill && looks_like_skill_text(text) {
            self.overlay = Some(Overlay::Install(InstallState {
                tap: "pasted SKILL.md".to_owned(),
                kind,
                phase: InstallPhase::Fetching,
            }));
            return vec![Action::Extension(ExtensionCommand::StageSkillText(text.to_owned()))];
        }
        let taps = taps_in(text);
        if taps.len() > 1 && kind != InstallKind::Marketplace {
            return self.start_batch(taps, kind);
        }
        state.tap.push_str(text.trim());
        Vec::new()
    }

    /// The driver finished a batch: each tap's outcome (skills as `Ok`, plugins as `Err`).
    pub fn install_batch(&mut self, results: Vec<(String, Result<InstallOutcome, PluginOutcome>)>) {
        self.hub_progress = None;
        let waiting =
            matches!(&self.overlay, Some(Overlay::Install(InstallState { phase: InstallPhase::Batch { .. }, .. })));
        if !waiting {
            // The window was closed while fetching: nothing staged may linger.
            for (_, result) in results {
                match result {
                    Ok(InstallOutcome::Preview(preview)) => {
                        self.pending_actions.push(Action::Extension(ExtensionCommand::DiscardInstall(preview.staging)));
                    }
                    Err(PluginOutcome::Preview(preview)) => {
                        self.pending_actions.push(Action::Extension(ExtensionCommand::DiscardInstall(preview.staging)));
                    }
                    _ => {}
                }
            }
            return;
        }
        let Some(Overlay::Install(InstallState { phase: InstallPhase::Batch { items, .. }, .. })) = &mut self.overlay
        else {
            return;
        };
        for (tap, result) in results {
            let outcome = match result {
                Ok(InstallOutcome::Preview(preview)) => BatchOutcome::Skill(preview),
                Ok(InstallOutcome::UpToDate { name, .. }) => BatchOutcome::UpToDate(name),
                Ok(InstallOutcome::Failed { error, .. }) => BatchOutcome::Failed(error),
                Ok(InstallOutcome::Choose { candidates, .. }) => {
                    BatchOutcome::Failed(format!("holds {} skills; install one by its folder", candidates.len()))
                }
                Err(PluginOutcome::Preview(preview)) => BatchOutcome::Plugin(preview),
                Err(PluginOutcome::UpToDate { name, .. }) => BatchOutcome::UpToDate(name),
                Err(PluginOutcome::Failed { error, .. }) => BatchOutcome::Failed(error),
            };
            match items.iter_mut().find(|item| item.tap == tap && matches!(item.outcome, BatchOutcome::Fetching)) {
                Some(item) => {
                    item.checked = matches!(outcome, BatchOutcome::Skill(_) | BatchOutcome::Plugin(_));
                    item.outcome = outcome;
                }
                None => items.push(BatchItem {
                    checked: matches!(outcome, BatchOutcome::Skill(_) | BatchOutcome::Plugin(_)),
                    tap,
                    outcome,
                }),
            }
        }
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

    /// `/skill search [words]`, `/skill browse [source]`, or `s` on the Skills tab: the Hermes index,
    /// listed at once and narrowed as you type.
    pub fn open_skill_search(&mut self, query: Option<String>, source: Option<String>) -> Vec<Action> {
        if self.demo {
            self.toast("That isn't available in the demo recording.", ToastLevel::Info);
            return Vec::new();
        }
        let query = query.map(|q| q.trim().to_owned()).unwrap_or_default();
        let source = source.map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty() && s != "all");
        self.overlay = Some(Overlay::SkillSearch {
            query: query.clone(),
            hits: Vec::new(),
            selected: 0,
            searching: true,
            error: None,
            note: None,
            ticked: BTreeSet::new(),
            source: source.clone(),
            sources: Vec::new(),
            total: 0,
        });
        vec![Action::Extension(ExtensionCommand::BrowseSkills { source, query })]
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
                let checked = vec![false; candidates.len()];
                InstallPhase::Choose { candidates, note, selected: 0, checked }
            }
            InstallOutcome::UpToDate { name, sha } => InstallPhase::UpToDate { name, sha },
            InstallOutcome::Failed { tap, error } => {
                state.tap = tap;
                InstallPhase::Failed(error)
            }
        };
    }

    /// The driver answered a page of the index. A page for a source or words no longer shown is dropped.
    pub fn skill_search_result(&mut self, page: BrowsePage) {
        let BrowsePage { source, query, hits, total, sources, note, error } = page;
        if let Some(Overlay::SkillSearch {
            query: current,
            hits: shown,
            selected,
            searching,
            error: shown_error,
            note: shown_note,
            source: current_source,
            sources: shown_sources,
            total: shown_total,
            ..
        }) = &mut self.overlay
            && current.trim() == query
            && *current_source == source
        {
            *searching = false;
            *selected = 0;
            *shown_error = if hits.is_empty() && error.is_none() {
                let place = source.as_deref().map(|s| format!(" from {s}")).unwrap_or_default();
                Some(if query.is_empty() {
                    format!("The Hermes index lists nothing{place}.")
                } else {
                    format!("Nothing in the Hermes index{place} matches \"{query}\".")
                })
            } else {
                error
            };
            *shown_note = note;
            *shown = hits;
            *shown_total = total;
            if !sources.is_empty() {
                *shown_sources = sources;
            }
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
                        let kind = state.kind;
                        let taps = taps_in(&tap);
                        if taps.len() > 1 && kind != InstallKind::Marketplace {
                            return self.start_batch(taps, kind);
                        }
                        return match kind {
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
                        return vec![Action::Extension(ExtensionCommand::CommitInstall {
                            staging,
                            enable: true,
                            accept: false,
                        })];
                    }
                    KeyCode::Char('y') | KeyCode::Enter => {
                        self.toast(
                            "The scan found something dangerous: k keeps it off, or o reads the findings and enables it on your word.",
                            ToastLevel::Warning,
                        );
                    }
                    KeyCode::Char('o') if dangerous => {
                        state.phase = InstallPhase::AcceptDangerous(preview.clone());
                    }
                    KeyCode::Char('k') => {
                        self.overlay = None;
                        return vec![Action::Extension(ExtensionCommand::CommitInstall {
                            staging,
                            enable: false,
                            accept: false,
                        })];
                    }
                    KeyCode::Char('n') | KeyCode::Esc => {
                        self.overlay = None;
                        return vec![Action::Extension(ExtensionCommand::DiscardInstall(staging))];
                    }
                    _ => {}
                }
            }
            InstallPhase::AcceptDangerous(preview) => match key.code {
                KeyCode::Char('y') => {
                    let staging = preview.staging.clone();
                    self.overlay = None;
                    return vec![Action::Extension(ExtensionCommand::CommitInstall {
                        staging,
                        enable: true,
                        accept: true,
                    })];
                }
                KeyCode::Char('n') | KeyCode::Esc | KeyCode::Char('k') => {
                    let preview = preview.clone();
                    state.phase = InstallPhase::Preview { preview, reading: None };
                }
                _ => {}
            },
            InstallPhase::Choose { candidates, selected, checked, .. } => match key.code {
                KeyCode::Esc => self.overlay = None,
                KeyCode::Up => *selected = selected.saturating_sub(1),
                KeyCode::Down => *selected = (*selected + 1).min(candidates.len().saturating_sub(1)),
                KeyCode::Char(' ') => {
                    if let Some(flag) = checked.get_mut(*selected) {
                        *flag = !*flag;
                    }
                }
                KeyCode::Char('a') => {
                    let all = checked.iter().all(|c| *c);
                    checked.iter_mut().for_each(|c| *c = !all);
                }
                KeyCode::Enter => {
                    let ticked: Vec<String> = candidates
                        .iter()
                        .zip(checked.iter())
                        .filter(|(_, c)| **c)
                        .map(|(cand, _)| cand.tap.clone())
                        .collect();
                    if !ticked.is_empty() {
                        return self.start_batch(ticked, InstallKind::Skill);
                    }
                    if let Some(candidate) = candidates.get(*selected) {
                        let tap = candidate.tap.clone();
                        return self.start_install(tap);
                    }
                }
                _ => {}
            },
            InstallPhase::Batch { items, selected } => match key.code {
                KeyCode::Up => *selected = selected.saturating_sub(1),
                KeyCode::Down => *selected = (*selected + 1).min(items.len().saturating_sub(1)),
                KeyCode::Char(' ') => {
                    if let Some(item) = items.get_mut(*selected)
                        && item.installable()
                    {
                        item.checked = !item.checked;
                    }
                }
                KeyCode::Char('a') => {
                    let all = items.iter().filter(|i| i.installable()).all(|i| i.checked);
                    items.iter_mut().filter(|i| i.installable()).for_each(|i| i.checked = !all);
                }
                KeyCode::Char('y') | KeyCode::Enter | KeyCode::Char('k') => {
                    if items.iter().any(|i| matches!(i.outcome, BatchOutcome::Fetching)) {
                        return Vec::new();
                    }
                    let keep_off = key.code == KeyCode::Char('k');
                    let mut skills = Vec::new();
                    let mut plugins = Vec::new();
                    let mut discard = Vec::new();
                    for item in items.iter() {
                        let Some(staging) = item.staging() else { continue };
                        if !item.checked {
                            discard.push(staging);
                            continue;
                        }
                        let enable = !keep_off && !item.dangerous();
                        match item.outcome {
                            BatchOutcome::Skill(_) => skills.push((staging, enable)),
                            BatchOutcome::Plugin(_) => plugins.push((staging, enable)),
                            _ => {}
                        }
                    }
                    if skills.is_empty() && plugins.is_empty() {
                        self.toast("Nothing is ticked: space ticks a row, a ticks them all.", ToastLevel::Info);
                        return Vec::new();
                    }
                    self.overlay = None;
                    let mut actions = Vec::new();
                    if !skills.is_empty() {
                        actions.push(Action::Extension(ExtensionCommand::CommitInstallBatch(skills)));
                    }
                    if !plugins.is_empty() {
                        actions.push(Action::Extension(ExtensionCommand::CommitPluginBatch(plugins)));
                    }
                    actions.extend(discard.into_iter().map(|s| Action::Extension(ExtensionCommand::DiscardInstall(s))));
                    return actions;
                }
                KeyCode::Char('n') | KeyCode::Esc => {
                    let discard: Vec<String> = items.iter().filter_map(BatchItem::staging).collect();
                    self.overlay = None;
                    return discard
                        .into_iter()
                        .map(|s| Action::Extension(ExtensionCommand::DiscardInstall(s)))
                        .collect();
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
        let Some(Overlay::SkillSearch { query, hits, selected, searching, error, ticked, source, sources, .. }) =
            &mut self.overlay
        else {
            return Vec::new();
        };
        // Typing or changing source asks the index again; the answer replaces the page when it lands.
        let ask = |source: &Option<String>, query: &str| {
            vec![Action::Extension(ExtensionCommand::BrowseSkills { source: source.clone(), query: query.to_owned() })]
        };
        match key.code {
            KeyCode::Esc => self.overlay = None,
            KeyCode::Up => *selected = selected.saturating_sub(1),
            KeyCode::Down => *selected = (*selected + 1).min(hits.len().saturating_sub(1)),
            KeyCode::PageUp => *selected = selected.saturating_sub(8),
            KeyCode::PageDown => *selected = (*selected + 8).min(hits.len().saturating_sub(1)),
            KeyCode::Left | KeyCode::Right if !sources.is_empty() => {
                let names: Vec<&str> = sources.iter().map(|(name, _)| name.as_str()).collect();
                let at = source.as_deref().and_then(|s| names.iter().position(|n| *n == s)).map_or(0, |i| i + 1);
                let count = names.len() + 1;
                let next = if key.code == KeyCode::Right { (at + 1) % count } else { (at + count - 1) % count };
                *source = if next == 0 { None } else { Some(names[next - 1].to_owned()) };
                // Another source's rows would be wrong to keep; a narrowed query's are still right.
                hits.clear();
                *selected = 0;
                *searching = true;
                *error = None;
                return ask(source, query);
            }
            KeyCode::Tab => {
                if let Some(hit) = hits.get(*selected)
                    && !ticked.remove(&hit.tap)
                {
                    ticked.insert(hit.tap.clone());
                }
            }
            KeyCode::Char('a') if ctrl && !hits.is_empty() => {
                if hits.iter().all(|hit| ticked.contains(&hit.tap)) {
                    ticked.clear();
                } else {
                    ticked.extend(hits.iter().map(|hit| hit.tap.clone()));
                }
            }
            KeyCode::Backspace => {
                query.pop();
                *selected = 0;
                *searching = true;
                *error = None;
                return ask(source, query);
            }
            KeyCode::Char(c) if !ctrl => {
                query.push(c);
                *selected = 0;
                *searching = true;
                *error = None;
                return ask(source, query);
            }
            KeyCode::Enter if !ticked.is_empty() => {
                let taps: Vec<String> = ticked.iter().cloned().collect();
                return self.start_batch(taps, InstallKind::Skill);
            }
            KeyCode::Enter => {
                if let Some(hit) = hits.get((*selected).min(hits.len().saturating_sub(1))) {
                    let tap = hit.tap.clone();
                    return self.start_install(tap);
                } else if !*searching {
                    *searching = true;
                    *error = None;
                    return ask(source, query);
                }
            }
            _ => {}
        }
        Vec::new()
    }
}

/// How many things one checklist fetches: each is at least one GitHub API listing, and the anonymous
/// limit is 60 an hour.
const MAX_BATCH: usize = 25;

const ASK_HELP: &str = "official/security/godmode · a name from the Hermes index · owner/repo/folder[@ref] · a GitHub, skills.sh or SKILL.md link · a folder on this computer. Paste several links (one per line) for a checklist, or paste a SKILL.md itself. Nothing fetched is run: it's read, scanned and shown first.";
const ASK_PLUGIN_HELP: &str = "A name from the Hermes catalog (better-session-appearance) · name@marketplace · owner/repo[/folder][@ref] · a GitHub link · a folder on this computer. Paste several for a checklist. Its skills and MCP servers will work here; Python code, desktop parts and hooks are listed, never run.";
const ASK_MARKETPLACE_HELP: &str = "A Claude Code marketplace: owner/repo[@ref] (anthropics/claude-plugins-official), a GitHub link, or a folder holding .claude-plugin/marketplace.json. Its manifest is read at a pinned commit; nothing is installed yet.";

/// Draws the install window in whatever phase it's in.
pub fn draw_install(frame: &mut Frame<'_>, area: Rect, state: &InstallState, progress: Option<&str>) {
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
            if let Some(progress) = progress {
                lines.extend(wrap_plain(progress, theme::accent(), inner));
            }
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
        InstallPhase::AcceptDangerous(preview) => draw_accept_dangerous(frame, area, preview),
        InstallPhase::Choose { candidates, note, selected, checked } => {
            draw_choose(frame, area, &state.tap, candidates, note.as_deref(), *selected, checked);
        }
        InstallPhase::Batch { items, selected } => draw_batch(frame, area, state.kind, items, *selected, progress),
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
fn filtered_listings<'a>(
    listings: &'a [PluginListing],
    filter: &str,
    category: Option<&str>,
) -> Vec<&'a PluginListing> {
    let words: Vec<String> = filter.split_whitespace().map(str::to_lowercase).collect();
    listings
        .iter()
        .filter(|listing| category.is_none_or(|c| listing.category.as_deref() == Some(c)))
        .filter(|listing| {
            let hay = format!(
                "{} {} {} {}",
                listing.name,
                listing.description,
                listing.label,
                listing.category.as_deref().unwrap_or_default()
            )
            .to_lowercase();
            words.iter().all(|word| hay.contains(word.as_str()))
        })
        .collect()
}

/// The categories present, largest first, with how many plugins each holds.
fn categories(listings: &[PluginListing]) -> Vec<(String, usize)> {
    let mut counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for listing in listings {
        if let Some(category) = listing.category.as_deref() {
            *counts.entry(category).or_default() += 1;
        }
    }
    let mut found: Vec<(String, usize)> = counts.into_iter().map(|(name, n)| (name.to_owned(), n)).collect();
    found.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    found
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
    ticked: &BTreeSet<String>,
    category: Option<&str>,
) {
    let shown = filtered_listings(listings, filter, category);
    let groups = categories(listings);
    let rows = u16::try_from(shown.len().min(9) * 2).unwrap_or(18);
    let rect = overlay_area(area, 96, rows + 8 + u16::from(!groups.is_empty()));
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
    if !groups.is_empty() {
        // "all 1203 · tools 486 · desktop 241 …", the current one lit; ←/→ move along it.
        let mut spans = vec![Span::styled("Category  ", theme::muted())];
        let mut cells = vec![("all".to_owned(), listings.len(), category.is_none())];
        cells.extend(groups.iter().map(|(name, n)| (name.clone(), *n, category == Some(name.as_str()))));
        let mut used = 10;
        for (name, n, lit) in cells {
            let cell = format!("{name} {n}");
            if used + cell.len() + 3 > inner {
                spans.push(Span::styled(" …", theme::muted()));
                break;
            }
            used += cell.len() + 3;
            let style = if lit {
                Style::new().fg(theme::color(Token::Accent)).add_modifier(Modifier::BOLD)
            } else {
                theme::muted()
            };
            spans.push(Span::styled(cell, style));
            spans.push(Span::styled(" · ", theme::muted()));
        }
        if spans.len() > 1 && spans.last().is_some_and(|s| s.content == " · ") {
            spans.pop();
        }
        lines.push(Line::from(spans));
    }
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
        let marker = if listing.unsupported.is_some() {
            Span::styled("    ", base)
        } else {
            tickbox(ticked.contains(&listing.tap), base)
        };
        lines.push(Line::from(vec![
            Span::styled(if index == selected { "❯ " } else { "  " }, theme::accent().patch(base)),
            marker,
            Span::styled(truncate(&format!("{}{version}", listing.name), inner.saturating_sub(20)), base),
            Span::raw("  "),
            chip(listing.label.clone(), label_color(&listing.label)),
        ]));
        let detail = match &listing.unsupported {
            Some(why) => format!("can't be installed: {why}"),
            None => listing.description.clone(),
        };
        let style =
            if listing.unsupported.is_some() { Style::new().fg(theme::color(Token::Danger)) } else { theme::muted() };
        lines.push(Line::styled(format!("        {}", truncate(&detail, inner.saturating_sub(8))), style));
    }
    lines.push(Line::default());
    let keys: &[(&str, &str)] = if shown.is_empty() {
        &[("enter", "search"), ("esc", "cancel")]
    } else if ticked.is_empty() {
        &[("enter", "install"), ("tab", "tick"), ("^a", "all shown"), ("←→", "category"), ("esc", "cancel")]
    } else {
        &[("enter", "install ticked"), ("tab", "tick"), ("^a", "all shown"), ("←→", "category"), ("esc", "cancel")]
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
        let level = if dangerous { "dangerous (off unless you accept the findings)" } else { "caution" };
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
        &[("k", "keep off"), ("o", "enable on my word"), ("n", "discard"), ("r", "read SKILL.md")]
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

/// Every dangerous finding, then what accepting them means, then the question. The scan can't tell an
/// exploit write-up from instructions to carry one out; the person reading can.
fn draw_accept_dangerous(frame: &mut Frame<'_>, area: Rect, preview: &InstallPreview) {
    let rect = overlay_area(area, 96, area.height.saturating_sub(2));
    let inner = usize::from(rect.width).saturating_sub(4);
    let findings = preview.scan.findings();
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.extend(wrap_plain(
        &format!(
            "The scan found {} dangerous pattern{} in {}. They may be documentation about attacks (a CVE write-up that quotes rm -rf /) rather than instructions to carry them out; the scan can't tell, and you can.",
            findings.len(),
            if findings.len() == 1 { "" } else { "s" },
            preview.name
        ),
        Style::new(),
        inner,
    ));
    lines.push(Line::default());
    let room = usize::from(rect.height).saturating_sub(12);
    let shown = findings.len().min(room.max(3) / 2);
    for finding in findings.iter().take(shown) {
        let place = if finding.line > 0 { format!("{}:{}", finding.file, finding.line) } else { finding.file.clone() };
        let width = place.chars().count();
        lines.push(Line::from(vec![
            Span::styled(truncate(&place, inner), Style::new().fg(theme::color(Token::Danger))),
            Span::styled(format!("  {}", truncate(&finding.reason, inner.saturating_sub(width + 2))), Style::new()),
        ]));
        if !finding.excerpt.is_empty() {
            lines.push(Line::styled(
                format!("    {}", truncate(&finding.excerpt, inner.saturating_sub(4))),
                theme::muted(),
            ));
        }
    }
    if findings.len() > shown {
        lines.push(Line::styled(
            format!("    …and {} more (r on the preview reads the file)", findings.len() - shown),
            theme::muted(),
        ));
    }
    lines.push(Line::default());
    lines.extend(wrap_plain(
        "If you enable it, the model reads every one of these as instructions it may act on, with the sandbox and approvals as usual. The acceptance is tied to exactly these findings: an update that changes them switches the skill off again until you look at it anew.",
        theme::muted(),
        inner,
    ));
    lines.push(Line::default());
    lines.push(Line::styled("Enable it on your word?", Style::new().add_modifier(Modifier::BOLD)));
    lines.push(legend(&[("y", "yes, I read them"), ("n", "back")], inner));
    boxed(frame, rect, &format!("Accept the findings of {}", preview.name), lines);
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
    checked: &[bool],
) {
    let rows = u16::try_from(candidates.len().min(8) * 2).unwrap_or(16);
    let rect = overlay_area(area, 90, rows + 7);
    let inner = usize::from(rect.width).saturating_sub(4);
    let ticked = checked.iter().filter(|c| **c).count();
    let question = if ticked == 0 {
        format!("{tap} holds {} skills. Which ones?", candidates.len())
    } else {
        format!("{tap} holds {} skills. {ticked} ticked.", candidates.len())
    };
    let mut lines = wrap_plain(&question, Style::new(), inner);
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
        let tick = checked.get(index).copied().unwrap_or(false);
        lines.push(Line::from(vec![
            Span::styled(if index == selected { "❯ " } else { "  " }, theme::accent().patch(base)),
            tickbox(tick, base),
            Span::styled(truncate(&candidate.name, inner.saturating_sub(6)), base),
        ]));
        let detail = if candidate.description.is_empty() {
            candidate.path.clone()
        } else {
            format!("{}  ·  {}", candidate.description, candidate.path)
        };
        lines.push(Line::styled(format!("        {}", truncate(&detail, inner.saturating_sub(8))), theme::muted()));
    }
    lines.push(Line::default());
    let enter = if ticked == 0 { "fetch this one" } else { "fetch ticked" };
    lines
        .push(legend(&[("enter", enter), ("space", "tick"), ("a", "all"), ("↑↓", "choose"), ("esc", "cancel")], inner));
    boxed(frame, rect, "Install a skill", lines);
}

fn tickbox(ticked: bool, base: Style) -> Span<'static> {
    if ticked {
        Span::styled("[x] ", Style::new().fg(theme::color(Token::Success)).patch(base))
    } else {
        Span::styled("[ ] ", theme::muted().patch(base))
    }
}

/// The batch checklist: one row per thing staged, its verdict, and a tick box for what to install.
fn draw_batch(
    frame: &mut Frame<'_>,
    area: Rect,
    kind: InstallKind,
    items: &[BatchItem],
    selected: usize,
    progress: Option<&str>,
) {
    let rows = u16::try_from(items.len().min(10) * 2).unwrap_or(20);
    let rect = overlay_area(area, 92, rows + 7);
    let inner = usize::from(rect.width).saturating_sub(4);
    let fetching = items.iter().filter(|i| matches!(i.outcome, BatchOutcome::Fetching)).count();
    let ready = items.iter().filter(|i| i.installable()).count();
    let ticked = items.iter().filter(|i| i.checked).count();
    let what = if kind == InstallKind::Plugin { "plugins" } else { "skills" };
    let head = if fetching > 0 {
        format!("Fetching {fetching} of {} {what}…", items.len())
    } else {
        format!("{ready} of {} {what} can be installed; {ticked} ticked.", items.len())
    };
    let mut lines = wrap_plain(&head, Style::new(), inner);
    if fetching > 0
        && let Some(progress) = progress
    {
        lines.push(Line::styled(truncate(progress, inner), theme::accent()));
    }
    lines.push(Line::default());
    let first = selected.saturating_sub(9);
    for (index, item) in items.iter().enumerate().skip(first).take(10) {
        let base = if index == selected {
            Style::new().bg(theme::color(Token::UserBg)).add_modifier(Modifier::BOLD)
        } else {
            Style::new()
        };
        let marker = if item.installable() { tickbox(item.checked, base) } else { Span::styled("    ", base) };
        let (name, detail, style) = match &item.outcome {
            BatchOutcome::Fetching => (item.tap.clone(), "fetching…".to_owned(), theme::muted()),
            BatchOutcome::Skill(preview) => {
                let scan = if preview.scan.blocks() {
                    "scan: dangerous, installs off".to_owned()
                } else if preview.scan.findings().is_empty() {
                    "scan clean".to_owned()
                } else {
                    format!("scan: {} to read", preview.scan.findings().len())
                };
                let files = if preview.files.len() == 1 {
                    "1 file".to_owned()
                } else {
                    format!("{} files", preview.files.len())
                };
                let detail = format!("{}  ·  {files}  ·  {scan}", preview.description);
                (preview.name.clone(), detail, Style::new())
            }
            BatchOutcome::Plugin(preview) => {
                let files = if preview.files == 1 { "1 file".to_owned() } else { format!("{} files", preview.files) };
                let detail = format!("{}  ·  {files}", preview.plugin.capability_sentence());
                (preview.plugin.name.clone(), detail, Style::new())
            }
            BatchOutcome::UpToDate(name) => {
                (name.clone(), "already installed at the newest commit".to_owned(), theme::muted())
            }
            BatchOutcome::Failed(error) => {
                (item.tap.clone(), error.clone(), Style::new().fg(theme::color(Token::Danger)))
            }
        };
        lines.push(Line::from(vec![
            Span::styled(if index == selected { "❯ " } else { "  " }, theme::accent().patch(base)),
            marker,
            Span::styled(truncate(&name, inner.saturating_sub(6)), style.patch(base)),
        ]));
        lines.push(Line::styled(format!("        {}", truncate(&detail, inner.saturating_sub(8))), theme::muted()));
    }
    lines.push(Line::default());
    let keys: &[(&str, &str)] = if fetching > 0 {
        &[("space", "tick"), ("a", "all"), ("↑↓", "move"), ("esc", "cancel")]
    } else {
        &[("y", "install ticked"), ("k", "install off"), ("space", "tick"), ("a", "all"), ("n", "none of them")]
    };
    lines.push(legend(keys, inner));
    let title = if kind == InstallKind::Plugin { "Install plugins" } else { "Install skills" };
    boxed(frame, rect, title, lines);
}

/// One page of the index as the driver answers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowsePage {
    pub source: Option<String>,
    pub query: String,
    pub hits: Vec<SkillSearchHit>,
    pub total: usize,
    pub sources: Vec<(String, usize)>,
    pub note: Option<String>,
    pub error: Option<String>,
}

/// What the browse window shows.
pub struct SearchView<'a> {
    pub query: &'a str,
    pub hits: &'a [SkillSearchHit],
    pub selected: usize,
    pub searching: bool,
    pub error: Option<&'a str>,
    pub note: Option<&'a str>,
    pub ticked: &'a BTreeSet<String>,
    pub source: Option<&'a str>,
    pub sources: &'a [(String, usize)],
    pub total: usize,
    pub progress: Option<&'a str>,
}

/// Thousands separated with a thin space, so 103386 reads as 103 386.
fn grouped(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push('\u{2009}');
        }
        out.push(c);
    }
    out
}

/// The index browser: a filter line, the sources with their counts, then a page of hits with their
/// label, category and where they live.
pub fn draw_skill_search(frame: &mut Frame<'_>, area: Rect, view: &SearchView<'_>) {
    let SearchView { query, hits, selected, searching, error, note, ticked, source, sources, total, progress } = *view;
    let rows = u16::try_from(hits.len().min(9) * 2).unwrap_or(18);
    let rect = overlay_area(area, 96, rows + 8 + u16::from(!sources.is_empty()));
    let inner = usize::from(rect.width).saturating_sub(4);
    let all: usize = sources.iter().map(|(_, n)| n).sum();
    let count = if hits.is_empty() || searching {
        String::new()
    } else if hits.len() < total {
        format!("   {} of {}", hits.len(), grouped(total))
    } else {
        format!("   {total}")
    };
    let mut lines = vec![Line::from(vec![
        Span::styled("Filter  ", theme::muted()),
        Span::raw(query.to_owned()),
        Span::styled("▏", theme::accent()),
        Span::styled(count, theme::muted()),
    ])];
    if !sources.is_empty() {
        let mut spans = vec![Span::styled("Source  ", theme::muted())];
        let mut cells = vec![("all".to_owned(), all, source.is_none())];
        cells.extend(sources.iter().map(|(name, n)| (name.clone(), *n, source == Some(name.as_str()))));
        let mut used = 8;
        for (name, n, lit) in cells {
            let cell = format!("{name} {}", grouped(n));
            if used + cell.chars().count() + 3 > inner {
                spans.push(Span::styled(" …", theme::muted()));
                break;
            }
            used += cell.chars().count() + 3;
            let style = if lit {
                Style::new().fg(theme::color(Token::Accent)).add_modifier(Modifier::BOLD)
            } else {
                theme::muted()
            };
            spans.push(Span::styled(cell, style));
            spans.push(Span::styled(" · ", theme::muted()));
        }
        if spans.last().is_some_and(|s| s.content == " · ") {
            spans.pop();
        }
        lines.push(Line::from(spans));
    }
    lines.push(Line::default());
    if searching && hits.is_empty() && sources.is_empty() {
        lines
            .push(Line::styled("Loading the Hermes index (about 42 MB, fetched once a day and kept)…", theme::muted()));
        if let Some(progress) = progress {
            lines.push(Line::styled(truncate(progress, inner), theme::accent()));
        }
    } else if searching && hits.is_empty() {
        lines.push(Line::styled("Looking…", theme::muted()));
    } else if let Some(error) = error {
        lines.extend(wrap_plain(error, Style::new().fg(theme::color(Token::Danger)), inner));
    }
    if let Some(note) = note {
        lines.extend(wrap_plain(note, theme::muted(), inner));
    }
    let first = selected.saturating_sub(8);
    for (index, hit) in hits.iter().enumerate().skip(first).take(9) {
        let base = if index == selected {
            Style::new().bg(theme::color(Token::UserBg)).add_modifier(Modifier::BOLD)
        } else {
            Style::new()
        };
        let mut row = vec![
            Span::styled(if index == selected { "❯ " } else { "  " }, theme::accent().patch(base)),
            tickbox(ticked.contains(&hit.tap), base),
            Span::styled(truncate(&hit.name, inner.saturating_sub(30)), base),
            Span::raw("  "),
            chip(hit.label.clone(), label_color(&hit.label)),
        ];
        if let Some(category) = &hit.category {
            row.push(Span::raw(" "));
            row.push(chip(category.clone(), theme::color(Token::Muted)));
        }
        lines.push(Line::from(row));
        let place = if hit.path.is_empty() { hit.repo.clone() } else { format!("{}/{}", hit.repo, hit.path) };
        let detail = if hit.description.is_empty() { place } else { format!("{}  ·  {place}", hit.description) };
        lines.push(Line::styled(format!("        {}", truncate(&detail, inner.saturating_sub(8))), theme::muted()));
    }
    lines.push(Line::default());
    let keys: &[(&str, &str)] = if hits.is_empty() {
        &[("type", "to narrow"), ("←→", "source"), ("esc", "cancel")]
    } else if ticked.is_empty() {
        &[("enter", "install"), ("tab", "tick"), ("^a", "all shown"), ("←→", "source"), ("esc", "cancel")]
    } else {
        &[("enter", "install ticked"), ("tab", "tick"), ("^a", "all shown"), ("←→", "source"), ("esc", "cancel")]
    };
    lines.push(legend(keys, inner));
    let title = match source {
        Some("official") => "Hermes' own skills".to_owned(),
        Some(name) => format!("Skills from {name}"),
        None => "Browse skills".to_owned(),
    };
    boxed(frame, rect, &title, lines);
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
