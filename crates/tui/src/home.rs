//! The Home tab (what to pick up, and the free models available right now) and the Settings tab.

use crate::app::App;
use crate::theme::{self, Token, color};
use crate::widgets::chip;
use crate::wrap::{truncate, wrap_plain};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

/// One thing the Home tab offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DashItem {
    /// Continue a saved session (id, what to show).
    Resume(String, String),
    /// Start with this model.
    Model(String),
    GetFree,
    Theme,
    Settings,
}

/// The Home tab's rows, in order: sessions to continue, models to start with, then the rest.
#[must_use]
pub fn dashboard_rows(app: &App) -> Vec<DashItem> {
    let mut rows = Vec::new();
    for session in app.recent_sessions.iter().take(3) {
        let label = format!("{} · {} · {} messages", session.title, session.age, session.messages);
        rows.push(DashItem::Resume(session.id.clone(), label));
    }
    if let Some(info) = &app.welcome_info {
        let last = info.last_model.clone().or_else(|| (!app.model.is_empty()).then(|| app.model.clone()));
        if let Some(last) = last.filter(|name| info.models.iter().any(|m| &m.name == name)) {
            rows.push(DashItem::Model(last));
        }
        let free: Vec<&crate::welcome::WelcomeModel> =
            info.models.iter().filter(|m| m.free && m.tools && !m.needs_key).take(3).collect();
        if free.is_empty() {
            rows.push(DashItem::GetFree);
        } else {
            for model in free {
                rows.push(DashItem::Model(model.name.clone()));
            }
        }
    }
    rows.push(DashItem::Theme);
    rows.push(DashItem::Settings);
    rows
}

/// A section heading in the Home and Settings tabs.
fn heading(text: &str, width: usize) -> Line<'static> {
    let text = truncate(text, width.saturating_sub(2));
    let fill = width.saturating_sub(text.width() + 2);
    Line::from(vec![
        Span::styled(text, Style::new().fg(color(Token::Attention))),
        Span::styled(format!(" {}", "─".repeat(fill)), theme::faint()),
    ])
}

fn row(
    icon: &str,
    icon_color: ratatui::style::Color,
    text: &str,
    detail: &str,
    selected: bool,
    width: usize,
) -> Line<'static> {
    let base = if selected { Style::new().bg(color(Token::UserBg)).add_modifier(Modifier::BOLD) } else { Style::new() };
    let pointer = if selected { "❯ " } else { "  " };
    let room = width.saturating_sub(4);
    let detail = if room >= detail.width() + 16 { detail.to_owned() } else { String::new() };
    let text = truncate(text, room.saturating_sub(detail.width() + usize::from(!detail.is_empty())));
    let fill = room.saturating_sub(text.width() + detail.width());
    Line::from(vec![
        Span::styled(pointer, theme::accent().patch(base)),
        Span::styled(format!("{icon} "), Style::new().fg(icon_color).patch(base)),
        Span::styled(text, base),
        Span::styled(" ".repeat(fill), base),
        Span::styled(detail, theme::muted().patch(base)),
    ])
}

/// Draws the Home tab. Returns how many rows there are.
pub fn draw_home(frame: &mut Frame<'_>, area: Rect, app: &mut App, selected: usize) -> usize {
    let width = usize::from(area.width);
    let rows = dashboard_rows(app);
    if area.height == 0 || width < 8 {
        return rows.len();
    }
    let mut lines: Vec<(Line<'static>, Option<usize>)> = Vec::new();
    let mut section = None;
    for (index, item) in rows.iter().enumerate() {
        let (kind, line) = match item {
            DashItem::Resume(_, label) => {
                ("Continue", row("↩", color(Token::Sky), label, "", index == selected, width))
            }
            DashItem::Model(name) => {
                let free =
                    app.welcome_info.as_ref().is_some_and(|i| i.models.iter().any(|m| &m.name == name && m.free));
                let (icon, icon_color, detail) = if free {
                    ("★", color(Token::Success), "free")
                } else {
                    ("▶", color(Token::Accent), "last used")
                };
                ("Start", row(icon, icon_color, name, detail, index == selected, width))
            }
            DashItem::GetFree => {
                ("Start", row("★", color(Token::Success), "Get free models", "/free", index == selected, width))
            }
            DashItem::Theme => {
                ("More", row("◆", color(Token::Violet), "Change theme", "/theme", index == selected, width))
            }
            DashItem::Settings => ("More", row("⚙", color(Token::Muted), "Settings", "tab", index == selected, width)),
        };
        if section != Some(kind) {
            if section.is_some() {
                lines.push((Line::default(), None));
            }
            lines.push((heading(kind, width), None));
            section = Some(kind);
        }
        lines.push((line, Some(index)));
    }
    if app.recent_sessions.is_empty() && area.height >= 10 {
        lines.push((Line::default(), None));
        lines.extend(
            wrap_plain(
                "Sessions are saved as you go: this is where they'll be to pick up later.",
                theme::muted(),
                width,
            )
            .into_iter()
            .map(|line| (line, None)),
        );
    }
    // Keep the selected row in view on a short screen.
    let height = usize::from(area.height);
    let position = lines.iter().position(|(_, index)| *index == Some(selected)).unwrap_or(0);
    let first = (position + 1).saturating_sub(height).min(lines.len().saturating_sub(height));
    for (offset, (line, index)) in lines.into_iter().enumerate().skip(first).take(height) {
        let y = area.y + u16::try_from(offset - first).unwrap_or(0);
        frame.render_widget(Paragraph::new(line), Rect::new(area.x, y, area.width, 1));
        if let Some(index) = index {
            app.hits.rows.push((y, area.x, area.x + area.width, index));
        }
    }
    rows.len()
}

/// The sizes `Context size` steps through, in tokens.
pub const CONTEXT_CHOICES: [u32; 6] = [4096, 8192, 16384, 32768, 65536, 131_072];
pub const NOTIFICATION_CHOICES: [&str; 4] = ["off", "bell", "desktop", "both"];
pub const MEMORY_CHOICES: [&str; 3] = ["on", "read-only", "off"];
/// Minutes of quiet before an idle compaction, or off.
pub const IDLE_CHOICES: [&str; 5] = ["off", "5", "15", "30", "60"];

/// A row of the Settings tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingRow {
    pub label: &'static str,
    pub value: String,
    pub hint: &'static str,
    pub kind: SettingKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingKind {
    Theme,
    Variant,
    Accent,
    Background,
    ContextSize,
    /// Where automatic compaction starts, for the active model.
    CompactionTrigger,
    /// Tokens kept for the reply, for the active model.
    Reserve,
    /// Compact a conversation left alone for a while.
    IdleCompaction,
    Notifications,
    LineNumbers,
    /// Whether re-code keeps memory between sessions.
    Memory,
    /// An API key for this provider (preset name), and whether one is stored.
    ApiKey(String, bool),
}

#[must_use]
pub fn settings_rows(app: &App) -> Vec<SettingRow> {
    let locked = if app.theme_locked { " (set in config.toml)" } else { "" };
    let setting = |key: &str, default: &str| app.settings.get(key).cloned().unwrap_or_else(|| default.to_owned());
    let mut rows = vec![
        SettingRow {
            label: "Theme",
            value: format!("{}{locked}", app.theme.name),
            hint: "enter picks",
            kind: SettingKind::Theme,
        },
        SettingRow {
            label: "Variant",
            value: app.theme.variant.label().to_owned(),
            hint: "← → changes",
            kind: SettingKind::Variant,
        },
        SettingRow {
            label: "Accent",
            value: app.theme.accent.and_then(theme::hex).unwrap_or_else(|| "theme's own".to_owned()),
            hint: "← → changes",
            kind: SettingKind::Accent,
        },
        SettingRow {
            label: "Theme background",
            value: if app.theme.solid { "on".to_owned() } else { "off (terminal's)".to_owned() },
            hint: "enter toggles",
            kind: SettingKind::Background,
        },
        SettingRow {
            label: "Context size (local models)",
            value: format!("{} tokens", setting("num_ctx", "16384")),
            hint: "← → changes, applies on the next model switch",
            kind: SettingKind::ContextSize,
        },
        SettingRow {
            label: "Idle compaction",
            value: match setting("idle_compaction", "off").as_str() {
                "off" => "off".to_owned(),
                minutes => format!("after {minutes} min of quiet, when over half full"),
            },
            hint: "← → changes: off, 5, 15, 30, 60 minutes",
            kind: SettingKind::IdleCompaction,
        },
        SettingRow {
            label: "Notifications",
            value: setting("notifications", "both"),
            hint: "← → changes: off, bell, desktop, both",
            kind: SettingKind::Notifications,
        },
        SettingRow {
            label: "Memory",
            value: setting("memory", "on"),
            hint: "← → changes: on (re-code saves what it learns; /memory lists it), read-only, off",
            kind: SettingKind::Memory,
        },
        SettingRow {
            label: "Line numbers in code",
            value: setting("line_numbers", "off"),
            hint: "enter toggles",
            kind: SettingKind::LineNumbers,
        },
    ];
    // With a model active, its own window replaces the default row, and compaction becomes tunable.
    if let (Some(window), false) = (&app.context_window, app.model.is_empty()) {
        let setting = &app.context_setting;
        let describe = window.describe(setting);
        let mut parts = describe.split(" · ");
        let size = parts.next().unwrap_or_default().to_owned();
        let compaction = parts.next().unwrap_or_default().to_owned();
        let reserve = parts.next().unwrap_or_default().trim_start_matches("reserve ").to_owned();
        let local = window.local_num_ctx.is_some();
        let kv = match (app.kv_bytes_per_token, window.window) {
            (Some(bytes), Some(tokens)) => {
                #[allow(clippy::cast_precision_loss)]
                let gib = (bytes * tokens) as f64 / (1024.0 * 1024.0 * 1024.0);
                format!(" · KV ~{gib:.1} GiB")
            }
            _ => String::new(),
        };
        rows[4] = SettingRow {
            label: "Context size",
            value: format!("{} · {size}{kv}", app.model),
            hint: if local {
                "← → changes, a auto; the model reloads with the new size on its next request"
            } else {
                "← → changes, a auto; the listing's limit when auto"
            },
            kind: SettingKind::ContextSize,
        };
        rows.insert(
            5,
            SettingRow {
                label: "Compaction",
                value: compaction,
                hint: "← → changes: auto, 50–90 % of the window, off; a auto",
                kind: SettingKind::CompactionTrigger,
            },
        );
        rows.insert(
            6,
            SettingRow {
                label: "Reserved for the reply",
                value: reserve,
                hint: "← → changes, a auto",
                kind: SettingKind::Reserve,
            },
        );
    }
    for preset in recode_core::providers::PROVIDER_PRESETS {
        let stored = app.extensions.as_ref().is_some_and(|e| e.providers_with_keys.contains(preset.name));
        rows.push(SettingRow {
            label: preset.name,
            value: if stored { "key stored".to_owned() } else { "no key".to_owned() },
            hint: if stored { "enter replaces, d removes" } else { "enter adds a key" },
            kind: SettingKind::ApiKey(preset.name.to_owned(), stored),
        });
    }
    rows
}

/// Draws the Settings tab. Returns how many rows there are.
pub fn draw_settings(frame: &mut Frame<'_>, area: Rect, app: &mut App, selected: usize) -> usize {
    let rows = settings_rows(app);
    let width = usize::from(area.width);
    if area.height == 0 || width < 8 {
        return rows.len();
    }
    let mut lines: Vec<(Line<'static>, Option<usize>)> = Vec::new();
    let mut section = None;
    for (index, setting) in rows.iter().enumerate() {
        let kind =
            if matches!(setting.kind, SettingKind::ApiKey(..)) { "API keys" } else { "Appearance and behaviour" };
        if section != Some(kind) {
            if section.is_some() {
                lines.push((Line::default(), None));
            }
            lines.push((heading(kind, width), None));
            section = Some(kind);
        }
        let is_selected = index == selected;
        let base =
            if is_selected { Style::new().bg(color(Token::UserBg)).add_modifier(Modifier::BOLD) } else { Style::new() };
        let room = width.saturating_sub(4);
        let value = truncate(&setting.value, room / 2);
        let label = truncate(setting.label, room.saturating_sub(value.width() + 2));
        let fill = room.saturating_sub(label.width() + value.width());
        let value_style = match setting.kind {
            SettingKind::ApiKey(_, true) => Style::new().fg(color(Token::Success)),
            _ => theme::muted(),
        };
        lines.push((
            Line::from(vec![
                Span::styled(if is_selected { "❯ " } else { "  " }, theme::accent().patch(base)),
                Span::styled(label, base),
                Span::styled(" ".repeat(fill), base),
                Span::styled(value, value_style.patch(base)),
            ]),
            Some(index),
        ));
    }
    if let Some(setting) = rows.get(selected)
        && area.height >= 12
    {
        lines.push((Line::default(), None));
        lines.push((Line::from(vec![Span::raw("  "), chip(setting.hint, color(Token::Sky))]), None));
    }
    let height = usize::from(area.height);
    let position = lines.iter().position(|(_, index)| *index == Some(selected)).unwrap_or(0);
    let first = (position + 1).saturating_sub(height).min(lines.len().saturating_sub(height));
    for (offset, (line, index)) in lines.into_iter().enumerate().skip(first).take(height) {
        let y = area.y + u16::try_from(offset - first).unwrap_or(0);
        frame.render_widget(Paragraph::new(line), Rect::new(area.x, y, area.width, 1));
        if let Some(index) = index {
            app.hits.rows.push((y, area.x, area.x + area.width, index));
        }
    }
    rows.len()
}
