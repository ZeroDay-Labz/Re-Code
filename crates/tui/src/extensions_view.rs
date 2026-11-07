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
    CustomTool, ExtensionsSnapshot, McpProbe, McpServer, McpTransport, ProviderProbe, Skill, SkillSource,
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
    let source = if server.external.is_some() { " · claude" } else { "" };
    Row {
        marker,
        marker_style,
        name: server.name.clone(),
        right: format!("{kind}{source}  {status}"),
        enabled: server.enabled && unsupported.is_none(),
    }
}

fn skill_row(skill: &Skill) -> Row {
    let source = match skill.source {
        SkillSource::User => "yours",
        SkillSource::Project => "project",
        SkillSource::ClaudeCode => "claude code",
    };
    let (marker, marker_style) = match (&skill.problem, skill.scan.blocks(), skill.enabled) {
        (_, true, _) => ("⚠", Style::new().fg(color(Token::Danger))),
        (Some(_), _, _) => ("✗", Style::new().fg(color(Token::Danger))),
        (None, _, true) => ("●", Style::new().fg(color(Token::Success))),
        (None, _, false) => ("○", theme::muted()),
    };
    let right = match &skill.scan {
        recode_core::extensions::Verdict::Clean => source.to_owned(),
        verdict => format!("{source} · {}", verdict.label()),
    };
    Row { marker, marker_style, name: skill.name.clone(), right, enabled: skill.enabled }
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
        HomeTab::Tools => extensions.tools.iter().map(tool_row).collect(),
        HomeTab::Models | HomeTab::Home | HomeTab::Settings | HomeTab::Runtimes => return,
    };
    if rows.is_empty() {
        draw_empty(frame, area, tab);
        return;
    }
    let show_details = area.width >= DETAILS_WIDTH + 30 && area.height >= 5;
    let list = if show_details { Rect::new(area.x, area.y, area.width - DETAILS_WIDTH - 3, area.height) } else { area };
    draw_rows(frame, list, &rows, selected, app);
    if show_details {
        let details = Rect::new(list.x + list.width + 3, area.y, DETAILS_WIDTH, area.height);
        let rule: Vec<Line<'_>> = (0..details.height).map(|_| Line::styled("│", theme::faint())).collect();
        frame.render_widget(Paragraph::new(rule), Rect::new(details.x - 2, details.y, 1, details.height));
        let width = usize::from(details.width);
        let lines = match tab {
            HomeTab::Providers => {
                extensions.providers.get(selected).map(|provider| provider_details(provider, &extensions, width))
            }
            HomeTab::Mcp => extensions.servers.get(selected).map(|server| server_details(server, &extensions, width)),
            HomeTab::Skills => extensions.skills.get(selected).map(|skill| skill_details(skill, width)),
            HomeTab::Tools => extensions.tools.get(selected).map(|tool| tool_details(tool, width)),
            HomeTab::Models | HomeTab::Home | HomeTab::Settings | HomeTab::Runtimes => None,
        };
        frame.render_widget(Paragraph::new(lines.unwrap_or_default()), details);
    }
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
    let source = match (skill.source, skill.trusted) {
        (SkillSource::User, _) => "Yours. Press e to edit it.",
        (SkillSource::Project, false) => {
            "From this project. It stays off until you trust this folder's configuration (/trust), as a repository could carry anything."
        }
        (SkillSource::Project, true) => "From this project (trusted).",
        (SkillSource::ClaudeCode, _) => "Found in Claude Code's directory. Read-only here.",
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
        let headline = if skill.scan.blocks() {
            "Scan: dangerous. It can't be enabled."
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
