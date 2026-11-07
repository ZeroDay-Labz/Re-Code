//! The Runtimes tab: what runs models on this machine, how each is served, and the containers
//! around it. Enter on a stopped runtime offers to start it, showing the exact command first.

use crate::app::App;
use crate::link::{ContainerRow, RuntimeRow, RuntimeRowState, RuntimesInfo};
use crate::theme::{self, Token, color};
use crate::widgets::chip;
use crate::wrap::{truncate, wrap_plain};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

const DETAILS_WIDTH: u16 = 40;

/// A selectable row of the tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimesRow<'a> {
    Runtime(&'a RuntimeRow),
    Container(&'a ContainerRow),
}

/// The selectable rows, in display order: runtimes, then containers grouped by pod.
#[must_use]
pub fn rows(info: &RuntimesInfo) -> Vec<RuntimesRow<'_>> {
    let mut rows: Vec<RuntimesRow<'_>> = info.runtimes.iter().map(RuntimesRow::Runtime).collect();
    for pod in pod_order(info) {
        rows.extend(info.containers.iter().filter(|c| c.pod.as_deref() == pod).map(RuntimesRow::Container));
    }
    rows
}

/// Standalone containers first, then each pod, in the order the engine listed them.
fn pod_order(info: &RuntimesInfo) -> Vec<Option<&str>> {
    let mut pods: Vec<Option<&str>> = vec![None];
    for container in &info.containers {
        if let Some(pod) = container.pod.as_deref()
            && !pods.contains(&Some(pod))
        {
            pods.push(Some(pod));
        }
    }
    pods
}

/// Draws the tab. Returns the number of selectable rows.
pub fn draw(frame: &mut Frame<'_>, area: Rect, app: &mut App, selected: usize) -> usize {
    let Some(info) = app.runtimes.clone() else {
        frame.render_widget(Paragraph::new(Line::styled(" Looking for runtimes…", theme::muted())), area);
        return 0;
    };
    let all = rows(&info);
    if area.height == 0 || area.width < 8 {
        return all.len();
    }
    if all.is_empty() {
        let text = wrap_plain(
            "No inference runtime or container found. If Ollama runs on another machine, set its address with \
             --ollama-url or in the config file. Ctrl+R looks again.",
            theme::muted(),
            usize::from(area.width).saturating_sub(2),
        );
        frame.render_widget(
            Paragraph::new(text),
            Rect::new(area.x + 1, area.y, area.width.saturating_sub(1), area.height),
        );
        return 0;
    }
    let show_details = area.width >= DETAILS_WIDTH + 36 && area.height >= 6;
    let list = if show_details { Rect::new(area.x, area.y, area.width - DETAILS_WIDTH - 3, area.height) } else { area };
    draw_list(frame, list, app, &info, &all, selected);
    if show_details {
        let details = Rect::new(list.x + list.width + 3, area.y, DETAILS_WIDTH, area.height);
        let rule: Vec<Line<'_>> = (0..details.height).map(|_| Line::styled("│", theme::faint())).collect();
        frame.render_widget(Paragraph::new(rule), Rect::new(details.x - 2, details.y, 1, details.height));
        let lines = match all.get(selected.min(all.len().saturating_sub(1))) {
            Some(RuntimesRow::Runtime(runtime)) => runtime_details(runtime, app, usize::from(details.width)),
            Some(RuntimesRow::Container(container)) => container_details(container, usize::from(details.width)),
            None => Vec::new(),
        };
        frame.render_widget(Paragraph::new(lines), details);
    }
    all.len()
}

/// One line per row, with headings for the engines, the containers and each pod.
fn draw_list(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    info: &RuntimesInfo,
    all: &[RuntimesRow<'_>],
    selected: usize,
) {
    let width = usize::from(area.width);
    let mut lines: Vec<(Line<'static>, Option<usize>)> = Vec::new();
    if !info.engines.is_empty() {
        lines.push((Line::styled(format!(" {}", info.engines.join(" · ")), theme::muted()), None));
    }
    let mut index = 0;
    if !info.runtimes.is_empty() {
        lines.push((heading("Runtimes", width), None));
        for runtime in &info.runtimes {
            lines.push((runtime_line(runtime, app, index == selected, width), Some(index)));
            index += 1;
        }
    }
    if !info.containers.is_empty() {
        lines.push((heading("Containers", width), None));
        for pod in pod_order(info) {
            let members: Vec<&ContainerRow> = info.containers.iter().filter(|c| c.pod.as_deref() == pod).collect();
            if members.is_empty() {
                continue;
            }
            let indent = if let Some(pod) = pod {
                let ports = info.pods.iter().find(|p| p.name == pod).map(|p| ports_text(&p.ports)).unwrap_or_default();
                lines.push((
                    Line::from(vec![
                        Span::styled(" pod ", theme::muted()),
                        Span::styled(pod.to_owned(), Style::new().fg(color(Token::Sky))),
                        Span::styled(ports, theme::muted()),
                    ]),
                    None,
                ));
                "   "
            } else {
                " "
            };
            for container in members {
                lines.push((container_line(container, indent, index == selected, width), Some(index)));
                index += 1;
            }
        }
    }
    for unknown in &info.unidentified {
        lines.push((Line::styled(truncate(&format!(" ? {unknown}"), width), theme::muted()), None));
    }
    for warning in &info.warnings {
        lines.extend(
            wrap_plain(&format!("! {warning}"), Style::new().fg(color(Token::Attention)), width)
                .into_iter()
                .map(|l| (l, None)),
        );
    }
    for problem in &info.problems {
        lines.extend(
            wrap_plain(&format!("couldn't check: {problem}"), theme::muted(), width).into_iter().map(|l| (l, None)),
        );
    }
    let _ = all;
    // Keep the selected row in view on a short screen.
    let height = usize::from(area.height);
    let position = lines.iter().position(|(_, i)| *i == Some(selected)).unwrap_or(0);
    let first = (position + 1).saturating_sub(height).min(lines.len().saturating_sub(height));
    for (offset, (line, index)) in lines.into_iter().enumerate().skip(first).take(height) {
        let y = area.y + u16::try_from(offset - first).unwrap_or(0);
        frame.render_widget(Paragraph::new(line), Rect::new(area.x, y, area.width, 1));
        if let Some(index) = index {
            app.hits.rows.push((y, area.x, area.x + area.width, index));
        }
    }
}

fn heading(text: &str, width: usize) -> Line<'static> {
    let fill = width.saturating_sub(text.width() + 3);
    Line::from(vec![
        Span::styled(format!(" {text} "), Style::new().fg(color(Token::Attention))),
        Span::styled("─".repeat(fill), theme::faint()),
    ])
}

fn ports_text(ports: &[u16]) -> String {
    if ports.is_empty() {
        String::new()
    } else {
        format!("  {}", ports.iter().map(|p| format!(":{p}")).collect::<Vec<_>>().join(" "))
    }
}

/// The glyph and words for a runtime's state.
#[must_use]
pub fn state_text(runtime: &RuntimeRow, app: &App) -> (&'static str, ratatui::style::Color, String) {
    if let Some((id, phase)) = &app.runtime_starting
        && *id == runtime.id
    {
        return ("◌", color(Token::Attention), phase.clone());
    }
    match &runtime.state {
        RuntimeRowState::Running { version } => (
            "●",
            color(Token::Success),
            format!("running{}", version.as_deref().map(|v| format!(" {v}")).unwrap_or_default()),
        ),
        RuntimeRowState::Stopped { start_command: Some(_), .. } => {
            ("◐", color(Token::Attention), "stopped · enter starts".to_owned())
        }
        RuntimeRowState::Stopped { start_command: None, .. } => {
            ("◐", color(Token::Muted), "installed, stopped".to_owned())
        }
        RuntimeRowState::NotAnswering { .. } => ("✗", color(Token::Danger), "active, not answering".to_owned()),
    }
}

fn runtime_line(runtime: &RuntimeRow, app: &App, selected: bool, width: usize) -> Line<'static> {
    let base = if selected { Style::new().bg(color(Token::UserBg)).add_modifier(Modifier::BOLD) } else { Style::new() };
    let (glyph, glyph_color, state) = state_text(runtime, app);
    let detail = runtime.url.clone().unwrap_or_default();
    let name = if runtime.supported { runtime.name.clone() } else { format!("{} (detected)", runtime.name) };
    let room = width.saturating_sub(5);
    let detail = if room >= name.width() + state.width() + detail.width() + 6 { detail } else { String::new() };
    let left = format!("{name}  {state}");
    let left = truncate(&left, room.saturating_sub(detail.width() + usize::from(!detail.is_empty())));
    let fill = room.saturating_sub(left.width() + detail.width());
    Line::from(vec![
        Span::styled(if selected { "❯ " } else { "  " }, theme::accent().patch(base)),
        Span::styled(format!("{glyph} "), Style::new().fg(glyph_color).patch(base)),
        Span::styled(left, base),
        Span::styled(" ".repeat(fill), base),
        Span::styled(detail, theme::muted().patch(base)),
    ])
}

fn container_line(container: &ContainerRow, indent: &str, selected: bool, width: usize) -> Line<'static> {
    let base = if selected { Style::new().bg(color(Token::UserBg)).add_modifier(Modifier::BOLD) } else { Style::new() };
    let (glyph, glyph_color) =
        if container.running { ("●", color(Token::Success)) } else { ("○", color(Token::Muted)) };
    let runs = container.runs.as_deref().map(|r| format!(" {r}")).unwrap_or_default();
    let detail = format!(
        "{}{}",
        container.status,
        if container.pod.is_none() { ports_text(&container.ports) } else { String::new() }
    );
    let room = width.saturating_sub(indent.width() + 4);
    let detail =
        if room >= container.name.width() + detail.width() + runs.width() + 4 { detail } else { String::new() };
    let left = truncate(
        &format!("{}{runs}", container.name),
        room.saturating_sub(detail.width() + usize::from(!detail.is_empty())),
    );
    let fill = room.saturating_sub(left.width() + detail.width());
    Line::from(vec![
        Span::styled(if selected { "❯" } else { " " }, theme::accent().patch(base)),
        Span::raw(indent.to_owned()),
        Span::styled(format!("{glyph} "), Style::new().fg(glyph_color).patch(base)),
        Span::styled(left, base),
        Span::styled(" ".repeat(fill), base),
        Span::styled(detail, theme::muted().patch(base)),
    ])
}

fn runtime_details(runtime: &RuntimeRow, app: &App, width: usize) -> Vec<Line<'static>> {
    let mut lines = vec![Line::styled(runtime.name.clone(), Style::new().add_modifier(Modifier::BOLD))];
    let (_, state_color, state) = state_text(runtime, app);
    lines.push(Line::from(vec![chip(state, state_color)]));
    lines.push(Line::default());
    if let Some(url) = &runtime.url {
        lines.extend(wrap_plain(&format!("Address  {url}"), Style::new(), width));
    }
    if let Some(served) = &runtime.served_by {
        lines.extend(wrap_plain(&format!("Run by   {served}"), Style::new(), width));
    }
    if let Some(image) = &runtime.image {
        lines.extend(wrap_plain(&format!("Image    {image}"), theme::muted(), width));
    }
    if let Some(gpu) = &runtime.gpu {
        lines.extend(wrap_plain(gpu, Style::new(), width));
    }
    match &runtime.state {
        RuntimeRowState::Stopped { start_command: Some(command), needs_root } => {
            lines.push(Line::default());
            lines.extend(wrap_plain("Enter starts it with:", theme::muted(), width));
            lines.extend(wrap_plain(command, Style::new().fg(color(Token::Sky)), width));
            if *needs_root {
                lines.extend(wrap_plain("This needs your password (sudo).", theme::muted(), width));
            }
        }
        RuntimeRowState::Stopped { start_command: None, .. } => {
            lines.push(Line::default());
            lines.extend(wrap_plain(
                "re-code doesn't know a safe way to start this one; start it yourself.",
                theme::muted(),
                width,
            ));
        }
        RuntimeRowState::NotAnswering { error } => {
            lines.push(Line::default());
            lines.extend(wrap_plain(error, Style::new().fg(color(Token::Danger)), width));
        }
        RuntimeRowState::Running { .. } => {}
    }
    if !runtime.supported {
        lines.push(Line::default());
        lines.extend(wrap_plain(
            "Detected, but re-code hasn't been tested with it yet, so it isn't offered as a provider.",
            theme::muted(),
            width,
        ));
    }
    if runtime.yours {
        lines.push(Line::default());
        lines.extend(wrap_plain(
            "Yours: re-code only starts it when you say so, and never stops or changes it.",
            theme::muted(),
            width,
        ));
    }
    for note in &runtime.notes {
        lines.extend(wrap_plain(note, theme::muted(), width));
    }
    lines
}

fn container_details(container: &ContainerRow, width: usize) -> Vec<Line<'static>> {
    let mut lines = vec![Line::styled(container.name.clone(), Style::new().add_modifier(Modifier::BOLD))];
    let state_color = if container.running { color(Token::Success) } else { color(Token::Muted) };
    lines.push(Line::from(vec![chip(container.status.clone(), state_color)]));
    lines.push(Line::default());
    lines.extend(wrap_plain(&format!("Image  {}", container.image), Style::new(), width));
    if let Some(pod) = &container.pod {
        lines.extend(wrap_plain(&format!("Pod    {pod}"), Style::new(), width));
    }
    if !container.ports.is_empty() {
        lines.extend(wrap_plain(&format!("Ports {}", ports_text(&container.ports)), Style::new(), width));
    }
    lines.push(Line::default());
    match &container.runs {
        Some(runs) => lines.extend(wrap_plain(&format!("Its image runs {runs}."), Style::new(), width)),
        None => lines.extend(wrap_plain(
            "Not an inference runtime re-code recognises; listed so you can see what's around it.",
            theme::muted(),
            width,
        )),
    }
    lines
}
