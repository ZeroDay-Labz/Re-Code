//! The home screen: what this machine can run, which model to use, and the extensions on offer.

use crate::app::{App, Screen};
use crate::extensions_view;
use crate::theme::{self, Token, color, gradient};
use crate::widgets::{chip, legend, meter, panel};
use crate::wrap::{truncate, wrap_plain};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthStr;

const LOGO: [&str; 3] =
    ["█▀▀▄ █▀▀▀     █▀▀▀ █▀▀█ █▀▀▄ █▀▀▀", "█▄▄▀ █▀▀  ▀▀▀ █    █  █ █  █ █▀▀ ", "█  █ █▄▄▄     █▄▄▄ █▄▄█ █▄▄▀ █▄▄▄"];
const TAGLINE: &str = "a coding agent for your own machine";
const INTRO: Duration = Duration::from_millis(1400);
/// The content column stops growing here, so the screen stays composed on huge terminals.
const COLUMN: u16 = 116;
const DETAILS_WIDTH: u16 = 38;
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// The home screen's tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HomeTab {
    Home,
    Models,
    Runtimes,
    Providers,
    Mcp,
    Skills,
    Plugins,
    Tools,
    Settings,
}

impl HomeTab {
    const ALL: [Self; 9] = [
        Self::Home,
        Self::Models,
        Self::Runtimes,
        Self::Providers,
        Self::Mcp,
        Self::Skills,
        Self::Plugins,
        Self::Tools,
        Self::Settings,
    ];

    /// The tab this many places along, wrapping around.
    #[must_use]
    pub fn moved(self, delta: i32) -> Self {
        let index = Self::ALL.iter().position(|tab| *tab == self).unwrap_or(0);
        let count = i32::try_from(Self::ALL.len()).unwrap_or(7);
        let moved = (i32::try_from(index).unwrap_or(0) + delta).rem_euclid(count);
        Self::ALL[usize::try_from(moved).unwrap_or(0)]
    }

    fn label(self) -> &'static str {
        match self {
            Self::Home => "Home",
            Self::Settings => "Settings",
            Self::Models => "Models",
            Self::Runtimes => "Runtimes",
            Self::Providers => "Providers",
            Self::Mcp => "MCP",
            Self::Skills => "Skills",
            Self::Plugins => "Plugins",
            Self::Tools => "Tools",
        }
    }
}

/// A real figure about this machine, drawn as a meter.
#[derive(Debug, Clone, PartialEq)]
pub struct Meter {
    pub label: String,
    pub used_gib: Option<f64>,
    pub total_gib: Option<f64>,
    pub detail: String,
}

/// An estimate of how a model will run here. It is never presented as a measurement.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GpuFit {
    FitsGpu,
    PartlyCpu {
        over_gib: f64,
    },
    TooLarge,
    /// This driver doesn't report graphics memory, or there's no GPU.
    Unknown,
    /// A cloud model, which runs on the provider's servers.
    Cloud,
}

/// A model on offer.
#[derive(Debug, Clone, PartialEq)]
pub struct WelcomeModel {
    pub name: String,
    pub size_gib: Option<f64>,
    pub family: Option<String>,
    pub parameters: Option<String>,
    pub quantization: Option<String>,
    pub context_limit: Option<u64>,
    /// What the model declares. None of this has been tested here.
    pub tools: bool,
    pub vision: bool,
    pub thinking: bool,
    pub fit: GpuFit,
    pub loaded: bool,
    /// For a cloud model, the provider it belongs to. `None` for a local model.
    pub provider_label: Option<String>,
    /// A cloud model that costs nothing, as its provider says.
    pub free: bool,
    /// A cloud model whose provider has no API key stored yet.
    pub needs_key: bool,
    /// Price per million tokens (input, output) in US dollars, when the provider says.
    pub price: Option<(f64, f64)>,
    /// Bytes of key/value cache per token of context, when the runtime says (local models).
    pub kv_bytes_per_token: Option<u64>,
    /// What the provider's listing (or the models.dev catalog) says about it beyond the basics.
    pub details: recode_core::model_facts::ModelFacts,
}

/// Whether a runtime can be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderState {
    Ready { detail: String },
    Unavailable { headline: String, details: Vec<String> },
}

/// Everything the welcome screen shows.
#[derive(Debug, Clone, PartialEq)]
pub struct WelcomeInfo {
    pub provider_name: String,
    pub provider: ProviderState,
    pub models: Vec<WelcomeModel>,
    pub hardware: Vec<Meter>,
    /// Free graphics memory, used to show how much of it each model would take.
    pub free_vram_gib: Option<f64>,
    pub last_model: Option<String>,
}

/// Puts the models most likely to work well first: those declaring tools that fit on the GPU, largest first.
#[must_use]
pub fn sort_models(mut models: Vec<WelcomeModel>) -> Vec<WelcomeModel> {
    let rank = |model: &WelcomeModel| {
        // Local models first (best fit first), then cloud models grouped after.
        let fit = match model.fit {
            GpuFit::FitsGpu => 0,
            GpuFit::Unknown => 1,
            GpuFit::PartlyCpu { .. } => 2,
            GpuFit::TooLarge => 3,
            GpuFit::Cloud => 4,
        };
        (Group::of(model), u8::from(!model.tools), fit)
    };
    models.sort_by(|a, b| {
        rank(a)
            .cmp(&rank(b))
            .then(b.size_gib.unwrap_or(0.0).total_cmp(&a.size_gib.unwrap_or(0.0)))
            .then_with(|| a.name.cmp(&b.name))
    });
    models
}

/// The screen's own state.
/// Which models to show, beyond the typed filter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Only {
    pub free: bool,
    pub tools: bool,
    pub vision: bool,
}

/// How the list is grouped.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Group {
    Local,
    /// The provider charges nothing for it on this account, by its own listing.
    Free,
    /// Billed to the user's own account with that provider; one group per provider, so what a Gemini
    /// key pays for isn't mixed with what an OpenRouter key pays for.
    Paid {
        provider: String,
    },
    NeedsKey,
}

impl Group {
    #[must_use]
    pub fn of(model: &WelcomeModel) -> Self {
        match &model.provider_label {
            None => Self::Local,
            Some(_) if model.needs_key => Self::NeedsKey,
            Some(_) if model.free => Self::Free,
            Some(provider) => Self::Paid { provider: provider.clone() },
        }
    }

    fn label(&self) -> String {
        match self {
            Self::Local => "On this computer".to_owned(),
            Self::Free => "Free · no charge on your account".to_owned(),
            Self::Paid { provider } => format!("Paid · your {provider} account"),
            Self::NeedsKey => "Needs a key".to_owned(),
        }
    }
}

/// The heading a model is listed under: its group, or its provider when sorted by provider.
fn heading_of(model: &WelcomeModel, by_provider: bool) -> String {
    match (&model.provider_label, by_provider) {
        (Some(label), true) => label.clone(),
        (None, true) => Group::Local.label(),
        (_, false) => Group::of(model).label(),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct WelcomeState {
    pub filter: String,
    pub only: Only,
    /// An index into all models, so the selection survives filtering.
    pub selected: usize,
    pub tab: HomeTab,
    /// Selected row in the MCP, Skills and Tools tabs.
    extension_rows: [usize; 8],
    opened: Instant,
    /// Models listed under their provider (Ctrl+P) instead of local, free, paid.
    pub by_provider: bool,
}

impl WelcomeState {
    #[must_use]
    pub fn new(now: Instant) -> Self {
        Self {
            filter: String::new(),
            only: Only::default(),
            selected: 0,
            tab: HomeTab::Models,
            extension_rows: [0; 8],
            opened: now,
            by_provider: false,
        }
    }

    #[must_use]
    pub fn intro_running(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.opened) < INTRO
    }

    fn extension_slot(&self) -> usize {
        match self.tab {
            HomeTab::Models | HomeTab::Providers => 0,
            HomeTab::Mcp => 1,
            HomeTab::Skills => 2,
            HomeTab::Tools => 3,
            HomeTab::Home => 4,
            HomeTab::Settings => 5,
            HomeTab::Runtimes => 6,
            HomeTab::Plugins => 7,
        }
    }

    /// The selected row in the current extensions tab, kept within the list as it changes.
    #[must_use]
    pub fn extension_index(&self, count: usize) -> usize {
        self.extension_rows[self.extension_slot()].min(count.saturating_sub(1))
    }

    pub fn set_extension_index(&mut self, index: usize) {
        let slot = self.extension_slot();
        self.extension_rows[slot] = index;
    }

    pub fn move_extension(&mut self, delta: isize, count: usize) {
        let moved = self.extension_index(count).saturating_add_signed(delta).min(count.saturating_sub(1));
        self.set_extension_index(moved);
    }

    /// Indexes of the models matching the filter: every typed character must appear in order.
    #[must_use]
    pub fn matches(&self, info: &WelcomeInfo) -> Vec<usize> {
        let filter = self.filter.to_lowercase();
        let mut matches: Vec<usize> = info
            .models
            .iter()
            .enumerate()
            .filter(|(_, model)| {
                let name = model.name.to_lowercase();
                let mut letters = name.chars();
                filter.chars().filter(|c| !c.is_whitespace()).all(|wanted| letters.any(|c| c == wanted))
            })
            .filter(|(_, model)| {
                (!self.only.free || model.free)
                    && (!self.only.tools || model.tools)
                    && (!self.only.vision || model.vision)
            })
            .map(|(index, _)| index)
            .collect();
        // Grouped: local, free, paid, then those that need a key; or local first, then by provider.
        // Stable within a group either way.
        if self.by_provider {
            matches.sort_by_cached_key(|index| {
                let model = &info.models[*index];
                (model.provider_label.is_some(), heading_of(model, true).to_lowercase(), Group::of(model))
            });
        } else {
            matches.sort_by_key(|index| Group::of(&info.models[*index]));
        }
        matches
    }

    /// Switches a filter toggle, keeping a selection that still shows.
    pub fn toggle(&mut self, which: fn(&mut Only) -> &mut bool, info: Option<&WelcomeInfo>) {
        let flag = which(&mut self.only);
        *flag = !*flag;
        if let Some(info) = info {
            let matches = self.matches(info);
            if !matches.contains(&self.selected)
                && let Some(first) = matches.first()
            {
                self.selected = *first;
            }
        }
    }

    /// Moves within the matches, stopping at each end.
    pub fn move_selection(&mut self, matches: &[usize], delta: isize) {
        let Some(last) = matches.len().checked_sub(1) else { return };
        let position = matches.iter().position(|index| *index == self.selected).unwrap_or(0);
        self.selected = matches[position.saturating_add_signed(delta).min(last)];
    }

    /// Changes the filter, keeping the selection if it still matches.
    pub fn set_filter(&mut self, filter: String, info: Option<&WelcomeInfo>) {
        self.filter = filter;
        if let Some(info) = info {
            let matches = self.matches(info);
            if !matches.contains(&self.selected)
                && let Some(first) = matches.first()
            {
                self.selected = *first;
            }
        }
    }
}

/// The wordmark in the signature gradient, with a bright band sweeping over it just after opening.
fn logo_line(row: &str, progress: Option<f32>) -> Line<'static> {
    #[allow(clippy::cast_precision_loss)]
    let width = row.chars().count().max(1) as f32;
    let spans: Vec<Span<'static>> = row
        .chars()
        .enumerate()
        .map(|(column, c)| {
            #[allow(clippy::cast_precision_loss)]
            let x = column as f32 / width;
            let mut shade = gradient(x);
            if let Some(progress) = progress {
                let centre = progress * 1.6 - 0.3;
                let glow = (1.0 - (x - centre).abs() / 0.18).clamp(0.0, 1.0);
                shade = theme::lerp(shade, color(Token::Text), glow);
            }
            Span::styled(c.to_string(), Style::new().fg(shade))
        })
        .collect();
    Line::from(spans).centered()
}

fn fit_marker(fit: GpuFit) -> (&'static str, Style) {
    match fit {
        GpuFit::FitsGpu => ("●", Style::new().fg(color(Token::Success))),
        GpuFit::PartlyCpu { .. } => ("◐", Style::new().fg(color(Token::Attention))),
        GpuFit::TooLarge => ("○", Style::new().fg(color(Token::Danger))),
        GpuFit::Unknown => ("·", theme::muted()),
        GpuFit::Cloud => ("☁", Style::new().fg(theme::color(Token::Sky))),
    }
}

fn fit_sentence(fit: GpuFit) -> String {
    match fit {
        GpuFit::FitsGpu => "Estimate: should run entirely on your GPU.".to_owned(),
        GpuFit::PartlyCpu { over_gib } => {
            format!("Estimate: about {over_gib:.1} GiB won't fit on your GPU, so part of it runs slower on the CPU.")
        }
        GpuFit::TooLarge => "Estimate: larger than your available memory. It probably won't load.".to_owned(),
        GpuFit::Unknown => "Your GPU's free memory isn't known, so there's no estimate.".to_owned(),
        GpuFit::Cloud => "Runs on the provider's servers, so it doesn't use your GPU.".to_owned(),
    }
}

fn declares(model: &WelcomeModel) -> Vec<&'static str> {
    [("tools", model.tools), ("vision", model.vision), ("thinking", model.thinking)]
        .into_iter()
        .filter_map(|(name, present)| present.then_some(name))
        .collect()
}

fn tab_count(app: &App, tab: HomeTab) -> Option<usize> {
    let extensions = app.extensions.as_ref();
    match tab {
        HomeTab::Home | HomeTab::Settings => None,
        HomeTab::Models => app.welcome_info.as_ref().map(|info| info.models.len()),
        HomeTab::Runtimes => app.runtimes.as_ref().map(|info| info.runtimes.len()),
        HomeTab::Providers => extensions.map(|e| e.providers.len()),
        HomeTab::Mcp => extensions.map(|e| e.servers.len()),
        HomeTab::Skills => extensions.map(|e| e.skills.len()),
        HomeTab::Plugins => extensions.map(|e| e.plugins.len()),
        HomeTab::Tools => extensions.map(|e| e.tools.len()),
    }
}

/// Solid tabs with counts, recording where each landed so they can be clicked.
fn draw_tabs(frame: &mut Frame<'_>, area: Rect, app: &mut App, current: HomeTab) {
    let mut spans = Vec::new();
    let mut x = area.x;
    let end = area.x + area.width;
    for tab in HomeTab::ALL {
        let count = tab_count(app, tab).map_or_else(String::new, |count| format!(" {count}"));
        let text = format!(" {}{count} ", tab.label());
        let width = u16::try_from(text.width()).unwrap_or(u16::MAX);
        if x + width > end {
            break;
        }
        let style = if tab == current {
            Style::new()
                .fg(theme::contrast_for(color(Token::Accent)))
                .bg(color(Token::Accent))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(theme::color(Token::Text))
        };
        app.hits.tabs.push((area.y, x, x + width, tab));
        spans.push(Span::styled(text, style));
        x += width;
        if x + 1 < end {
            spans.push(Span::styled("│", theme::faint()));
            x += 1;
        }
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), Rect::new(area.x, area.y, area.width, 1));
}

fn draw_meter(frame: &mut Frame<'_>, area: Rect, item: &Meter) {
    let width = usize::from(area.width);
    let label = format!("{:<7}", truncate(&item.label, 7));
    let figures = match (item.used_gib, item.total_gib) {
        (Some(used), Some(total)) => format!("{used:.1} of {total:.1} GiB used"),
        _ => String::new(),
    };
    let detail_room = width.saturating_sub(label.width() + figures.width() + 24);
    let detail = truncate(&item.detail, detail_room.min(44));
    let bar = width.saturating_sub(label.width() + figures.width() + detail.width() + 4);
    let mut spans = vec![Span::styled(label, theme::muted()), Span::raw(" ")];
    if let (Some(used), Some(total)) = (item.used_gib, item.total_gib)
        && total > 0.0
        && bar >= 6
    {
        spans.extend(meter(used / total, bar));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(figures, theme::muted()));
    }
    if !detail.is_empty() {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(detail, theme::faint()));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn home_legend(app: &App, tab: HomeTab) -> Vec<(&'static str, &'static str)> {
    let mut items: Vec<(&str, &str)> = Vec::new();
    if app.quit_armed {
        items.push(("^C", "again to exit"));
    }
    match tab {
        HomeTab::Home => items.extend([("enter", "open"), ("↑↓", "choose"), ("d", "delete chat")]),
        HomeTab::Runtimes => items.extend([("enter", "start"), ("↑↓", "choose")]),
        HomeTab::Settings => {
            items.extend([("enter", "change"), ("←→", "adjust"), ("↑↓", "choose"), ("d", "remove key")])
        }
        HomeTab::Models => {
            if app.welcome_info.as_ref().is_some_and(|info| !info.models.is_empty()) {
                items.push(("enter", "start"));
                items.push(("↑↓", "choose"));
                // The filter row says "type to filter" itself.
                let only = match &app.screen {
                    Screen::Welcome(welcome) => welcome.only,
                    Screen::Conversation => Only::default(),
                };
                items.push(("^F", if only.free { "free ●" } else { "free ○" }));
                items.push(("^T", if only.tools { "tools ●" } else { "tools ○" }));
                items.push(("^V", if only.vision { "vision ●" } else { "vision ○" }));
                let by_provider = matches!(&app.screen, Screen::Welcome(welcome) if welcome.by_provider);
                items.push(("^P", if by_provider { "provider ●" } else { "provider ○" }));
                items.push(("k", "key"));
            }
        }
        HomeTab::Providers => {
            items.extend([
                ("a", "add"),
                ("p", "preset"),
                ("o", "opencode"),
                ("t", "test"),
                ("e", "edit"),
                ("space", "on/off"),
                ("d", "delete"),
            ]);
        }
        HomeTab::Mcp => {
            items.extend([
                ("a", "add"),
                ("p", "preset"),
                ("s", "search"),
                ("f", "tools"),
                ("t", "test"),
                ("i", "import"),
                ("o", "opencode"),
                ("e", "edit"),
                ("space", "on/off"),
                ("x", "expose"),
                ("d", "delete"),
            ]);
        }
        HomeTab::Skills => {
            items.extend([
                ("a", "add"),
                ("i", "install"),
                ("e", "edit"),
                ("space", "on/off"),
                ("d", "delete"),
                ("s", "browse"),
                ("u", "update"),
            ]);
        }
        HomeTab::Plugins => {
            items.extend([
                ("i", "install"),
                ("b", "browse"),
                ("s", "search"),
                ("m", "marketplace"),
                ("space", "on/off"),
                ("u", "update"),
                ("d", "remove"),
            ]);
        }
        HomeTab::Tools => {
            items.extend([("a", "add"), ("e", "edit"), ("space", "on/off"), ("d", "delete")]);
        }
    }
    items.push(("tab", "next"));
    if !app.model.is_empty() {
        items.push(("esc", "back"));
    }
    items.push(("^R", "rescan"));
    if !app.quit_armed {
        items.push(("^C", "quit"));
    }
    items
}

/// Draws the screen, removing decoration as space runs out and keeping the list and keys to the last.
pub fn draw(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let crate::app::Screen::Welcome(welcome) = &app.screen else { return };
    let welcome = welcome.clone();
    app.hits = crate::app::Hits::default();
    let info = app.welcome_info.clone();
    let intro = (!app.reduced_motion && welcome.intro_running(app.now))
        .then(|| app.now.saturating_duration_since(welcome.opened).as_secs_f32() / INTRO.as_secs_f32());

    let meters: u16 = match (area.height, &info) {
        (22.., Some(info)) if area.width >= 44 => u16::try_from(info.hardware.len().min(2)).unwrap_or(0),
        _ => 0,
    };
    let logo_rows: u16 = if area.height >= 26 && area.width >= 37 {
        5
    } else if area.height >= 12 {
        2
    } else {
        0
    };
    let tabs_rows: u16 = u16::from(area.height >= 7 && area.width >= 26);
    let chrome = logo_rows + tabs_rows + meters + 1;
    let room = area.height.saturating_sub(chrome);
    // Very tall terminals get a composed screen, not a list stretched across all of it.
    let body_rows = room.min(24);

    let total = chrome + body_rows;
    let mut y = area.y + area.height.saturating_sub(total) / 2;
    let column_width = area.width.saturating_sub(2).clamp(1, COLUMN);
    let column_x = area.x + (area.width - column_width) / 2;

    if logo_rows == 5 {
        for (index, row) in LOGO.iter().enumerate() {
            let row_y = y + u16::try_from(index).unwrap_or(0);
            frame.render_widget(Paragraph::new(logo_line(row, intro)), Rect::new(area.x, row_y, area.width, 1));
        }
        let tagline = Line::styled(TAGLINE, theme::muted()).centered();
        frame.render_widget(Paragraph::new(tagline), Rect::new(area.x, y + 3, area.width, 1));
    } else if logo_rows == 2 {
        let brand = Line::styled("◆ re-code", theme::accent().add_modifier(Modifier::BOLD)).centered();
        frame.render_widget(Paragraph::new(brand), Rect::new(area.x, y, area.width, 1));
    }
    y += logo_rows;

    if tabs_rows > 0 {
        draw_tabs(frame, Rect::new(column_x, y, column_width, 1), app, welcome.tab);
        y += 1;
    }

    let body = Rect::new(column_x, y, column_width, body_rows);
    let bordered = body.height >= 5 && body.width >= 30;
    let inner = if bordered {
        let bottom = app.home_message.as_ref().map(|(text, is_error)| {
            let style =
                if *is_error { Style::new().fg(color(Token::Danger)) } else { Style::new().fg(color(Token::Success)) };
            Line::styled(format!(" {} ", truncate(text, usize::from(body.width).saturating_sub(6))), style)
        });
        let title = Line::styled(body_title(&welcome, info.as_ref()), Style::new().fg(theme::color(Token::Attention)));
        panel(frame, body, title, None, bottom, true)
    } else {
        body
    };
    if welcome.tab == HomeTab::Home {
        let count = crate::home::dashboard_rows(app).len();
        crate::home::draw_home(frame, inner, app, welcome.extension_index(count));
    } else if welcome.tab == HomeTab::Runtimes {
        let count = app.runtimes.as_ref().map_or(0, |info| crate::runtimes_view::rows(info).len());
        crate::runtimes_view::draw(frame, inner, app, welcome.extension_index(count));
    } else if welcome.tab == HomeTab::Settings {
        let count = crate::home::settings_rows(app).len();
        crate::home::draw_settings(frame, inner, app, welcome.extension_index(count));
    } else if welcome.tab == HomeTab::Models {
        let rows = draw_models(frame, inner, app, &welcome, info.as_ref());
        app.model_list_rows = rows;
    } else {
        let count = match welcome.tab {
            HomeTab::Providers => app.extensions.as_ref().map_or(0, |e| e.providers.len()),
            HomeTab::Mcp => app.extensions.as_ref().map_or(0, |e| e.servers.len()),
            HomeTab::Skills => app.extensions.as_ref().map_or(0, |e| e.skills.len()),
            HomeTab::Plugins => app.extensions.as_ref().map_or(0, |e| extensions_view::plugin_rows(e).len()),
            _ => app.extensions.as_ref().map_or(0, |e| e.tools.len()),
        };
        extensions_view::draw(frame, inner, app, welcome.tab, welcome.extension_index(count));
    }
    y += body_rows;

    if meters > 0
        && let Some(info) = &info
    {
        for item in info.hardware.iter().take(2) {
            draw_meter(frame, Rect::new(column_x, y, column_width, 1), item);
            y += 1;
        }
    }

    let keys = legend(&home_legend(app, welcome.tab), usize::from(area.width)).centered();
    let keys_y = (area.y + area.height).saturating_sub(1).min(y);
    frame.render_widget(Paragraph::new(keys), Rect::new(area.x, keys_y, area.width, 1));
}

fn body_title(welcome: &WelcomeState, info: Option<&WelcomeInfo>) -> String {
    match welcome.tab {
        HomeTab::Models => match info {
            Some(info) if !info.models.is_empty() => {
                let matching = welcome.matches(info).len();
                let provider = match &info.provider {
                    ProviderState::Ready { detail } => detail.clone(),
                    ProviderState::Unavailable { .. } => "Cloud".to_owned(),
                };
                format!(" {provider} · {matching} of {} models ", info.models.len())
            }
            Some(info) => format!(" {} ", info.provider_name),
            None => " Models ".to_owned(),
        },
        HomeTab::Home => " Home ".to_owned(),
        HomeTab::Runtimes => " Runtimes and containers ".to_owned(),
        HomeTab::Settings => " Settings ".to_owned(),
        HomeTab::Providers => " Model providers ".to_owned(),
        HomeTab::Mcp => " MCP servers ".to_owned(),
        HomeTab::Skills => " Skills ".to_owned(),
        HomeTab::Plugins => " Plugins and marketplaces ".to_owned(),
        HomeTab::Tools => " Custom tools ".to_owned(),
    }
}

/// Draws the list and details, returning how many model rows fit.
fn draw_models(
    frame: &mut Frame<'_>,
    inner: Rect,
    app: &mut App,
    welcome: &WelcomeState,
    info: Option<&WelcomeInfo>,
) -> usize {
    if inner.height == 0 || inner.width == 0 {
        return 0;
    }
    let Some(info) = info else {
        let frame_index = usize::try_from(app.now.saturating_duration_since(welcome.opened).as_millis() / 100)
            .unwrap_or(0)
            % SPINNER.len();
        let text = format!("{} Looking for models…", SPINNER[frame_index]);
        frame.render_widget(
            Paragraph::new(Line::styled(truncate(&text, usize::from(inner.width)), theme::accent())),
            inner,
        );
        return 0;
    };
    // With Ollama down, say so, then still list whatever cloud models there are below the notice.
    let inner = if let ProviderState::Unavailable { headline, details } = &info.provider {
        let width = usize::from(inner.width);
        let mut lines = wrap_plain(headline, theme::attention(), width);
        if info.models.is_empty() {
            for detail in details {
                lines.push(Line::default());
                lines.extend(wrap_plain(detail, Style::new(), width));
            }
            lines.push(Line::default());
            lines.extend(wrap_plain("Press Ctrl+R to look again.", theme::muted(), width));
            frame.render_widget(Paragraph::new(lines), inner);
            return 0;
        }
        // Keep the notice short so the list has room; the details show when there's plenty.
        if inner.height >= 14 {
            for detail in details {
                lines.extend(wrap_plain(detail, theme::muted(), width));
            }
        }
        lines.push(Line::default());
        let used = u16::try_from(lines.len()).unwrap_or(u16::MAX).min(inner.height.saturating_sub(2));
        frame.render_widget(Paragraph::new(lines), Rect::new(inner.x, inner.y, inner.width, used));
        Rect::new(inner.x, inner.y + used, inner.width, inner.height - used)
    } else {
        inner
    };
    let show_details = inner.width >= DETAILS_WIDTH + 40 && inner.height >= 8;
    let (list_area, details_area) = if show_details {
        let list = Rect::new(inner.x, inner.y, inner.width - DETAILS_WIDTH - 3, inner.height);
        let details = Rect::new(list.x + list.width + 3, inner.y, DETAILS_WIDTH, inner.height);
        (list, Some(details))
    } else {
        (inner, None)
    };
    let rows = draw_list(frame, list_area, welcome, info, app);
    if let Some(details) = details_area {
        let rule: Vec<Line<'_>> = (0..details.height).map(|_| Line::styled("│", theme::faint())).collect();
        frame.render_widget(Paragraph::new(rule), Rect::new(details.x - 2, details.y, 1, details.height));
        if let Some(model) = info.models.get(welcome.selected) {
            draw_details(frame, details, model, info.free_vram_gib);
        }
    }
    rows
}

fn draw_list(frame: &mut Frame<'_>, area: Rect, welcome: &WelcomeState, info: &WelcomeInfo, app: &mut App) -> usize {
    if area.height == 0 || area.width == 0 {
        return 0;
    }
    let width = usize::from(area.width);
    // The filter row is the first thing to go when the window is very short.
    let show_filter = area.height >= 4 || !welcome.filter.is_empty();
    let mut y = area.y;
    if show_filter {
        let line = if welcome.filter.is_empty() {
            Line::from(vec![
                Span::styled("/ ", theme::accent()),
                Span::styled(truncate("type to filter", width.saturating_sub(2)), theme::faint()),
            ])
        } else {
            Line::from(vec![
                Span::styled("/ ", theme::accent()),
                Span::raw(truncate(&welcome.filter, width.saturating_sub(2))),
            ])
        };
        frame.render_widget(Paragraph::new(line), Rect::new(area.x, y, area.width, 1));
        y += 1;
        if area.height >= 6 {
            y += 1;
        }
    }
    let rows = usize::from((area.y + area.height).saturating_sub(y));
    if rows == 0 {
        return 0;
    }
    let matches = welcome.matches(info);
    if matches.is_empty() {
        let text = truncate("No models match", width);
        frame.render_widget(Paragraph::new(Line::styled(text, theme::muted())), Rect::new(area.x, y, area.width, 1));
        return rows;
    }
    // Group headings only when there's room for them, and only when there's more than one group.
    let by_provider = welcome.by_provider;
    let groups: std::collections::BTreeSet<String> =
        matches.iter().map(|index| heading_of(&info.models[*index], by_provider)).collect();
    let with_headings = rows >= 6 && groups.len() > 1;
    let mut list: Vec<ListRow> = Vec::new();
    let mut current_group: Option<String> = None;
    for index in &matches {
        let group = heading_of(&info.models[*index], by_provider);
        if with_headings && current_group.as_ref() != Some(&group) {
            let count = matches.iter().filter(|other| heading_of(&info.models[**other], by_provider) == group).count();
            list.push(ListRow::Heading(group.clone(), count));
            current_group = Some(group);
        }
        list.push(ListRow::Model(*index));
    }
    let position =
        list.iter().position(|row| matches!(row, ListRow::Model(index) if *index == welcome.selected)).unwrap_or(0);
    let first =
        (position + 1).saturating_sub(rows).max(position.saturating_sub(rows / 2)).min(list.len().saturating_sub(rows));
    for (offset, row) in list.iter().enumerate().skip(first).take(rows) {
        let row_y = y + u16::try_from(offset - first).unwrap_or(0);
        let rect = Rect::new(area.x, row_y, area.width, 1);
        match row {
            ListRow::Heading(group, count) => {
                let text = format!("{group} · {count}");
                let text = truncate(&text, width.saturating_sub(4));
                let fill = width.saturating_sub(text.width() + 4);
                let line = Line::from(vec![
                    Span::styled("─ ", theme::faint()),
                    Span::styled(text, Style::new().fg(color(Token::Attention))),
                    Span::styled(format!(" {}", "─".repeat(fill)), theme::faint()),
                ]);
                frame.render_widget(Paragraph::new(line), rect);
            }
            ListRow::Model(index) => {
                let Some(model) = info.models.get(*index) else { continue };
                let line = model_row(model, offset == position, width, app.model == model.name);
                frame.render_widget(Paragraph::new(line), rect);
                app.hits.rows.push((row_y, area.x, area.x + area.width, *index));
            }
        }
    }
    rows
}

/// A row of the model list: a model, or the heading of the group that follows.
enum ListRow {
    Heading(String, usize),
    Model(usize),
}

/// Which columns the list shows. They appear or disappear for the whole list, because a label missing from
/// one row would read as the model lacking that capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Columns {
    NameOnly,
    Size,
    SizeAndTools,
    SizeAndAll,
}

const SIZE_WIDTH: usize = 8;
const ALL_CHIPS_WIDTH: usize = "tools vision thinking".len();
const TOOLS_CHIP_WIDTH: usize = "tools".len();

fn columns_for(room: usize) -> Columns {
    if room >= 26 + SIZE_WIDTH + ALL_CHIPS_WIDTH + 4 {
        Columns::SizeAndAll
    } else if room >= 22 + SIZE_WIDTH + TOOLS_CHIP_WIDTH + 4 {
        Columns::SizeAndTools
    } else if room >= 18 + SIZE_WIDTH + 2 {
        Columns::Size
    } else {
        Columns::NameOnly
    }
}

/// One model: marker and name, then whichever columns this width allows.
fn model_row(model: &WelcomeModel, selected: bool, width: usize, current: bool) -> Line<'static> {
    let base =
        if selected { Style::new().bg(theme::color(Token::UserBg)).add_modifier(Modifier::BOLD) } else { Style::new() };
    let dim = if model.tools { base } else { theme::muted().patch(base) };
    let pointer = if selected { "❯ " } else { "  " };
    let (marker, marker_style) = if model.needs_key {
        ("○", theme::muted())
    } else if model.free && model.provider_label.is_some() {
        ("★", Style::new().fg(color(Token::Success)))
    } else {
        fit_marker(model.fit)
    };
    // Cloud models read by price at a glance: free in green, paid in the theme's accent.
    let dim = if model.needs_key {
        theme::muted().patch(base)
    } else if model.provider_label.is_some() && model.free {
        Style::new().fg(color(Token::Success)).patch(base)
    } else if model.provider_label.is_some() {
        theme::accent().patch(base)
    } else {
        dim
    };
    let room = width.saturating_sub(4);
    let columns = columns_for(room);
    let size = match (&model.provider_label, model.size_gib) {
        (Some(label), _) => label.clone(),
        (None, Some(size)) => format!("{size:.1} GiB"),
        (None, None) => String::new(),
    };
    let size = format!("{size:>SIZE_WIDTH$}");
    let right = match columns {
        Columns::NameOnly => String::new(),
        Columns::Size => format!("  {size}"),
        Columns::SizeAndTools => {
            let chip = if model.tools { "tools" } else { "" };
            format!("  {size}  {chip:<TOOLS_CHIP_WIDTH$}")
        }
        Columns::SizeAndAll => {
            let chips = declares(model).join(" ");
            format!("  {size}  {chips:<ALL_CHIPS_WIDTH$}")
        }
    };
    let current_mark = if current { " (current)" } else { "" };
    let name = truncate(&format!("{}{current_mark}", model.name), room.saturating_sub(right.width()));
    let fill = room.saturating_sub(name.width() + right.width());
    Line::from(vec![
        Span::styled(pointer, theme::accent().patch(base)),
        Span::styled(format!("{marker} "), marker_style.patch(base)),
        Span::styled(name, dim),
        Span::styled(" ".repeat(fill), base),
        Span::styled(right, theme::muted().patch(base)),
    ])
}

fn draw_details(frame: &mut Frame<'_>, area: Rect, model: &WelcomeModel, free_vram: Option<f64>) {
    let width = usize::from(area.width);
    let mut lines = wrap_plain(&model.name, Style::new().add_modifier(Modifier::BOLD), width);
    let facts: Vec<String> =
        [model.family.clone(), model.parameters.clone(), model.quantization.clone()].into_iter().flatten().collect();
    if !facts.is_empty() {
        lines.extend(wrap_plain(&facts.join(" · "), theme::muted(), width));
    }
    // What the provider says about it, cut to a few lines: listings are written by the provider.
    let about = &model.details;
    if let Some(display) = about.display_name.as_ref().filter(|display| **display != model.name) {
        lines.extend(wrap_plain(&crate::sanitize::sanitize(display), theme::muted(), width));
    }
    if let Some(description) = &about.description {
        let mut text = wrap_plain(&crate::sanitize::sanitize(description), Style::new(), width);
        if text.len() > 5 {
            text.truncate(5);
            if let Some(last) = text.last_mut() {
                let shown: String = last.spans.iter().map(|span| span.content.as_ref()).collect();
                *last = Line::raw(format!("{}…", truncate(&shown, width.saturating_sub(1))));
            }
        }
        lines.push(Line::default());
        lines.extend(text);
    }
    lines.push(Line::default());

    let mut chips = Vec::new();
    for (name, present, color) in [
        ("tools", model.tools, color(Token::Success)),
        ("vision", model.vision, color(Token::Sky)),
        ("thinking", model.thinking, color(Token::Violet)),
    ] {
        if present {
            chips.push(chip(name, color));
            chips.push(Span::raw(" "));
        }
    }
    if model.free {
        lines.push(Line::from(chip("free", color(Token::Success))));
    } else if let Some((input, output)) = model.price {
        let cached = about.cached_price.map(|cached| format!(" · ${cached:.2} cached in")).unwrap_or_default();
        let text = format!("${input:.2} in · ${output:.2} out{cached}, per million tokens");
        lines.extend(wrap_plain(&text, theme::accent(), width));
    }
    if model.needs_key {
        let who = model.provider_label.as_deref().unwrap_or("this provider");
        lines.extend(wrap_plain(
            &format!("{who} has no API key yet. Press k to add one."),
            Style::new().fg(color(Token::Attention)),
            width,
        ));
    }
    if !chips.is_empty() && chips.iter().map(|span| span.content.width()).sum::<usize>() <= width {
        lines.push(Line::from(chips));
        let source = if model.provider_label.is_some() {
            "As the provider reports them; not tested here."
        } else {
            "Declared by the model, not tested here."
        };
        lines.extend(wrap_plain(source, theme::muted(), width));
        lines.push(Line::default());
    }

    if let (Some(size), Some(free)) = (model.size_gib, free_vram)
        && free > 0.0
        && width >= 24
    {
        lines.push(Line::styled("Share of free GPU memory", Style::new().fg(color(Token::Attention))));
        let figures = format!(" {size:.1} of {free:.1}");
        lines.push(Line::from(
            [meter(size / free, width.saturating_sub(figures.width())), vec![Span::styled(figures, theme::muted())]]
                .concat(),
        ));
    }
    lines.extend(wrap_plain(&fit_sentence(model.fit), Style::new(), width));
    lines.push(Line::default());
    if let Some(limit) = model.context_limit {
        let replies = about.max_output.map(|max| format!(", replies up to {max}")).unwrap_or_default();
        lines.extend(wrap_plain(&format!("Context limit: {limit} tokens{replies}"), theme::muted(), width));
    }
    if let Some(modalities) = &about.modalities {
        lines.extend(wrap_plain(&format!("Takes {modalities}"), theme::muted(), width));
    }
    if let Some(hosts) = &about.hosts {
        lines.extend(wrap_plain(&format!("Served by {}", crate::sanitize::sanitize(hosts)), theme::muted(), width));
    }
    let dates: Vec<String> = [
        about.released.as_ref().map(|date| format!("released {date}")),
        about.knowledge.as_ref().map(|date| format!("knowledge to {date}")),
    ]
    .into_iter()
    .flatten()
    .collect();
    if !dates.is_empty() {
        let text = dates.join(" · ");
        let text = format!("{}{}", text[..1].to_uppercase(), &text[1..]);
        lines.extend(wrap_plain(&text, theme::muted(), width));
    }
    if let Some(retires) = &about.retires {
        lines.extend(wrap_plain(
            &format!("The provider stops serving it on {retires}."),
            Style::new().fg(color(Token::Attention)),
            width,
        ));
    }
    if let Some(bytes) = model.kv_bytes_per_token {
        #[allow(clippy::cast_precision_loss)]
        let gib_at = |tokens: u64| (bytes * tokens) as f64 / (1024.0 * 1024.0 * 1024.0);
        lines.extend(wrap_plain(
            &format!(
                "Context memory (estimated): ~{:.1} GiB at 16k, ~{:.1} GiB at 64k, on top of the weights",
                gib_at(16384),
                gib_at(65536)
            ),
            theme::muted(),
            width,
        ));
    }
    if model.loaded {
        lines.extend(wrap_plain("Loaded in memory right now.", Style::new().fg(color(Token::Success)), width));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorting_by_provider_lists_local_first_then_each_provider() {
        let model = |name: &str, provider: Option<&str>, free: bool| WelcomeModel {
            name: name.to_owned(),
            size_gib: None,
            family: None,
            parameters: None,
            quantization: None,
            context_limit: None,
            tools: true,
            vision: false,
            thinking: false,
            fit: GpuFit::Cloud,
            loaded: false,
            provider_label: provider.map(str::to_owned),
            free,
            needs_key: false,
            price: None,
            kv_bytes_per_token: None,
            details: recode_core::model_facts::ModelFacts::default(),
        };
        let info = WelcomeInfo {
            models: vec![
                model("zen-free", Some("opencode-zen"), true),
                model("local", None, false),
                model("or-paid", Some("openrouter"), false),
                model("anthropic-paid", Some("anthropic"), false),
                model("or-free", Some("openrouter"), true),
            ],
            provider_name: "Ollama".to_owned(),
            provider: ProviderState::Ready { detail: String::new() },
            hardware: Vec::new(),
            free_vram_gib: None,
            last_model: None,
        };
        let mut state = WelcomeState::new(Instant::now());
        let names = |state: &WelcomeState| -> Vec<String> {
            state.matches(&info).into_iter().map(|i| info.models[i].name.clone()).collect()
        };
        // Paid models sit under their own provider, providers in name order.
        assert_eq!(names(&state), ["local", "zen-free", "or-free", "anthropic-paid", "or-paid"]);
        state.by_provider = true;
        assert_eq!(names(&state), ["local", "anthropic-paid", "zen-free", "or-free", "or-paid"]);
    }

    fn model(name: &str, size: f64, fit: GpuFit, tools: bool) -> WelcomeModel {
        WelcomeModel {
            name: name.to_owned(),
            size_gib: Some(size),
            family: None,
            parameters: None,
            quantization: None,
            context_limit: None,
            tools,
            vision: false,
            thinking: false,
            fit,
            loaded: false,
            provider_label: None,
            free: false,
            needs_key: false,
            price: None,
            kv_bytes_per_token: None,
            details: recode_core::model_facts::ModelFacts::default(),
        }
    }

    fn info(models: Vec<WelcomeModel>) -> WelcomeInfo {
        WelcomeInfo {
            provider_name: "x".to_owned(),
            provider: ProviderState::Ready { detail: "x".to_owned() },
            models,
            hardware: Vec::new(),
            free_vram_gib: None,
            last_model: None,
        }
    }

    #[test]
    fn models_likely_to_work_come_first() {
        let sorted = sort_models(vec![
            model("no-tools", 1.0, GpuFit::FitsGpu, false),
            model("big", 20.0, GpuFit::PartlyCpu { over_gib: 4.0 }, true),
            model("small", 2.0, GpuFit::FitsGpu, true),
            model("medium", 8.0, GpuFit::FitsGpu, true),
        ]);
        let names: Vec<_> = sorted.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["medium", "small", "big", "no-tools"]);
    }

    #[test]
    fn filtering_matches_letters_in_order_and_ignores_case() {
        let info =
            info(vec![model("gpt-oss:20b", 1.0, GpuFit::FitsGpu, true), model("llama3.2", 1.0, GpuFit::FitsGpu, true)]);
        let mut state = WelcomeState::new(Instant::now());
        state.set_filter("GOSS".to_owned(), Some(&info));
        assert_eq!(state.matches(&info), [0]);
        state.set_filter("lm3".to_owned(), Some(&info));
        assert_eq!(state.matches(&info), [1]);
        assert_eq!(state.selected, 1, "the selection moves to a model that matches");
        state.set_filter("zzz".to_owned(), Some(&info));
        assert!(state.matches(&info).is_empty());
    }

    #[test]
    fn selection_stops_at_the_ends() {
        let mut state = WelcomeState::new(Instant::now());
        let matches = [0, 2, 5];
        state.selected = 2;
        state.move_selection(&matches, 1);
        assert_eq!(state.selected, 5);
        state.move_selection(&matches, 10);
        assert_eq!(state.selected, 5);
        state.move_selection(&matches, -10);
        assert_eq!(state.selected, 0);
        state.move_selection(&[], 1);
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn a_column_is_shown_for_every_row_or_for_none() {
        for width in 1..120 {
            let row = |m: &WelcomeModel| model_row(m, false, width, false).to_string();
            let long = model("a-really-quite-long-model-name:latest", 16.5, GpuFit::FitsGpu, true);
            let short = model("x", 16.5, GpuFit::FitsGpu, true);
            assert_eq!(row(&long).contains("tools"), row(&short).contains("tools"), "width {width}");
            assert_eq!(row(&long).contains("GiB"), row(&short).contains("GiB"), "width {width}");
        }
    }

    #[test]
    fn the_intro_ends_so_an_idle_screen_uses_no_cpu() {
        let start = Instant::now();
        let state = WelcomeState::new(start);
        assert!(state.intro_running(start));
        assert!(!state.intro_running(start + INTRO));
    }
}
