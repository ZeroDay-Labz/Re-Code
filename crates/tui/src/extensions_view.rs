//! The MCP, Skills and Tools tabs of the home screen.

use crate::app::App;
use crate::theme::{self, Token, color};
use crate::welcome::HomeTab;
use crate::wrap::{truncate, wrap_plain};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use recode_core::extensions::{
    CustomTool, ExtensionsSnapshot, McpProbe, McpServer, McpTransport, Plugin, PluginPart, ProviderProbe, Skill,
    SkillSource,
};
use recode_core::providers::CloudProvider;
use unicode_width::UnicodeWidthStr;

const DETAILS_WIDTH: u16 = 44;
const NOT_CONNECTED: &str = "A tool-capable model uses this when you're in a project folder. Pick one with Ctrl+L.";

struct Row {
    marker: &'static str,
    marker_style: Style,
    name: String,
    right: String,
    enabled: bool,
}

impl Row {
    fn blank() -> Self {
        Self { marker: " ", marker_style: Style::new(), name: String::new(), right: String::new(), enabled: false }
    }
}

fn server_row(server: &McpServer, extensions: &ExtensionsSnapshot) -> Row {
    let kind = match server.transport {
        McpTransport::Stdio { .. } => "stdio",
        McpTransport::Http { .. } => "http",
    };
    let unsupported = server.external.as_ref().and_then(|e| e.unsupported.as_ref());
    let (marker, marker_style, status) = if unsupported.is_some() {
        ("⊘", Style::new().fg(color(Token::Danger)), "unsupported".to_owned())
    } else if extensions.probing.contains(&server.name) {
        ("◌", theme::accent(), "testing…".to_owned())
    } else if !server.enabled {
        ("○", theme::muted(), "off".to_owned())
    } else {
        match extensions.probes.get(&server.name) {
            Some(McpProbe::Connected { tools, .. }) => {
                ("●", Style::new().fg(color(Token::Success)), format!("{} tools", tools.len()))
            }
            Some(McpProbe::Failed { .. }) => ("✗", Style::new().fg(color(Token::Danger)), "failed".to_owned()),
            // A stdio server starts only when the model first uses it; opening this tab doesn't run it.
            None if kind == "stdio" => ("◐", Style::new().fg(color(Token::Attention)), "starts on use".to_owned()),
            None => ("◐", Style::new().fg(color(Token::Attention)), "not tested".to_owned()),
        }
    };
    let source = match &server.external {
        Some(external) if external.label.starts_with("plugin ") => " · plugin".to_owned(),
        Some(external) if external.label.starts_with("OpenCode") => " · opencode".to_owned(),
        Some(_) => " · claude".to_owned(),
        None => String::new(),
    };
    let unsandboxed = matches!(server.transport, McpTransport::Stdio { .. })
        && server.sandbox == recode_core::extensions::McpSandbox::None;
    let sandbox = if unsandboxed { " · unsandboxed" } else { "" };
    Row {
        marker,
        marker_style,
        name: server.name.clone(),
        right: format!("{kind}{source}{sandbox}  {status}"),
        enabled: server.enabled && unsupported.is_none(),
    }
}

fn skill_row(skill: &Skill) -> Row {
    let source = match skill.source {
        SkillSource::User => "yours",
        SkillSource::Project => "project",
        SkillSource::ClaudeCode => "claude code",
        SkillSource::Plugin => "plugin",
    };
    let (marker, marker_style) = match (&skill.problem, skill.scan.blocks(), skill.enabled) {
        (_, true, true) => ("▲", Style::new().fg(color(Token::Attention))),
        (_, true, _) => ("⚠", Style::new().fg(color(Token::Danger))),
        (Some(_), _, _) => ("✗", Style::new().fg(color(Token::Danger))),
        (None, _, true) => ("●", Style::new().fg(color(Token::Success))),
        (None, _, false) => ("○", theme::muted()),
    };
    let source = match &skill.installed_from {
        Some(installed) => installed.pin_text(),
        None => source.to_owned(),
    };
    let right = match &skill.scan {
        recode_core::extensions::Verdict::Clean => source,
        verdict if skill.accepted => format!("{source} · {}, accepted", verdict.label()),
        verdict => format!("{source} · {}", verdict.label()),
    };
    Row { marker, marker_style, name: skill.name.clone(), right, enabled: skill.enabled }
}

/// What the Plugins tab lists: installed plugins, then followed marketplaces, then what Claude Code has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginRow {
    Plugin(usize),
    Marketplace(usize),
    Claude(usize),
}

#[must_use]
pub fn plugin_rows(extensions: &ExtensionsSnapshot) -> Vec<PluginRow> {
    let mut rows: Vec<PluginRow> = (0..extensions.plugins.len()).map(PluginRow::Plugin).collect();
    rows.extend((0..extensions.marketplaces.len()).map(PluginRow::Marketplace));
    rows.extend((0..extensions.claude_plugins.len()).map(PluginRow::Claude));
    rows
}

fn plugin_row(row: &PluginRow, extensions: &ExtensionsSnapshot) -> Row {
    match row {
        PluginRow::Plugin(index) => {
            let Some(plugin) = extensions.plugins.get(*index) else { return Row::blank() };
            let running = plugin.parts.iter().filter(|part| part.runs_here()).count();
            let (marker, marker_style) = if !plugin.enabled {
                ("○", theme::muted())
            } else if running == 0 {
                ("◌", theme::muted())
            } else {
                ("●", Style::new().fg(color(Token::Success)))
            };
            let brings = match running {
                0 => "nothing runs here".to_owned(),
                1 => "1 part".to_owned(),
                n => format!("{n} parts"),
            };
            Row {
                marker,
                marker_style,
                name: plugin.name.clone(),
                right: format!("{} · {brings}", plugin.source.pin_text()),
                enabled: plugin.enabled,
            }
        }
        PluginRow::Marketplace(index) => {
            let Some(market) = extensions.marketplaces.get(*index) else { return Row::blank() };
            Row {
                marker: "⌂",
                marker_style: Style::new().fg(color(Token::Attention)),
                name: market.name.clone(),
                right: format!("marketplace · {} plugins", market.plugin_count),
                enabled: true,
            }
        }
        PluginRow::Claude(index) => {
            let Some(info) = extensions.claude_plugins.get(*index) else { return Row::blank() };
            let version = info.version.as_deref().map(|v| format!(" {v}")).unwrap_or_default();
            Row {
                marker: "◇",
                marker_style: theme::muted(),
                name: info.key.clone(),
                right: format!("installed in Claude Code{version}"),
                enabled: false,
            }
        }
    }
}

fn plugin_details(row: &PluginRow, extensions: &ExtensionsSnapshot, width: usize) -> Vec<Line<'static>> {
    match row {
        PluginRow::Plugin(index) => {
            extensions.plugins.get(*index).map(|plugin| plugin_lines(plugin, width)).unwrap_or_default()
        }
        PluginRow::Marketplace(index) => {
            let Some(market) = extensions.marketplaces.get(*index) else { return Vec::new() };
            let mut lines = heading(&market.name, width);
            lines.extend(wrap_plain(&format!("A Claude Code marketplace at {}.", market.repo), Style::new(), width));
            if !market.sha.is_empty() {
                lines.extend(wrap_plain(
                    &format!("Manifest read at {}.", &market.sha[..8.min(market.sha.len())]),
                    theme::muted(),
                    width,
                ));
            }
            lines.extend(wrap_plain(
                &format!("{} plugins listed. Press b to browse them, d to stop following.", market.plugin_count),
                theme::muted(),
                width,
            ));
            lines
        }
        PluginRow::Claude(index) => {
            let Some(info) = extensions.claude_plugins.get(*index) else { return Vec::new() };
            let mut lines = heading(&info.key, width);
            lines.extend(wrap_plain(
                "Installed by Claude Code. Read-only here; its hooks never run.",
                Style::new(),
                width,
            ));
            lines.extend(wrap_plain("Press Enter to bring its skills and MCP servers into re-code (installed from its folder as a local plugin).", theme::muted(), width));
            lines.extend(wrap_plain(&info.install_path, theme::faint(), width));
            lines
        }
    }
}

fn plugin_lines(plugin: &Plugin, width: usize) -> Vec<Line<'static>> {
    let mut lines = heading(&plugin.name, width);
    if !plugin.description.is_empty() {
        lines.extend(wrap_plain(&plugin.description, Style::new(), width));
    }
    lines.extend(wrap_plain(&plugin.header(), theme::muted(), width));
    lines.extend(wrap_plain(&plugin.capability_sentence(), theme::muted(), width));
    if let Some(version) = &plugin.version {
        lines.extend(wrap_plain(&format!("Version {version}"), theme::muted(), width));
    }
    lines.push(Line::default());
    for part in plugin.parts.iter().take(12) {
        let style = if part.runs_here() { Style::new() } else { theme::muted() };
        let mark = if part.runs_here() { "• " } else { "· " };
        lines.extend(wrap_plain(&format!("{mark}{}", part.describe()), style, width));
    }
    if plugin.parts.len() > 12 {
        lines.extend(wrap_plain(&format!("…and {} more.", plugin.parts.len() - 12), theme::muted(), width));
    }
    if plugin.parts.iter().any(|part| matches!(part, PluginPart::Hook { converted: true, .. })) {
        lines.extend(wrap_plain(
            "Converted hooks are in hooks.converted.toml inside the plugin; copy what you want into your hooks.toml.",
            theme::muted(),
            width,
        ));
    }
    lines.push(Line::default());
    lines.extend(wrap_plain(&plugin.directory, theme::faint(), width));
    lines.extend(wrap_plain(
        "Space switches it on or off, u checks for an update, d removes it.",
        theme::muted(),
        width,
    ));
    lines
}

fn tool_row(tool: &CustomTool) -> Row {
    let (marker, marker_style) =
        if tool.enabled { ("●", Style::new().fg(color(Token::Success))) } else { ("○", theme::muted()) };
    let count = tool.parameters.len();
    let right = match count {
        0 => "no parameters".to_owned(),
        1 => "1 parameter".to_owned(),
        _ => format!("{count} parameters"),
    };
    Row { marker, marker_style, name: tool.name.clone(), right, enabled: tool.enabled }
}

fn provider_row(provider: &CloudProvider, extensions: &ExtensionsSnapshot) -> Row {
    let (marker, marker_style, status) = if !provider.enabled {
        ("○", theme::muted(), "off".to_owned())
    } else {
        match extensions.provider_probes.get(&provider.name) {
            Some(ProviderProbe::Reachable { model_count, free_model_count, .. }) => {
                let free = if *free_model_count > 0 { format!("  {free_model_count} free") } else { String::new() };
                ("●", Style::new().fg(color(Token::Success)), format!("{model_count} models{free}"))
            }
            Some(ProviderProbe::Failed { .. }) => ("✗", Style::new().fg(color(Token::Danger)), "failed".to_owned()),
            Some(ProviderProbe::NoKey) | None => {
                let has_key = extensions.providers_with_keys.contains(&provider.name);
                if has_key {
                    ("◐", Style::new().fg(color(Token::Attention)), "not tested".to_owned())
                } else {
                    ("◐", Style::new().fg(color(Token::Attention)), "needs a key".to_owned())
                }
            }
        }
    };
    let right = format!("{}  {status}", provider.kind.label());
    Row { marker, marker_style, name: provider.name.clone(), right, enabled: provider.enabled }
}

fn provider_details(provider: &CloudProvider, extensions: &ExtensionsSnapshot, width: usize) -> Vec<Line<'static>> {
    let mut lines = heading(&provider.name, width);
    lines.push(label("Kind"));
    lines.extend(wrap_plain(provider.kind.label(), Style::new(), width));
    lines.push(label("Address"));
    lines.extend(wrap_plain(
        &provider.base_url().unwrap_or_else(|| "(needs a base URL)".to_owned()),
        Style::new(),
        width,
    ));
    lines.push(Line::default());
    if extensions.providers_with_keys.contains(&provider.name) {
        lines.extend(wrap_plain("API key stored in your keyring.", Style::new().fg(color(Token::Success)), width));
    } else {
        lines.extend(wrap_plain("No API key yet. Press e to add one.", theme::muted(), width));
    }
    lines.push(Line::default());
    match extensions.provider_probes.get(&provider.name) {
        Some(ProviderProbe::Reachable { model_count, free_model_count, note }) => {
            let free = if *free_model_count > 0 { format!(", {free_model_count} free") } else { String::new() };
            lines.extend(wrap_plain(
                &format!("Connected: {model_count} models{free}."),
                Style::new().fg(color(Token::Success)),
                width,
            ));
            if let Some(note) = note {
                lines.extend(wrap_plain(note, Style::new(), width));
            }
            lines.extend(wrap_plain("Press Ctrl+R to add them to the model list.", theme::muted(), width));
        }
        Some(ProviderProbe::Failed { error }) => lines.extend(wrap_plain(
            &format!("Last test failed: {error}"),
            Style::new().fg(color(Token::Danger)),
            width,
        )),
        Some(ProviderProbe::NoKey) => {
            lines.extend(wrap_plain("No key stored, so it can't be used.", theme::muted(), width))
        }
        None => lines.extend(wrap_plain("Not tested yet. Press t to connect.", theme::muted(), width)),
    }
    lines.push(Line::default());
    lines.extend(wrap_plain(
        "Pick a model from this provider on the Models tab once it's connected.",
        theme::muted(),
        width,
    ));
    lines
}

/// Draws a tab's contents.
pub fn draw(frame: &mut Frame<'_>, area: Rect, app: &mut App, tab: HomeTab, selected: usize) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let Some(extensions) = app.extensions.clone() else {
        let text = truncate("Loading…", usize::from(area.width));
        frame.render_widget(Paragraph::new(Line::styled(text, theme::muted())), area);
        return;
    };
    let rows: Vec<Row> = match tab {
        HomeTab::Providers => extensions.providers.iter().map(|provider| provider_row(provider, &extensions)).collect(),
        HomeTab::Mcp => extensions.servers.iter().map(|server| server_row(server, &extensions)).collect(),
        HomeTab::Skills => extensions.skills.iter().map(skill_row).collect(),
        HomeTab::Plugins => plugin_rows(&extensions).iter().map(|row| plugin_row(row, &extensions)).collect(),
        HomeTab::Tools => extensions.tools.iter().map(tool_row).collect(),
        HomeTab::Models | HomeTab::Home | HomeTab::Settings | HomeTab::Runtimes => return,
    };
    if rows.is_empty() {
        draw_empty(frame, area, tab);
        return;
    }
    let show_details = area.width >= DETAILS_WIDTH + 30 && area.height >= 5;
    let pane = crate::welcome::detail_width(area.width, DETAILS_WIDTH, app);
    let list = if show_details { Rect::new(area.x, area.y, area.width - pane - 3, area.height) } else { area };
    draw_rows(frame, list, &rows, selected, app);
    if show_details {
        let details = Rect::new(list.x + list.width + 3, area.y, pane, area.height);
        let rule: Vec<Line<'_>> = (0..details.height).map(|_| Line::styled("│", theme::faint())).collect();
        frame.render_widget(Paragraph::new(rule), Rect::new(details.x - 2, details.y, 1, details.height));
        let lines = detail_lines(tab, selected, &extensions, usize::from(details.width));
        frame.render_widget(Paragraph::new(lines.unwrap_or_default()), details);
    }
}

/// Everything the detail pane says about the selected row, wrapped to `width`; `v` reads it in the pager.
#[must_use]
pub fn detail_lines(
    tab: HomeTab,
    selected: usize,
    extensions: &ExtensionsSnapshot,
    width: usize,
) -> Option<Vec<Line<'static>>> {
    match tab {
        HomeTab::Providers => {
            extensions.providers.get(selected).map(|provider| provider_details(provider, extensions, width))
        }
        HomeTab::Mcp => extensions.servers.get(selected).map(|server| server_details(server, extensions, width)),
        HomeTab::Skills => extensions.skills.get(selected).map(|skill| skill_details(skill, width)),
        HomeTab::Tools => extensions.tools.get(selected).map(|tool| tool_details(tool, width)),
        HomeTab::Plugins => plugin_rows(extensions).get(selected).map(|row| plugin_details(row, extensions, width)),
        HomeTab::Models | HomeTab::Home | HomeTab::Settings | HomeTab::Runtimes => None,
    }
}

/// The lines as plain text, one per line, for the pager.
#[must_use]
pub fn plain_text(lines: &[Line<'_>]) -> String {
    lines
        .iter()
        .map(|line| line.spans.iter().map(|span| span.content.as_ref()).collect::<String>().trim_end().to_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

fn draw_empty(frame: &mut Frame<'_>, area: Rect, tab: HomeTab) {
    let width = usize::from(area.width);
    let (headline, body) = match tab {
        HomeTab::Providers => (
            "No model providers yet.",
            "Press a to add Anthropic, OpenAI, Google Gemini, OpenRouter (for free models), or any OpenAI-compatible server. Keys are kept in your system keyring.",
        ),
        HomeTab::Mcp => (
            "No MCP servers yet.",
            "Press a to add one: a program to run, or an http(s):// address such as your JetBrains IDE's MCP server. Servers you set up in Claude Code appear here automatically.",
        ),
        HomeTab::Skills => (
            "No skills yet.",
            "Press a to create one. A skill is a set of instructions that a model loads only when they're relevant.",
        ),
        HomeTab::Plugins => (
            "No plugins yet.",
            "Press i to install one from the Hermes catalog (a name such as better-session-appearance), a Claude Code marketplace (m adds one, b browses it), GitHub, or a folder. A plugin's skills and MCP servers work here; its Python code, desktop parts and hooks are listed, not run.",
        ),
        _ => ("No custom tools yet.", "Press a to add one. A custom tool lets a model run a command that you define."),
    };
    let mut lines = wrap_plain(headline, Style::new().add_modifier(Modifier::BOLD), width);
    lines.push(Line::default());
    lines.extend(wrap_plain(body, theme::muted(), width));
    frame.render_widget(Paragraph::new(lines), area);
}

fn draw_rows(frame: &mut Frame<'_>, area: Rect, rows: &[Row], selected: usize, app: &mut App) {
    let height = usize::from(area.height);
    let width = usize::from(area.width);
    let first = (selected + 1)
        .saturating_sub(height)
        .max(selected.saturating_sub(height / 2))
        .min(rows.len().saturating_sub(height));
    for (index, row) in rows.iter().enumerate().skip(first).take(height) {
        let y = area.y + u16::try_from(index - first).unwrap_or(0);
        let is_selected = index == selected;
        let base =
            if is_selected { Style::new().bg(color(Token::UserBg)).add_modifier(Modifier::BOLD) } else { Style::new() };
        let text = if row.enabled { base } else { theme::muted().patch(base) };
        let room = width.saturating_sub(4);
        let show_right = room >= row.right.width() + 12;
        let right = if show_right { format!("  {}", row.right) } else { String::new() };
        let name = truncate(&row.name, room.saturating_sub(right.width()));
        let fill = room.saturating_sub(name.width() + right.width());
        let line = Line::from(vec![
            Span::styled(if is_selected { "❯ " } else { "  " }, theme::accent().patch(base)),
            Span::styled(format!("{} ", row.marker), row.marker_style.patch(base)),
            Span::styled(name, text),
            Span::styled(" ".repeat(fill), base),
            Span::styled(right, theme::muted().patch(base)),
        ]);
        frame.render_widget(Paragraph::new(line), Rect::new(area.x, y, area.width, 1));
        app.hits.rows.push((y, area.x, area.x + area.width, index));
    }
}

fn heading(text: &str, width: usize) -> Vec<Line<'static>> {
    wrap_plain(text, Style::new().add_modifier(Modifier::BOLD), width)
}

fn label(text: &str) -> Line<'static> {
    Line::styled(text.to_owned(), Style::new().fg(theme::color(Token::Attention)))
}

fn compact_count(value: u64) -> String {
    if value < 1000 {
        return format!("~{value}");
    }
    #[allow(clippy::cast_precision_loss)]
    let thousands = value as f64 / 1000.0;
    format!("~{thousands:.1}k")
}

fn server_details(server: &McpServer, extensions: &ExtensionsSnapshot, width: usize) -> Vec<Line<'static>> {
    let mut lines = heading(&server.name, width);
    if let Some(external) = &server.external {
        let text = format!(
            "From {}, read live, so changes there show up here. Space turns it on or off and x changes how its \
             tools are offered; those choices are saved in re-code. Press i to copy it into re-code's own list.",
            external.label
        );
        lines.extend(wrap_plain(&text, theme::muted(), width));
        if let Some(reason) = &external.unsupported {
            lines.extend(wrap_plain(&format!("Can't be used: {reason}"), Style::new().fg(color(Token::Danger)), width));
        }
        lines.push(Line::default());
    }
    lines.push(label(match server.transport {
        McpTransport::Stdio { .. } => "Runs",
        McpTransport::Http { .. } => "Address",
    }));
    lines.extend(wrap_plain(&server.connection_text(), Style::new(), width));
    let settings = server.masked_settings();
    if !settings.is_empty() {
        lines.push(label(match server.transport {
            McpTransport::Stdio { .. } => "Environment",
            McpTransport::Http { .. } => "Headers",
        }));
        for setting in settings {
            lines.extend(wrap_plain(&setting, theme::muted(), width));
        }
    }
    lines.push(Line::default());
    let exposure = if server.expose_to_agent {
        "Exposed to the agent directly: its tool schemas go to the model every request. Best for small servers. Press x for discover-then-call."
    } else {
        "Discover-then-call (default): the agent lists this server's tools, then calls one, so no schemas are sent until needed. Press x to expose it directly."
    };
    let exposure_style = if server.expose_to_agent { Style::new().fg(color(Token::Success)) } else { theme::muted() };
    lines.extend(wrap_plain(exposure, exposure_style, width));
    let filter = match &server.tools {
        Some(filter) if !filter.include.is_empty() => {
            format!("Tool filter: only {} of its tools are offered. Press f to change.", filter.include.len())
        }
        Some(filter) => format!("Tool filter: {} of its tools are hidden. Press f to change.", filter.exclude.len()),
        None => "All of its tools are offered. Press f to choose some.".to_owned(),
    };
    lines.extend(wrap_plain(&filter, theme::muted(), width));
    lines.push(Line::default());
    match extensions.probes.get(&server.name) {
        Some(McpProbe::Connected { server_name, server_version, tools, estimated_schema_tokens }) => {
            let identity =
                [server_name.clone(), server_version.clone()].into_iter().flatten().collect::<Vec<_>>().join(" ");
            let text = format!("Connected to {identity}, offering {} tools.", tools.len());
            lines.extend(wrap_plain(&text, Style::new().fg(color(Token::Success)), width));
            let cost = format!(
                "Sending all their schemas would cost about {} tokens on every request (estimated), so re-code will load them only on demand.",
                compact_count(*estimated_schema_tokens)
            );
            lines.extend(wrap_plain(&cost, theme::muted(), width));
            lines.push(Line::default());
            let names: Vec<&str> = tools.iter().take(12).map(|tool| tool.name.as_str()).collect();
            let mut list = names.join(", ");
            if tools.len() > names.len() {
                list.push_str(", …");
            }
            lines.extend(wrap_plain(&list, theme::muted(), width));
        }
        Some(McpProbe::Failed { error }) => {
            lines.extend(wrap_plain(
                &format!("Last test failed: {error}"),
                Style::new().fg(color(Token::Danger)),
                width,
            ));
        }
        None => {
            let text = match server.transport {
                McpTransport::Stdio { .. } => {
                    "Not started yet: it starts when the model first uses it. Press t to start it now and see its tools."
                }
                McpTransport::Http { .. } => "Not tested yet. Press t to connect.",
            };
            lines.extend(wrap_plain(text, theme::muted(), width));
        }
    }
    lines.push(Line::default());
    lines.extend(wrap_plain(NOT_CONNECTED, theme::muted(), width));
    lines
}

fn skill_details(skill: &Skill, width: usize) -> Vec<Line<'static>> {
    let mut lines = heading(&skill.name, width);
    if !skill.description.is_empty() {
        lines.extend(wrap_plain(&skill.description, Style::new(), width));
    }
    if let Some(problem) = &skill.problem {
        lines.extend(wrap_plain(problem, Style::new().fg(color(Token::Danger)), width));
    }
    lines.push(Line::default());
    if let Some(installed) = &skill.installed_from {
        let pin = if installed.sha.len() >= 8 { format!(" at {}", &installed.sha[..8]) } else { String::new() };
        let from = if installed.path.is_empty() {
            installed.repo.clone()
        } else {
            format!("{}, folder {}", installed.repo, installed.path)
        };
        lines.extend(wrap_plain(
            &format!("Installed from {} ({from}{pin}). Press u to check for an update.", installed.tap),
            theme::muted(),
            width,
        ));
    }
    let source = match (skill.source, skill.trusted) {
        (SkillSource::User, _) => "Yours. Press e to edit it.",
        (SkillSource::Project, false) => {
            "From this project. It stays off until you trust this folder's configuration (/trust), as a repository could carry anything."
        }
        (SkillSource::Project, true) => "From this project (trusted).",
        (SkillSource::ClaudeCode, _) => "Found in Claude Code's directory. Read-only here.",
        (SkillSource::Plugin, _) => "Part of an installed plugin (Plugins tab). Removed with it.",
    };
    lines.extend(wrap_plain(source, theme::muted(), width));
    if let Some(license) = &skill.license {
        lines.extend(wrap_plain(&format!("Licence: {license}"), theme::muted(), width));
    }
    if let Some(compatibility) = &skill.compatibility {
        lines.extend(wrap_plain(&format!("Needs: {compatibility}"), theme::muted(), width));
    }
    if !skill.allowed_tools.is_empty() {
        lines.extend(wrap_plain(&format!("Asks for tools: {}", skill.allowed_tools.join(" ")), theme::muted(), width));
    }
    let findings = skill.scan.findings();
    if !findings.is_empty() {
        lines.push(Line::default());
        let headline = if skill.scan.blocks() && skill.accepted {
            "Scan: dangerous. You accepted these findings, so it's enabled on your word; it goes off again if they change."
        } else if skill.scan.blocks() {
            "Scan: dangerous. Space shows these findings and asks whether to enable it on your word."
        } else {
            "Scan: caution. Read these before enabling it."
        };
        lines.extend(wrap_plain(headline, Style::new().fg(color(Token::Danger)), width));
        for finding in findings.iter().take(6) {
            let place =
                if finding.line > 0 { format!("{}:{}", finding.file, finding.line) } else { finding.file.clone() };
            lines.extend(wrap_plain(&format!("{place}: {}", finding.reason), Style::new(), width));
            if !finding.excerpt.is_empty() {
                lines.extend(wrap_plain(&format!("  {}", finding.excerpt), theme::muted(), width));
            }
        }
        if findings.len() > 6 {
            lines.extend(wrap_plain(&format!("…and {} more.", findings.len() - 6), theme::muted(), width));
        }
    }
    lines.extend(wrap_plain(
        &format!("About {} tokens when loaded (estimated).", compact_count(skill.estimated_tokens)),
        theme::muted(),
        width,
    ));
    lines.extend(wrap_plain(&skill.path, theme::faint(), width));
    lines.push(Line::default());
    lines.extend(wrap_plain(NOT_CONNECTED, theme::muted(), width));
    lines
}

fn tool_details(tool: &CustomTool, width: usize) -> Vec<Line<'static>> {
    let mut lines = heading(&tool.name, width);
    lines.extend(wrap_plain(&tool.description, Style::new(), width));
    lines.push(label("Runs"));
    lines.extend(wrap_plain(&tool.command, Style::new(), width));
    if !tool.parameters.is_empty() {
        lines.push(label("Parameters"));
        lines.extend(wrap_plain(&tool.parameters.join(", "), theme::muted(), width));
    }
    lines.push(Line::default());
    lines.extend(wrap_plain(NOT_CONNECTED, theme::muted(), width));
    lines
}
