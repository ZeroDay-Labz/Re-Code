//! Draws the interface. Every view has to fit and stay usable at whatever size the terminal is,
//! which `tests/any_size.rs` enforces across a sweep of sizes.

use crate::app::{App, Overlay, Screen};
use crate::form::{ConfirmState, FormState};
use crate::status::{self, Segment};
use crate::theme::{self, ColorMode, Token, color, gradient};
use crate::transcript::render_diff;
use crate::widgets::{chip, legend, meter, panel};
use crate::wrap::{line_width, truncate, wrap_plain};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use recode_core::protocol::AgentState;
use recode_core::usage::TokenCount;
use std::fmt::Write as _;
use unicode_width::UnicodeWidthStr;

/// Below this, there is no useful interface to show.
pub const MIN_WIDTH: u16 = 20;
pub const MIN_HEIGHT: u16 = 4;

pub const PANEL_WIDTH: u16 = 34;
pub const PANEL_MIN_WIDTH: u16 = 24;
pub const PANEL_MAX_WIDTH: u16 = 60;
/// A live graph is only worth its rows when there's plenty of height.
const ACTIVITY_MIN_TERMINAL: u16 = 32;
const PANEL_MIN_TERMINAL: u16 = 130;
/// Conversation text stops growing here, since very long lines are hard to read.
const READABLE_WIDTH: u16 = 110;
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Whether the side panel shows unless the user has chosen otherwise.
#[must_use]
pub fn panel_default(width: usize) -> bool {
    width >= usize::from(PANEL_MIN_TERMINAL)
}

/// The screen's regions, computed so they always fit the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Regions {
    /// Whether panels have borders. On a very small terminal they cost more than they give.
    pub bordered: bool,
    /// The conversation's panel, or its text area when borderless.
    pub conversation: Rect,
    pub panel: Option<Rect>,
    pub activity: Option<Rect>,
    /// The input's panel, or the bar and input rows when borderless.
    pub input_panel: Rect,
    /// Separate status row, only when borderless. Otherwise the status sits in the input panel's border.
    pub status: Option<Rect>,
    pub legend: Option<Rect>,
}

fn input_limit(height: u16) -> u16 {
    match height {
        0..=5 => 1,
        6..=11 => 3,
        _ => (height / 3).clamp(3, 10),
    }
}

/// Lays out the screen for a given input size. The output graph (`wants_activity`) only takes room
/// while the model is working; the rest of the time that space goes to the conversation.
#[must_use]
pub fn regions(
    area: Rect,
    input_rows: u16,
    wants_approval_bar: bool,
    wants_panel: bool,
    wants_activity: bool,
) -> Regions {
    regions_with_panel(area, input_rows, wants_approval_bar, wants_panel, wants_activity, PANEL_WIDTH)
}

/// [`regions`] with a chosen side-panel width.
#[must_use]
pub fn regions_with_panel(
    area: Rect,
    input_rows: u16,
    wants_approval_bar: bool,
    wants_panel: bool,
    wants_activity: bool,
    panel_width: u16,
) -> Regions {
    let bordered = area.width >= 40 && area.height >= 14;
    let bar = u16::from(wants_approval_bar);
    let input = input_rows.clamp(1, input_limit(area.height)) + bar;
    let (input_height, status_height, legend_height) = if bordered { (input + 2, 0, 1) } else { (input, 1, 0) };
    let activity_height = if wants_activity && bordered && area.height >= ACTIVITY_MIN_TERMINAL { 4 } else { 0 };
    let conversation_height =
        area.height.saturating_sub(input_height + status_height + legend_height + activity_height);

    let mut y = area.y;
    let mut take = |rows: u16| {
        let rect = Rect::new(area.x, y, area.width, rows);
        y += rows;
        rect
    };
    let middle = take(conversation_height);
    let activity = (activity_height > 0).then(|| take(activity_height));
    let input_panel = take(input_height);
    let status = (status_height > 0).then(|| take(status_height));
    let legend = (legend_height > 0).then(|| take(legend_height));

    let panel_width = panel_width.clamp(PANEL_MIN_WIDTH, PANEL_MAX_WIDTH);
    let show_panel = wants_panel && bordered && area.width >= panel_width + 40 && middle.height >= 8;
    let (conversation, panel) = if show_panel {
        let left = Rect::new(middle.x, middle.y, middle.width - panel_width, middle.height);
        let right = Rect::new(middle.x + left.width, middle.y, panel_width, middle.height);
        (left, Some(right))
    } else {
        (middle, None)
    };
    Regions { bordered, conversation, panel, activity, input_panel, status, legend }
}

/// Centres the text within its panel once it reaches a comfortable reading width.
fn readable(area: Rect) -> Rect {
    let margin = u16::from(area.width >= 60 && !theme::resolved().compact);
    let width = area.width.saturating_sub(margin * 2).min(READABLE_WIDTH);
    Rect::new(area.x + (area.width - width) / 2, area.y, width, area.height)
}

/// Draws one frame.
pub fn draw(frame: &mut Frame<'_>, app: &mut App, colors: ColorMode) {
    let area = frame.area();
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        draw_too_small(frame, area);
        colors.apply(frame.buffer_mut());
        return;
    }
    app.terminal_width = usize::from(area.width);
    // Density is the theme's; it's part of the transcript's cache key, so it must be set before drawing.
    app.options.compact = theme::resolved().compact;
    app.options.line_numbers = app.settings.get("line_numbers").is_some_and(|v| v == "on");
    app.screen_rect = area;
    if matches!(app.screen, Screen::Welcome(_)) {
        crate::welcome::draw(frame, area, app);
        if app.overlay.is_some() {
            draw_overlay(frame, area, app);
        }
        draw_toasts(frame, area, app);
        app.region_rects = vec![area];
        finish_frame(frame, app);
        colors.apply(frame.buffer_mut());
        return;
    }
    app.hits = crate::app::Hits::default();
    let wants_panel = app.panel_preference.unwrap_or(panel_default(usize::from(area.width)));
    let pending = app.pending_approval().is_some();
    let border_cost = if area.width >= 40 && area.height >= 14 { 2 } else { 0 };
    let prompt = if area.width.saturating_sub(border_cost) >= 8 { 4 } else { 2 };
    let input_inner = usize::from(area.width.saturating_sub(border_cost + prompt)).max(1);
    app.input_width = input_inner;
    let rows = app.input.rows(input_inner);
    // Reserve one extra row for the image-chip strip when images are attached, or the queue strip.
    let chip_row = u16::from(!app.pending_images.is_empty() || !app.queue.is_empty());
    let input_rows = u16::try_from(rows.len()).unwrap_or(u16::MAX).saturating_add(chip_row);
    let working = app.is_busy() && !pending;
    let regions = regions_with_panel(area, input_rows, pending, wants_panel, working, app.panel_width);

    let text_area = if regions.bordered {
        let title = conversation_title(app, usize::from(regions.conversation.width).saturating_sub(10));
        let right = app.demo.then(|| Line::from(chip("DEMO", theme::color(Token::Attention))));
        panel(frame, regions.conversation, title, right, None, !pending)
    } else {
        regions.conversation
    };
    let text = readable(text_area);
    app.view_size = (usize::from(text.width), usize::from(text.height));
    draw_transcript(frame, text, app);

    app.panel_shown = regions.panel.is_some();
    app.hits.panel = regions.panel;
    if let Some(side) = regions.panel {
        match app.panel_tab {
            crate::app::PanelTab::Session => draw_panel(frame, side, app),
            crate::app::PanelTab::Files => draw_files_panel(frame, side, app),
        }
    }
    if let Some(activity) = regions.activity {
        draw_activity(frame, activity, app);
    }

    let inner = if regions.bordered {
        let width = usize::from(regions.input_panel.width).saturating_sub(4);
        let status = Line::from(
            [vec![Span::raw(" ")], status_line(app, width.saturating_sub(2)).spans, vec![Span::raw(" ")]].concat(),
        );
        panel(frame, regions.input_panel, status, None, None, pending)
    } else {
        regions.input_panel
    };
    let bar_rows = u16::from(pending);
    draw_approval_bar(frame, Rect::new(inner.x, inner.y, inner.width, bar_rows.min(inner.height)), app);
    let input_area =
        Rect::new(inner.x, inner.y + bar_rows.min(inner.height), inner.width, inner.height.saturating_sub(bar_rows));
    draw_input(frame, input_area, app);

    if let Some(status) = regions.status {
        frame.render_widget(Paragraph::new(status_line(app, usize::from(status.width))), status);
    }
    if let Some(legend_area) = regions.legend {
        let line = legend(&conversation_legend(app), usize::from(legend_area.width)).centered();
        frame.render_widget(Paragraph::new(line), legend_area);
    }
    if let Some(completion) = app.completion() {
        let above = Rect::new(area.x, area.y, area.width, regions.input_panel.y.saturating_sub(area.y));
        draw_completion(frame, above, regions.input_panel.x, &completion, app.completion_selected);
    }
    if app.overlay.is_some() {
        draw_overlay(frame, area, app);
    }
    draw_toasts(frame, area, app);
    app.region_rects = [
        Some(regions.conversation),
        regions.panel,
        regions.input_panel.into(),
        regions.status,
        regions.legend,
        regions.activity,
    ]
    .into_iter()
    .flatten()
    .collect();
    finish_frame(frame, app);
    colors.apply(frame.buffer_mut());
}

/// After everything is drawn: highlights a selection made outside the conversation text and keeps the
/// frame's cells, so such a selection can be copied as it was seen.
fn finish_frame(frame: &mut Frame<'_>, app: &mut App) {
    let buffer = frame.buffer_mut();
    if let Some(selection) = app.screen_selection {
        let clip = selection.clip;
        for y in clip.y..clip.y + clip.height {
            for x in clip.x..clip.x + clip.width {
                if selection.contains(x, y)
                    && let Some(cell) = buffer.cell_mut((x, y))
                {
                    cell.modifier.insert(Modifier::REVERSED);
                }
            }
        }
    }
    app.last_frame = frame_cells(buffer);
}

/// The frame's cells as text, row by row: one string per cell, with the second cell of a wide
/// character empty so columns line up with the screen.
#[must_use]
pub fn frame_cells(buffer: &ratatui::buffer::Buffer) -> Vec<Vec<String>> {
    let mut rows = Vec::with_capacity(usize::from(buffer.area.height));
    for y in buffer.area.y..buffer.area.y + buffer.area.height {
        let mut cells: Vec<String> = Vec::with_capacity(usize::from(buffer.area.width));
        let mut x = buffer.area.x;
        while x < buffer.area.x + buffer.area.width {
            let symbol = buffer[(x, y)].symbol().to_owned();
            let width = unicode_width::UnicodeWidthStr::width(symbol.as_str()).max(1);
            cells.push(symbol);
            for _ in 1..width {
                cells.push(String::new());
            }
            x += u16::try_from(width).unwrap_or(1);
        }
        cells.truncate(usize::from(buffer.area.width));
        rows.push(cells);
    }
    rows
}

/// Toasts stack in the top-right corner, newest at the bottom, and never on a narrow screen.
fn draw_toasts(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let toasts = app.live_toasts();
    if toasts.is_empty() || area.width < 30 || area.height < 4 {
        return;
    }
    let width = area.width.min(48);
    let x = area.x + area.width - width;
    let mut y = area.y + 1;
    for toast in toasts {
        let (icon, color) = match toast.level {
            crate::app::ToastLevel::Info => ("•", theme::color(Token::Sky)),
            crate::app::ToastLevel::Success => ("✓", theme::color(Token::Success)),
            crate::app::ToastLevel::Warning => ("▲", theme::color(Token::Attention)),
        };
        let inner = usize::from(width).saturating_sub(4);
        let lines = wrap_plain(&toast.text, Style::new(), inner.saturating_sub(2));
        let rows = u16::try_from(lines.len().min(3)).unwrap_or(1);
        if y + rows + 1 > area.y + area.height {
            break;
        }
        let rect = Rect::new(x, y, width, rows + 2);
        frame.render_widget(ratatui::widgets::Clear, rect);
        let block = ratatui::widgets::Block::bordered()
            .border_type(crate::widgets::border_type())
            .border_style(Style::new().fg(color))
            .style(Style::new().bg(theme::color(Token::CodeBg)));
        let body = block.inner(rect);
        frame.render_widget(block, rect);
        let text: Vec<Line<'static>> = lines
            .into_iter()
            .take(3)
            .enumerate()
            .map(|(index, line)| {
                let lead = if index == 0 { format!("{icon} ") } else { "  ".to_owned() };
                Line::from([vec![Span::styled(lead, Style::new().fg(color))], line.spans].concat())
            })
            .collect();
        frame.render_widget(Paragraph::new(text), Rect::new(body.x + 1, body.y, body.width.saturating_sub(1), rows));
        y += rows + 2;
    }
}

/// The `/` and `@` suggestions, in a box sitting on top of the input.
fn draw_completion(
    frame: &mut Frame<'_>,
    above: Rect,
    left: u16,
    completion: &crate::complete::Completion,
    selected: usize,
) {
    if above.height == 0 || above.width < 4 {
        return;
    }
    let count = completion.items.len();
    let selected = selected.min(count.saturating_sub(1));
    let bordered = above.height >= 3 && above.width >= 24;
    let room = usize::from(above.height.saturating_sub(if bordered { 2 } else { 0 })).max(1);
    let shown = count.min(room);
    // Keep the highlighted row in view when there's less room than suggestions.
    let first = (selected + 1).saturating_sub(shown);
    let width = above.width.min(76);
    let x = left.min(above.x + above.width - width);
    let height = u16::try_from(shown).unwrap_or(1) + if bordered { 2 } else { 0 };
    let rect = Rect::new(x, above.y + above.height - height, width, height);
    frame.render_widget(ratatui::widgets::Clear, rect);
    let inner = if bordered {
        let title = match completion.kind {
            crate::complete::Kind::Command => " commands ",
            crate::complete::Kind::File => " files ",
        };
        let block = ratatui::widgets::Block::bordered()
            .border_type(crate::widgets::border_type())
            .border_style(theme::accent())
            .title(Span::styled(title, theme::accent()))
            .title_bottom(Line::from(Span::styled(" tab complete · esc close ", theme::faint())).right_aligned());
        let inner = block.inner(rect);
        frame.render_widget(block, rect);
        inner
    } else {
        rect
    };
    let text_width = usize::from(inner.width);
    for (row, item) in completion.items.iter().enumerate().skip(first).take(shown) {
        let is_selected = row == selected;
        let base = if is_selected {
            Style::new().bg(theme::color(Token::UserBg)).add_modifier(Modifier::BOLD)
        } else {
            Style::new()
        };
        let marker = if is_selected { "❯ " } else { "  " };
        let label = truncate(&item.label, text_width.saturating_sub(2));
        let detail_room = text_width.saturating_sub(2 + label.width() + 2);
        let detail = if detail_room >= 8 { truncate(&item.detail, detail_room) } else { String::new() };
        let fill = text_width.saturating_sub(2 + label.width() + detail.width());
        let line = Line::from(vec![
            Span::styled(marker, theme::accent().patch(base)),
            Span::styled(label, base),
            Span::styled(" ".repeat(fill), base),
            Span::styled(detail, theme::muted().patch(base)),
        ]);
        let y = inner.y + u16::try_from(row - first).unwrap_or(0);
        frame.render_widget(Paragraph::new(line), Rect::new(inner.x, y, inner.width, 1));
    }
}

fn draw_too_small(frame: &mut Frame<'_>, area: Rect) {
    let lines = wrap_plain("Terminal too small", theme::muted(), usize::from(area.width));
    frame.render_widget(Paragraph::new(lines), area);
}

fn conversation_title(app: &App, room: usize) -> Line<'static> {
    let brand = " ◆ re-code ";
    let project = if app.project == "re-code" { "" } else { app.project.as_str() };
    let location = match (project.is_empty(), app.title.is_empty()) {
        (false, false) => format!("{project} › {}", app.title),
        (false, true) => project.to_owned(),
        _ => app.title.clone(),
    };
    let location = truncate(&location, room.saturating_sub(brand.width() + 1));
    Line::from(vec![
        Span::styled(brand, theme::accent().add_modifier(Modifier::BOLD)),
        Span::styled(format!("{location} "), theme::muted()),
    ])
}

fn conversation_legend(app: &App) -> Vec<(&'static str, &'static str)> {
    let mut items: Vec<(&str, &str)> = Vec::new();
    if app.quit_armed {
        items.push(("^C", "again to exit"));
    }
    if app.is_busy() {
        items.push(("esc", "stop"));
        items.push(("enter", "queue"));
        if app.keyboard_enhanced {
            items.push(("^enter", "send now"));
        }
    } else {
        items.push(("enter", "send"));
        items.push(("alt+enter", "new line"));
    }
    items.extend([("pgup", "scroll"), ("^K", "commands")]);
    if !app.demo {
        items.extend([("^L", "model"), ("^E", "extensions"), ("⇧tab", "mode")]);
    }
    items.extend([("^B", "panel"), ("F1", "keys")]);
    if !app.quit_armed {
        items.push(("^C", "quit"));
    }
    items
}

/// A live graph of how fast the model is producing text. Silence shows as a flat line, because it's real data.
fn draw_activity(frame: &mut Frame<'_>, area: Rect, app: &App) {
    const EIGHTHS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let right =
        app.tokens_per_second.map(|speed| Line::styled(format!(" {speed:.0} tok/s last reply "), theme::muted()));
    let title = Line::styled(" Model output ", Style::new().fg(theme::color(Token::Attention)));
    let inner = panel(frame, area, title, right, None, false);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let values = app.activity_window(usize::from(inner.width));
    let peak = values.iter().copied().max().unwrap_or(0).max(1);
    let levels = usize::from(inner.height) * 8;
    let buffer = frame.buffer_mut();
    for (column, value) in values.iter().enumerate() {
        let x = inner.x + u16::try_from(column).unwrap_or(0);
        let value = usize::try_from(*value).unwrap_or(0);
        let peak = usize::try_from(peak).unwrap_or(1);
        let level = (value * levels).div_ceil(peak);
        for row in 0..usize::from(inner.height) {
            let y = inner.y + inner.height - 1 - u16::try_from(row).unwrap_or(0);
            let fill = level.saturating_sub(row * 8).min(8);
            let cell = &mut buffer[(x, y)];
            if fill > 0 {
                #[allow(clippy::cast_precision_loss)]
                let position = (row as f32 + fill as f32 / 8.0) / f32::from(inner.height);
                cell.set_char(EIGHTHS[fill]).set_fg(gradient(position));
            } else if row == 0 {
                cell.set_char('▁').set_fg(theme::color(Token::Faint));
            }
        }
    }
}

fn draw_transcript(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let (width, height) = (usize::from(area.width), usize::from(area.height));
    let (lines, owners) = app.transcript.visible_mapped(width, height, app.options);
    let below = app.transcript.lines_below(width, height, app.options);
    for (offset, entry) in owners.iter().enumerate() {
        let y = area.y + u16::try_from(offset).unwrap_or(u16::MAX);
        app.hits.lines.push((y, area.x, area.x + area.width, *entry));
    }
    // Paths of project files in the text are clickable: underline them and remember where they are.
    if !app.project_file_set.is_empty() {
        for (offset, line) in lines.iter().enumerate() {
            let y = area.y + u16::try_from(offset).unwrap_or(u16::MAX);
            for (start, end, path) in path_mentions(&line.to_string(), &app.project_file_set) {
                let (x0, x1) = (
                    area.x + u16::try_from(start).unwrap_or(u16::MAX),
                    area.x + u16::try_from(end).unwrap_or(u16::MAX),
                );
                app.hits.paths.push((y, x0, x1, path));
            }
        }
    }
    frame.render_widget(Paragraph::new(lines), area);
    let buffer = frame.buffer_mut();
    for (y, x0, x1, _) in &app.hits.paths {
        for x in *x0..*x1 {
            if let Some(cell) = buffer.cell_mut((x, *y)) {
                cell.modifier.insert(Modifier::UNDERLINED);
            }
        }
    }
    // While the model thinks, the newest reasoning line's marker pulses, so the wait reads as alive.
    if app.state == AgentState::Thinking && !app.reduced_motion {
        let last_reasoning = owners.iter().rposition(|entry| {
            matches!(app.transcript.iter().nth(*entry), Some(crate::transcript::Entry::Reasoning(_)))
        });
        if let Some(row) = last_reasoning {
            let frame_index =
                usize::try_from(app.now.saturating_duration_since(app.created).as_millis() / 150).unwrap_or(0) % 4;
            let glyph = ["·", "∙", "•", "∙"][frame_index];
            if let Some(cell) = buffer.cell_mut((area.x, area.y + u16::try_from(row).unwrap_or(u16::MAX))) {
                cell.set_symbol(glyph).set_fg(theme::color(Token::Accent));
            }
        }
    }
    // Where the conversation is and which line is at the top, so the mouse maps to its text.
    app.transcript_rect = (area.x, area.y, area.width, area.height);
    app.transcript_top = app.transcript.top(width, height, app.options);
    if let Some(selection) = app.selection {
        let buffer = frame.buffer_mut();
        for row in 0..area.height {
            let line = app.transcript_top + usize::from(row);
            for column in 0..area.width {
                if selection.contains(line, column)
                    && let Some(cell) = buffer.cell_mut((area.x + column, area.y + row))
                {
                    cell.modifier.insert(Modifier::REVERSED);
                }
            }
        }
    }
    // Scrolled up: say how much is below and how to get back (clicking it goes there too).
    app.hits.badge = None;
    if below > 0 && area.width >= 12 {
        let long = format!(" ↓ {below} more lines · End ");
        let text = if long.width() + 2 <= width { long } else { format!(" ↓ {below} ") };
        let badge_width = u16::try_from(text.width()).unwrap_or(area.width).min(area.width);
        let rect = Rect::new(area.x + area.width - badge_width, area.y + area.height - 1, badge_width, 1);
        let style =
            Style::new().fg(theme::color(Token::Accent)).bg(theme::color(Token::UserBg)).add_modifier(Modifier::BOLD);
        frame.render_widget(Paragraph::new(Line::styled(text, style)), rect);
        app.hits.badge = Some((rect.y, rect.x, rect.x + rect.width));
    }
}

/// Where known project files are mentioned in a line of text: (start column, end column, path).
/// A trailing `:line` is allowed and kept out of the path.
fn path_mentions(text: &str, files: &std::collections::HashSet<String>) -> Vec<(usize, usize, String)> {
    let mut found = Vec::new();
    let mut column = 0;
    for token in text.split(|c: char| c.is_whitespace() || "()[]{}<>\"'`,;".contains(c)) {
        let start = column;
        column += token.width() + 1;
        let trimmed = token.trim_end_matches(['.', ':']);
        let candidate = trimmed.split(':').next().unwrap_or(trimmed).trim_start_matches("./");
        if (candidate.contains('/') || candidate.contains('.')) && files.contains(candidate) {
            found.push((start, start + trimmed.width(), candidate.to_owned()));
        }
    }
    found
}

fn compact_tokens(value: u64) -> String {
    if value < 1000 {
        return value.to_string();
    }
    // Context sizes are usually powers of two, and people call 32768 "32k".
    if value.is_multiple_of(1024) {
        return format!("{}k", value / 1024);
    }
    #[allow(clippy::cast_precision_loss)]
    let thousands = value as f64 / 1000.0;
    if thousands < 10.0 { format!("{thousands:.1}k") } else { format!("{thousands:.0}k") }
}

/// How much room is left before compaction, and how loudly to say it: quiet above 40 % left, a
/// warning colour below, bold "compacts soon" under 15 %.
fn context_room(app: &App) -> (String, Style) {
    let Some((used, _)) = &app.context else { return (String::new(), theme::muted()) };
    let Some(window) = &app.context_window else { return (String::new(), theme::muted()) };
    let used = match used {
        TokenCount::Estimated { tokens, .. } | TokenCount::Exact { tokens, .. } => *tokens,
    };
    match window.trigger {
        Some(trigger) if trigger > 0 => {
            let left = trigger.saturating_sub(used) * 100 / trigger;
            if left >= 40 {
                (format!(" · {left}% left"), theme::muted())
            } else if left >= 15 {
                (format!(" · {left}% left"), theme::attention())
            } else {
                (" · compacts soon".to_owned(), theme::attention().add_modifier(Modifier::BOLD))
            }
        }
        _ if window.window.is_some() => (" · no auto-compact".to_owned(), theme::muted()),
        _ => (String::new(), theme::muted()),
    }
}

/// `~9.1k/32k`, with the marker only on estimated figures.
fn context_text(used: &TokenCount, limit: u64) -> String {
    let (marker, tokens) = match used {
        TokenCount::Estimated { tokens, .. } => ("~", *tokens),
        TokenCount::Exact { tokens, .. } => ("", *tokens),
    };
    if limit == 0 {
        return format!("{marker}{} (limit unknown)", compact_tokens(tokens));
    }
    format!("{marker}{}/{}", compact_tokens(tokens), compact_tokens(limit))
}

/// The project's files as a tree: click a folder to open it, a file to edit it.
fn draw_files_panel(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let title = Line::styled(" Files ", Style::new().fg(theme::color(Token::Attention)));
    let hint = Line::styled(" ^B session ", theme::faint());
    let inner = panel(frame, area, title, Some(hint), None, false);
    let width = usize::from(inner.width).saturating_sub(1);
    let rows = app.tree.rows(&app.project_files);
    if rows.is_empty() {
        let text = if app.project_files.is_empty() { " No project files found." } else { " Nothing to show." };
        frame.render_widget(Paragraph::new(Line::styled(text, theme::muted())), inner);
        return;
    }
    let height = usize::from(inner.height);
    let first = app.tree.scroll.min(rows.len().saturating_sub(height));
    app.tree.scroll = first;
    for (offset, (index, entry)) in rows.iter().enumerate().skip(first).take(height).enumerate() {
        let y = inner.y + u16::try_from(offset).unwrap_or(u16::MAX);
        let indent = "  ".repeat(entry.depth.min(8));
        let (icon, style) = if entry.is_dir {
            (if entry.open { "▾ " } else { "▸ " }, theme::fg(Token::Sky))
        } else {
            ("  ", Style::new())
        };
        let changed = app.files.iter().any(|file| file.path == entry.path);
        let style = if changed { theme::fg(Token::Attention) } else { style };
        let name = truncate(&entry.name, width.saturating_sub(indent.len() + 3));
        let line = Line::from(vec![
            Span::raw(format!(" {indent}")),
            Span::styled(icon, theme::fg(Token::Sky)),
            Span::styled(name, style),
        ]);
        frame.render_widget(Paragraph::new(line), Rect::new(inner.x, y, inner.width, 1));
        app.hits.tree_rows.push((y, inner.x, inner.x + inner.width, index));
    }
}

fn draw_panel(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let title = Line::styled(" Session ", Style::new().fg(theme::color(Token::Attention)));
    let hint = Line::styled(" ^B files ", theme::faint());
    let inner = panel(frame, area, title, Some(hint), None, false);
    let width = usize::from(inner.width).saturating_sub(2);
    let heading = |text: &str| Line::styled(format!(" {text}"), Style::new().fg(theme::color(Token::Attention)));
    let value = |text: &str| Line::raw(format!(" {}", truncate(text, width)));
    let mut lines = vec![
        heading("Model"),
        value(&app.model),
        Line::styled(format!(" {}", truncate(&app.provider, width)), theme::muted()),
        Line::default(),
    ];

    if let Some((active, summary)) = &app.tool_status {
        lines.push(heading("Tools"));
        let style = if *active { Style::new().fg(theme::color(Token::Success)) } else { theme::muted() };
        for line in wrap_plain(summary, style, width) {
            lines.push(Line::from([vec![Span::raw(" ")], line.spans].concat()));
        }
        lines.push(Line::default());
    }

    lines.push(heading("Context"));
    if let Some((used, limit)) = &app.context {
        let tokens = match used {
            TokenCount::Estimated { tokens, .. } | TokenCount::Exact { tokens, .. } => *tokens,
        };
        #[allow(clippy::cast_precision_loss)]
        let fraction = if *limit == 0 { 0.0 } else { tokens as f64 / *limit as f64 };
        lines.push(Line::from([vec![Span::raw(" ")], meter(fraction, width)].concat()));
        let (room, style) = context_room(app);
        lines.push(Line::from(vec![Span::raw(format!(" {}", context_text(used, *limit))), Span::styled(room, style)]));
        if let Some(trigger) = app.context_window.as_ref().and_then(|w| w.trigger) {
            lines.push(Line::styled(format!(" compacts at ~{}", compact_tokens(trigger)), theme::muted()));
        }
    } else {
        lines.push(Line::styled(" Nothing sent yet", theme::muted()));
    }
    lines.push(Line::default());

    if !app.todos.is_empty() {
        lines.push(heading("Tasks"));
        for item in &app.todos {
            let (mark, style) = match item.status {
                recode_core::protocol::TodoStatus::Done => ("☑", theme::muted()),
                recode_core::protocol::TodoStatus::InProgress => ("◐", theme::fg(Token::Accent)),
                recode_core::protocol::TodoStatus::Pending => ("☐", Style::new()),
            };
            let text = truncate(&item.text, width.saturating_sub(2));
            lines.push(Line::from(vec![Span::styled(format!(" {mark} "), style), Span::styled(text, style)]));
        }
        lines.push(Line::default());
    }

    lines.push(heading("Files changed"));
    if app.files.is_empty() {
        lines.push(Line::styled(" None", theme::muted()));
    }
    let files_first_row = inner.y + u16::try_from(lines.len()).unwrap_or(u16::MAX);
    for (index, file) in app.files.iter().enumerate() {
        let y = files_first_row + u16::try_from(index).unwrap_or(u16::MAX);
        if y < inner.y + inner.height {
            app.hits.panel_files.push((y, inner.x, inner.x + inner.width, file.path.clone()));
        }
        let added = format!("+{}", file.added);
        let removed = format!("−{}", file.removed);
        let counts = added.width() + removed.width() + 1;
        let path = truncate(&file.path, width.saturating_sub(counts + 1));
        let fill = width.saturating_sub(path.width() + counts);
        lines.push(Line::from(vec![
            Span::raw(format!(" {path}{}", " ".repeat(fill))),
            Span::styled(added, Style::new().fg(theme::color(Token::Success))),
            Span::raw(" "),
            Span::styled(removed, Style::new().fg(theme::color(Token::Danger))),
        ]));
    }
    lines.push(Line::default());
    lines.push(heading("Tools run"));
    lines.push(value(&app.tools_run.to_string()));
    frame.render_widget(Paragraph::new(lines), inner);
}

/// Stays on screen while approval is pending, so the keys remain visible however small the window is and wherever the
/// conversation is scrolled.
fn draw_approval_bar(frame: &mut Frame<'_>, area: Rect, app: &App) {
    if area.height == 0 {
        return;
    }
    let Some((tool, _)) = app.pending_approval() else { return };
    let width = usize::from(area.width);
    // "a" says exactly what it allows ("always change files in ~/notes"), or isn't offered at all.
    let always = tool.always();
    // The full "always …" wording if it fits, then plain "always", then just the keys.
    let fits = |label: Option<&str>| {
        let mut keys = vec![("y", "approve")];
        if let Some(label) = label {
            keys.push(("a", label));
            keys.push(("p", "for this project"));
        }
        keys.extend([("n", "deny"), ("d", "diff")]);
        let line = legend(&keys, width);
        (width >= line_width(&line) + 12 && line.to_string().contains("d diff")).then_some(line)
    };
    let short = if always.is_some() { "y/a/p/n/d" } else { "y/n/d" };
    let keys = fits(always)
        .or_else(|| always.and_then(|_| fits(Some("always"))))
        .unwrap_or_else(|| Line::styled(short, theme::accent().add_modifier(Modifier::BOLD)));
    let keys_width = line_width(&keys);
    let room = width.saturating_sub(keys_width + 4);
    let what = truncate(&format!("{} {}", tool.name, tool.target), room);
    let fill = width.saturating_sub(4 + what.width() + keys_width);
    let mut spans = vec![
        Span::styled(" ▲ ", theme::attention()),
        Span::styled(what, theme::attention()),
        Span::raw(" ".repeat(fill)),
    ];
    spans.extend(keys.spans);
    spans.push(Span::raw(" "));
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_input(frame: &mut Frame<'_>, area: Rect, app: &App) {
    if area.height == 0 || area.width < 3 {
        return;
    }
    // Messages queued while the model works sit above the prompt, or a row of image chips.
    let area = if !app.queue.is_empty() && area.height >= 2 {
        let (first, _) = &app.queue[0];
        let more = if app.queue.len() > 1 { format!(" (+{} more)", app.queue.len() - 1) } else { String::new() };
        // The hint shrinks before the message does.
        let width = usize::from(area.width);
        let hint = if width >= 90 {
            "  backspace edits · esc stops the turn"
        } else if width >= 50 {
            "  ⌫ edits"
        } else {
            ""
        };
        let room = width.saturating_sub(10 + more.width() + hint.width());
        let line = Line::from(vec![
            Span::styled("queued: ", theme::muted()),
            Span::styled(
                format!("\"{}\"", truncate(first.lines().next().unwrap_or_default(), room.max(4))),
                Style::new(),
            ),
            Span::styled(more, theme::muted()),
            Span::styled(hint, theme::faint()),
        ]);
        frame.render_widget(Paragraph::new(line), Rect::new(area.x, area.y, area.width, 1));
        Rect::new(area.x, area.y + 1, area.width, area.height - 1)
    } else if !app.pending_images.is_empty() && area.height >= 2 {
        let chips: Vec<Span<'static>> = app
            .pending_images
            .iter()
            .flat_map(|image| {
                let label = if image.label.is_empty() { "image" } else { image.label.as_str() };
                [chip(format!("image {label}"), theme::color(Token::Sky)), Span::raw(" ")]
            })
            .collect();
        let hint = Span::styled("  backspace removes", theme::faint());
        frame.render_widget(
            Paragraph::new(Line::from([chips, vec![hint]].concat())),
            Rect::new(area.x, area.y, area.width, 1),
        );
        Rect::new(area.x, area.y + 1, area.width, area.height - 1)
    } else {
        area
    };
    let prompt_width: u16 = if area.width >= 8 { 4 } else { 2 };
    // The prompt glyph says the mode at a glance: › ask, » accept edits, ⏸ plan, ⚡ auto.
    let glyph = match app.permission_mode {
        recode_core::protocol::PermissionMode::Ask => "❯",
        recode_core::protocol::PermissionMode::AcceptEdits => "»",
        recode_core::protocol::PermissionMode::Plan => "⏸",
        recode_core::protocol::PermissionMode::AutoSandbox => "⚡",
    };
    let prompt = if prompt_width == 4 { format!(" {glyph}  ") } else { format!("{glyph} ") };
    let prompt = prompt.as_str();
    let inner_width = usize::from(area.width - prompt_width);
    let rows = app.input.rows(inner_width);
    let (row, column) = app.input.cursor_at(&rows);
    let height = usize::from(area.height);
    // The input scrolls within its box, keeping the cursor visible.
    let first = (row + 1).saturating_sub(height);
    let mut lines: Vec<Line<'_>> = Vec::with_capacity(height);
    for (index, range) in rows.iter().enumerate().skip(first).take(height) {
        let marker = if index == 0 { prompt.to_owned() } else { " ".repeat(usize::from(prompt_width)) };
        lines.push(Line::from(vec![
            Span::styled(marker, theme::accent()),
            Span::raw(app.input.text()[range.start..range.end].to_owned()),
        ]));
    }
    if app.input.is_empty() {
        let hint = if app.pending_approval().is_some() {
            "Waiting for your answer above"
        } else if app.is_busy() {
            "Esc stops the agent"
        } else if app.demo {
            "Type anything and press Enter to replay the demo"
        } else {
            "Ask anything"
        };
        lines = vec![Line::from(vec![
            Span::styled(prompt.to_owned(), theme::accent()),
            Span::styled(truncate(hint, inner_width), theme::faint()),
        ])];
    }
    frame.render_widget(Paragraph::new(lines), area);
    if app.overlay.is_none() {
        let x = area.x + prompt_width + u16::try_from(column).unwrap_or(0);
        let y = area.y + u16::try_from(row - first).unwrap_or(0);
        if x < area.x + area.width && y < area.y + area.height {
            frame.set_cursor_position((x, y));
        }
    }
}

/// The state, in plain words, with a shorter form for narrow windows.
fn state_text(app: &App) -> (String, String) {
    let elapsed = app.state_elapsed().as_secs();
    let timed = |text: &str| if elapsed >= 2 { format!("{text} {elapsed}s") } else { text.to_owned() };
    let frame = usize::try_from(app.state_elapsed().as_millis() / 100).unwrap_or(0) % SPINNER.len();
    // With reduced motion the spinner holds still; the state and its timer still say what's happening.
    let spin = if app.reduced_motion { "◆" } else { SPINNER[frame] };
    match &app.state {
        AgentState::Idle => ("● Ready".into(), "●".into()),
        AgentState::LoadingModel => (format!("{spin} {}", timed("Loading model")), format!("{spin} Load")),
        AgentState::EvaluatingPrompt => (format!("{spin} {}", timed("Reading context")), format!("{spin} Read")),
        AgentState::Thinking => (format!("{spin} {}", timed("Thinking")), format!("{spin} Think")),
        AgentState::Responding => (format!("{spin} {}", timed("Responding")), format!("{spin} Reply")),
        AgentState::RunningTool { name } => {
            (format!("{spin} {}", timed(&format!("Running {name}"))), format!("{spin} {name}"))
        }
        AgentState::AwaitingApproval => ("▲ Needs approval".into(), "▲ Approve".into()),
        AgentState::Compacting => (format!("{spin} {}", timed("Compacting context")), format!("{spin} Compact")),
        AgentState::Stalled { silent_for_secs } => (format!("▲ No output for {silent_for_secs}s"), "▲ Stalled".into()),
    }
}

fn status_line(app: &App, width: usize) -> Line<'static> {
    let (state, state_short) = state_text(app);
    let state_style = match app.state {
        AgentState::AwaitingApproval | AgentState::Stalled { .. } => theme::attention(),
        AgentState::Idle => theme::muted(),
        _ => theme::accent(),
    };
    let mut segments = vec![Segment::new(0, state, state_style).or_short(state_short)];
    if app.quit_armed {
        segments.push(Segment::new(0, "Ctrl+C again to exit", theme::attention()).or_short("^C to exit"));
    }
    if app.selection.is_some() || app.screen_selection.is_some() {
        segments.push(Segment::new(1, "Ctrl+C copies the selection", theme::accent()).or_short("^C copy"));
    } else if app.dragging() {
        segments.push(Segment::new(1, crate::terminal_hint::native_selection_hint().to_owned(), theme::muted()));
    }
    if app.demo {
        segments.push(Segment::new(1, "DEMO", theme::attention()));
    }
    if !app.demo {
        let (text, style) = match app.permission_mode {
            recode_core::protocol::PermissionMode::Ask => ("ask", theme::muted()),
            recode_core::protocol::PermissionMode::AcceptEdits => ("accept edits", theme::accent()),
            recode_core::protocol::PermissionMode::Plan => ("plan ⏸", theme::accent()),
            recode_core::protocol::PermissionMode::AutoSandbox => {
                ("AUTO · sandbox", theme::attention().add_modifier(Modifier::BOLD))
            }
        };
        segments.push(Segment::new(1, text.to_owned(), style));
    }
    let model = format!("{} · {}", app.model, app.provider);
    segments.push(Segment::new(4, model, theme::muted()).or_short(app.model.clone()));
    if let Some((used, limit)) = &app.context {
        let text = context_text(used, *limit);
        let (room, style) = context_room(app);
        segments.push(Segment::new(3, format!("ctx {text}{room}"), style).or_short(format!("ctx {text}")));
    }
    if let Some(speed) = app.tokens_per_second {
        segments.push(Segment::new(6, format!("{speed:.0} tok/s"), theme::muted()));
    }
    if matches!(app.transcript.scroll, crate::transcript::Scroll::At { .. }) {
        segments.push(Segment::new(2, "End: latest", theme::accent()));
    }
    if app.select_mode {
        segments.push(
            Segment::new(1, "SELECT: the mouse is the terminal's · F2 takes it back", theme::attention())
                .or_short("SELECT · F2"),
        );
    }
    if app.is_busy() && app.state != AgentState::AwaitingApproval {
        segments.push(Segment::new(2, "esc stop", theme::muted()));
    }
    // With the side panel hidden, the task list is summed up here instead.
    if !app.panel_shown && !app.todos.is_empty() {
        let done = app.todos.iter().filter(|t| t.status == recode_core::protocol::TodoStatus::Done).count();
        segments.push(Segment::new(3, format!("tasks {done}/{}", app.todos.len()), theme::muted()));
    }
    status::line(&segments, width, theme::faint())
}

/// A centred window where there's room, and the whole screen where there isn't.
pub(crate) fn overlay_area(area: Rect, wanted_width: u16, wanted_height: u16) -> Rect {
    let width = if area.width < wanted_width + 4 { area.width } else { wanted_width };
    let height = if area.height < wanted_height + 2 { area.height } else { wanted_height };
    Rect::new(area.x + (area.width - width) / 2, area.y + (area.height - height) / 2, width, height)
}

fn draw_overlay(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let Some(overlay) = app.overlay.clone() else { return };
    match overlay {
        Overlay::Help { offset } => {
            let clamped = draw_help(frame, area, offset);
            if let Some(Overlay::Help { offset }) = &mut app.overlay {
                *offset = clamped;
            }
        }
        Overlay::Context => draw_context(frame, area, app),
        Overlay::Compactions { reports, selected } => draw_compactions(frame, area, &reports, selected),
        Overlay::Memory { files, user_path, project_path, selected } => {
            draw_memory(frame, area, &files, &user_path, project_path.as_deref(), selected);
        }
        Overlay::Permissions { mode, session, project, selected } => {
            draw_permissions(frame, area, mode, &session, &project, selected);
        }
        Overlay::HistorySearch { query, all_projects, selected } => {
            draw_history_search(frame, area, app, &query, all_projects, selected);
        }
        Overlay::Tasks { tasks, selected } => draw_tasks(frame, area, &tasks, selected),
        Overlay::Transcript { offset, typing, query, matches, current } => {
            let clamped = draw_transcript_mode(frame, area, app, offset, typing.as_deref(), &query, &matches, current);
            if let Some(Overlay::Transcript { offset, .. }) = &mut app.overlay {
                *offset = clamped;
            }
        }
        Overlay::Rewind { turns, selected, choice } => draw_rewind(frame, area, &turns, selected, choice),
        Overlay::NoteTarget { text, selected } => draw_note_target(frame, area, &text, selected),
        Overlay::Palette { query, selected } => draw_palette(frame, area, &query, selected, app),
        Overlay::Form(form) => draw_form(frame, area, &form),
        Overlay::Confirm(confirm) => draw_confirm(frame, area, &confirm),
        Overlay::McpPresets { selected } => draw_mcp_presets(frame, area, selected),
        Overlay::WebSetup { selected } => draw_web_setup(frame, area, selected),
        Overlay::Onboarding { step } => crate::onboarding::draw(frame, area, app, step),
        Overlay::McpSearch { query, hits, selected, searching, error } => {
            draw_mcp_search(frame, area, &query, &hits, selected, searching, error.as_deref());
        }
        Overlay::Install(state) => crate::install_view::draw_install(frame, area, &state, app.hub_progress.as_deref()),
        Overlay::PluginBrowse { title, listings, filter, selected, loading, error, ticked, category } => {
            crate::install_view::draw_plugin_browse(
                frame,
                area,
                &title,
                &listings,
                &filter,
                selected,
                loading,
                error.as_deref(),
                &ticked,
                category.as_deref(),
            );
        }
        Overlay::SkillSearch { query, hits, selected, searching, error, note, ticked, source, sources, total } => {
            let view = crate::install_view::SearchView {
                query: &query,
                hits: &hits,
                selected,
                searching,
                error: error.as_deref(),
                note: note.as_deref(),
                ticked: &ticked,
                source: source.as_deref(),
                sources: &sources,
                total,
                progress: app.hub_progress.as_deref(),
            };
            crate::install_view::draw_skill_search(frame, area, &view);
        }
        Overlay::ToolFilter { server, tools, selected } => draw_tool_filter(frame, area, &server, &tools, selected),
        Overlay::Sessions { sessions, selected } => draw_sessions(frame, area, &sessions, selected),
        Overlay::Themes { selected, .. } => draw_themes(frame, area, selected, &app.theme),
        Overlay::FreeSetup(step) => draw_free_setup(frame, area, &step, app),
        Overlay::CodeBlocks { blocks, selected } => draw_code_blocks(frame, area, &blocks, selected),
        Overlay::ProviderPresets { selected } => draw_provider_presets(frame, area, selected),
        Overlay::Pager { title, text, is_diff, side_by_side, offset, search } => {
            let clamped = draw_pager(frame, area, &title, &text, is_diff, side_by_side, offset, &search);
            if let Some(Overlay::Pager { offset, .. }) = &mut app.overlay {
                *offset = clamped;
            }
        }
    }
}

pub(crate) fn boxed(frame: &mut Frame<'_>, rect: Rect, title: &str, lines: Vec<Line<'static>>) {
    frame.render_widget(Clear, rect);
    let width = usize::from(rect.width);
    let inner = width.saturating_sub(4);
    let border = theme::accent();
    let title = truncate(title, width.saturating_sub(4));
    let (top_left, top_right, bottom_left, bottom_right) = theme::corners();
    let top = format!("{top_left} {title} {}{top_right}", "─".repeat(width.saturating_sub(title.width() + 4)));
    let mut out = vec![Line::styled(top, border)];
    let body_rows = usize::from(rect.height).saturating_sub(2);
    for index in 0..body_rows {
        let mut spans = vec![Span::styled("│ ", border)];
        let mut used = 0;
        if let Some(line) = lines.get(index) {
            used = line_width(line);
            spans.extend(line.spans.iter().cloned());
        }
        spans.push(Span::raw(" ".repeat(inner.saturating_sub(used))));
        spans.push(Span::styled(" │", border));
        out.push(Line::from(spans));
    }
    if rect.height >= 2 {
        out.push(Line::styled(format!("{bottom_left}{}{bottom_right}", "─".repeat(width.saturating_sub(2))), border));
    }
    frame.render_widget(Paragraph::new(out), rect);
}

fn draw_form(frame: &mut Frame<'_>, area: Rect, form: &FormState) {
    let rows_wanted = u16::try_from(form.fields.len() * 3 + 5).unwrap_or(u16::MAX);
    let rect = overlay_area(area, 76, rows_wanted);
    let inner_width = usize::from(rect.width).saturating_sub(4);
    let body_rows = usize::from(rect.height).saturating_sub(2);
    // When space is short, only the field being edited is shown.
    let compact = body_rows < form.fields.len() * 3 + 3;
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut cursor_row = 0;
    let mut cursor_column = 0;
    let mut show_cursor = false;
    for (index, field) in form.fields.iter().enumerate() {
        let focused = index == form.focus;
        if compact && !focused {
            continue;
        }
        let label_style = if focused { theme::accent().add_modifier(Modifier::BOLD) } else { theme::muted() };
        lines.push(Line::styled(truncate(field.label, inner_width), label_style));
        let room = inner_width.saturating_sub(4);
        if !field.choices.is_empty() {
            // A choice field shows its value between arrows and takes no cursor.
            let value = truncate(&field.value, room);
            lines.push(Line::from(vec![
                Span::styled("‹ ", label_style),
                Span::raw(value),
                Span::styled(" ›", label_style),
            ]));
        } else {
            // A secret shows dots; a long value scrolls so its end, where the cursor is, stays visible.
            let shown = if field.secret { "•".repeat(field.value.chars().count()) } else { field.value.clone() };
            let mut visible = shown;
            while visible.width() > room {
                visible.remove(0);
            }
            if focused {
                cursor_row = lines.len();
                cursor_column = visible.width() + 2;
                show_cursor = true;
            }
            lines.push(Line::from(vec![Span::styled("❯ ", label_style), Span::raw(visible)]));
        }
        if focused && body_rows > 3 {
            lines.push(Line::styled(truncate(field.hint, inner_width), theme::faint()));
        } else if !compact {
            lines.push(Line::default());
        }
    }
    if let Some(error) = &form.error {
        lines.extend(wrap_plain(error, Style::new().fg(theme::color(Token::Danger)), inner_width));
    }
    lines.truncate(body_rows.saturating_sub(1).max(1));
    lines.push(legend(&[("enter", "next"), ("^S", "save"), ("esc", "cancel")], inner_width));
    boxed(frame, rect, &form.title, lines);
    let x = rect.x + 2 + u16::try_from(cursor_column).unwrap_or(0);
    let y = rect.y + 1 + u16::try_from(cursor_row).unwrap_or(0);
    if show_cursor && x < rect.x + rect.width.saturating_sub(1) && y < rect.y + rect.height.saturating_sub(1) {
        frame.set_cursor_position((x, y));
    }
}

fn draw_themes(frame: &mut Frame<'_>, area: Rect, selected: usize, selection: &crate::theme::ThemeSelection) {
    let themes = crate::theme::themes();
    let rect = overlay_area(area, 66, u16::try_from(themes.len().min(14) + 6).unwrap_or(16));
    let inner = usize::from(rect.width).saturating_sub(4);
    let rows = usize::from(rect.height).saturating_sub(6).max(1);
    let first = (selected + 1).saturating_sub(rows);
    let mut lines = Vec::new();
    for (index, palette) in themes.iter().enumerate().skip(first).take(rows) {
        let is_selected = index == selected;
        let base =
            if is_selected { Style::new().bg(color(Token::UserBg)).add_modifier(Modifier::BOLD) } else { Style::new() };
        let tag = if palette.light { "light" } else { "dark" };
        // Accent, pink, violet, sky, success, danger, as this theme shows them.
        let swatch: Vec<Span<'static>> = [0, 2, 3, 4, 5, 6]
            .iter()
            .map(|&token| Span::styled("██", Style::new().fg(palette.colors[token])))
            .collect();
        let swatch_width = if inner >= 44 { 12 } else { 0 };
        let variant =
            if is_selected && inner >= 48 { format!("  ‹ {} ›", selection.variant.label()) } else { String::new() };
        let name_room = inner.saturating_sub(2 + variant.width() + swatch_width + 2 + tag.len() + 1);
        let name = truncate(&palette.name, name_room);
        let fill = inner.saturating_sub(2 + name.width() + variant.width() + swatch_width + 1 + tag.len());
        let mut spans = vec![
            Span::styled(if is_selected { "❯ " } else { "  " }, theme::accent().patch(base)),
            Span::styled(name, base),
            Span::styled(variant, theme::muted().patch(base)),
            Span::styled(" ".repeat(fill), base),
        ];
        if swatch_width > 0 {
            spans.extend(swatch);
        }
        spans.push(Span::styled(format!(" {tag}"), theme::muted().patch(base)));
        lines.push(Line::from(spans));
    }
    lines.push(Line::default());
    let accent = match selection.accent.and_then(crate::theme::hex) {
        Some(hex) => format!("accent {hex}"),
        None => "accent: theme's own".to_owned(),
    };
    let background = if selection.solid { "background on" } else { "background off" };
    lines.push(legend(&[("↑↓", "theme"), ("←→", "dim/bright"), ("a", &accent), ("b", background)], inner));
    lines.push(legend(&[("enter", "keep"), ("esc", "cancel")], inner));
    boxed(frame, rect, "Themes", lines);
}

fn draw_code_blocks(frame: &mut Frame<'_>, area: Rect, blocks: &[(String, String)], selected: usize) {
    let rect = overlay_area(area, 72, u16::try_from(blocks.len().min(12) + 4).unwrap_or(10));
    let inner = usize::from(rect.width).saturating_sub(4);
    let rows = usize::from(rect.height).saturating_sub(4).max(1);
    let first = (selected + 1).saturating_sub(rows);
    let mut lines = Vec::new();
    for (index, (language, code)) in blocks.iter().enumerate().skip(first).take(rows) {
        let base = if index == selected {
            Style::new().bg(color(Token::UserBg)).add_modifier(Modifier::BOLD)
        } else {
            Style::new()
        };
        let count = code.lines().count().max(1);
        let language = if language.is_empty() { "code" } else { language.as_str() };
        let preview = code.lines().find(|line| !line.trim().is_empty()).unwrap_or_default().trim();
        let head = format!("{}. {language} · {count} line{}  ", index + 1, if count == 1 { "" } else { "s" });
        let preview = truncate(preview, inner.saturating_sub(2 + head.width()));
        let fill = inner.saturating_sub(2 + head.width() + preview.width());
        lines.push(Line::from(vec![
            Span::styled(if index == selected { "❯ " } else { "  " }, theme::accent().patch(base)),
            Span::styled(head, base),
            Span::styled(preview, theme::muted().patch(base)),
            Span::styled(" ".repeat(fill), base),
        ]));
    }
    lines.push(Line::default());
    lines.push(legend(&[("enter", "copy"), ("1-9", "copy that one"), ("esc", "cancel")], inner));
    boxed(frame, rect, "Copy which code block?", lines);
}

fn draw_free_setup(frame: &mut Frame<'_>, area: Rect, step: &crate::app::FreeStep, app: &App) {
    use crate::app::FreeStep;
    match step {
        FreeStep::Pick { selected } => {
            let presets = FreeStep::free_presets();
            let rect = overlay_area(area, 72, u16::try_from(presets.len() * 3 + 5).unwrap_or(12));
            let inner = usize::from(rect.width).saturating_sub(4);
            let mut lines = wrap_plain(
                "Free models need a free account key. Pick a provider: its sign-up page opens, then paste the key here.",
                theme::muted(),
                inner,
            );
            lines.push(Line::default());
            for (index, preset) in presets.iter().enumerate() {
                let base = if index == *selected {
                    Style::new().bg(theme::color(Token::UserBg)).add_modifier(Modifier::BOLD)
                } else {
                    Style::new()
                };
                let probe = app.extensions.as_ref().and_then(|e| e.provider_probes.get(preset.name));
                let status = match probe {
                    Some(recode_core::extensions::ProviderProbe::Reachable { free_model_count, .. }) => {
                        format!("{free_model_count} free models now")
                    }
                    _ => String::new(),
                };
                let name = truncate(preset.name, inner.saturating_sub(2 + status.width() + 2));
                let fill = inner.saturating_sub(2 + name.width() + status.width());
                lines.push(Line::from(vec![
                    Span::styled(if index == *selected { "❯ " } else { "  " }, theme::accent().patch(base)),
                    Span::styled(name, base),
                    Span::styled(" ".repeat(fill), base),
                    Span::styled(status, Style::new().fg(theme::color(Token::Success)).patch(base)),
                ]));
                lines.push(Line::styled(
                    format!("    {}", truncate(preset.note, inner.saturating_sub(4))),
                    theme::muted(),
                ));
            }
            lines.push(Line::default());
            lines.push(legend(&[("enter", "open sign-up page"), ("esc", "cancel")], inner));
            boxed(frame, rect, "Get free models", lines);
        }
        FreeStep::Key { name, signup_url, key, .. } => {
            let rect = overlay_area(area, 72, 11);
            let inner = usize::from(rect.width).saturating_sub(4);
            let mut lines = Vec::new();
            if let Some(url) = signup_url {
                lines.extend(wrap_plain(&format!("1. Sign in and create a key at {url}"), Style::new(), inner));
                lines.extend(wrap_plain("   (Ctrl+O opens it again)", theme::muted(), inner));
            } else {
                lines.extend(wrap_plain(&format!("Create a key on {name}'s website."), Style::new(), inner));
            }
            lines.extend(wrap_plain("2. Paste it here (Ctrl+V or middle-click):", Style::new(), inner));
            // Never the key itself: its length, as dots.
            let masked: String = "•".repeat(key.chars().count().min(inner.saturating_sub(4)));
            lines.push(Line::from(vec![
                Span::styled(" ❯ ", theme::accent()),
                Span::styled(masked, Style::new().fg(theme::color(Token::Attention))),
            ]));
            lines.push(Line::default());
            lines.extend(wrap_plain("Keys are kept in your system keyring, never in a file.", theme::muted(), inner));
            lines.push(Line::default());
            lines.push(legend(&[("enter", "save and check"), ("esc", "back")], inner));
            boxed(frame, rect, &format!("Key for {name}"), lines);
        }
    }
}

fn draw_sessions(frame: &mut Frame<'_>, area: Rect, sessions: &[crate::link::SessionSummary], selected: usize) {
    let wanted = u16::try_from(sessions.len().min(12) + 4).unwrap_or(16);
    let rect = overlay_area(area, 80, wanted);
    let inner = usize::from(rect.width).saturating_sub(4);
    let rows = usize::from(rect.height).saturating_sub(4).max(1);
    let first = (selected + 1).saturating_sub(rows);
    let mut lines = Vec::new();
    for (index, session) in sessions.iter().enumerate().skip(first).take(rows) {
        let base = if index == selected {
            Style::new().bg(theme::color(Token::UserBg)).add_modifier(Modifier::BOLD)
        } else {
            Style::new()
        };
        let detail = format!("{} · {} messages", session.age, session.messages);
        let room = inner.saturating_sub(2 + detail.width() + 2);
        let (title, detail) = if room >= 12 {
            (truncate(&session.title, room), detail)
        } else {
            (truncate(&session.title, inner.saturating_sub(2)), String::new())
        };
        let fill = inner.saturating_sub(2 + title.width() + detail.width());
        lines.push(Line::from(vec![
            Span::styled(if index == selected { "❯ " } else { "  " }, theme::accent().patch(base)),
            Span::styled(title, base),
            Span::styled(" ".repeat(fill), base),
            Span::styled(detail, theme::muted().patch(base)),
        ]));
    }
    lines.push(Line::default());
    lines.push(legend(&[("enter", "continue it"), ("d", "delete"), ("esc", "cancel")], inner));
    boxed(frame, rect, "Sessions in this folder", lines);
}

fn draw_mcp_presets(frame: &mut Frame<'_>, area: Rect, selected: usize) {
    use recode_core::extensions::MCP_PRESETS;
    let rect = overlay_area(area, 72, u16::try_from(MCP_PRESETS.len() * 2 + 3).unwrap_or(10));
    let inner = usize::from(rect.width).saturating_sub(4);
    let mut lines = Vec::new();
    for (index, preset) in MCP_PRESETS.iter().enumerate() {
        let base = if index == selected {
            Style::new().bg(theme::color(Token::UserBg)).add_modifier(Modifier::BOLD)
        } else {
            Style::new()
        };
        lines.push(Line::from(vec![
            Span::styled(if index == selected { "❯ " } else { "  " }, theme::accent().patch(base)),
            Span::styled(truncate(preset.name, inner.saturating_sub(2)), base),
        ]));
        lines.push(Line::styled(format!("    {}", truncate(preset.note, inner.saturating_sub(4))), theme::muted()));
    }
    lines.push(Line::default());
    lines.push(legend(&[("enter", "fill the form"), ("esc", "cancel")], inner));
    boxed(frame, rect, "MCP presets", lines);
}

/// `/web`: search servers the model can use, with what each needs. Enter fills the MCP form.
fn draw_web_setup(frame: &mut Frame<'_>, area: Rect, selected: usize) {
    use recode_core::extensions::WEB_SEARCH_PRESETS;
    let rect = overlay_area(area, 84, u16::try_from(WEB_SEARCH_PRESETS.len() * 3 + 6).unwrap_or(24));
    let inner = usize::from(rect.width).saturating_sub(4);
    let mut lines = wrap_plain(
        "Web search reaches the model as an MCP server. Pick one; the form opens with the command filled in, and your address or key stays in your own configuration.",
        theme::muted(),
        inner,
    );
    lines.push(Line::default());
    let visible = (usize::from(rect.height).saturating_sub(lines.len() + 4) / 3).max(1);
    let first =
        selected.saturating_sub(visible.saturating_sub(1)).min(WEB_SEARCH_PRESETS.len().saturating_sub(visible));
    for (index, preset) in WEB_SEARCH_PRESETS.iter().enumerate().skip(first).take(visible) {
        let base = if index == selected {
            Style::new().bg(theme::color(Token::UserBg)).add_modifier(Modifier::BOLD)
        } else {
            Style::new()
        };
        let keyless = preset.settings.is_empty() || !preset.settings.contains("KEY");
        lines.push(Line::from(vec![
            Span::styled(if index == selected { "❯ " } else { "  " }, theme::accent().patch(base)),
            Span::styled(truncate(preset.name, inner.saturating_sub(14)), base),
            Span::raw("  "),
            crate::widgets::chip(
                if keyless { "no key" } else { "key" },
                if keyless { theme::color(Token::Success) } else { theme::color(Token::Attention) },
            ),
        ]));
        lines.push(Line::styled(format!("    {}", truncate(preset.note, inner.saturating_sub(4))), theme::muted()));
        lines.push(Line::styled(
            format!("    needs: {}", truncate(preset.needs, inner.saturating_sub(11))),
            theme::faint(),
        ));
    }
    lines.push(Line::default());
    lines.push(legend(&[("enter", "fill the form"), ("↑↓", "choose"), ("esc", "cancel")], inner));
    boxed(frame, rect, "Web search for the model", lines);
}

fn draw_provider_presets(frame: &mut Frame<'_>, area: Rect, selected: usize) {
    use recode_core::providers::PROVIDER_PRESETS;
    let rect = overlay_area(area, 76, u16::try_from(PROVIDER_PRESETS.len() * 2 + 3).unwrap_or(10));
    let inner = usize::from(rect.width).saturating_sub(4);
    // Two rows per preset, after the border, a blank and the legend. The window keeps the selection in view.
    let visible = (usize::from(rect.height).saturating_sub(4) / 2).max(1);
    let first = selected.saturating_sub(visible.saturating_sub(1)).min(PROVIDER_PRESETS.len().saturating_sub(visible));
    let mut lines = Vec::new();
    for (index, preset) in PROVIDER_PRESETS.iter().enumerate().skip(first).take(visible) {
        let base = if index == selected {
            Style::new().bg(theme::color(Token::UserBg)).add_modifier(Modifier::BOLD)
        } else {
            Style::new()
        };
        lines.push(Line::from(vec![
            Span::styled(if index == selected { "❯ " } else { "  " }, theme::accent().patch(base)),
            Span::styled(format!("{}  ", preset.name), base),
            Span::styled(preset.kind.label(), theme::muted().patch(base)),
        ]));
        lines.push(Line::styled(format!("    {}", truncate(preset.note, inner.saturating_sub(4))), theme::muted()));
    }
    lines.push(Line::default());
    lines.push(legend(&[("enter", "fill the form"), ("esc", "cancel")], inner));
    boxed(frame, rect, "Provider presets", lines);
}

fn draw_confirm(frame: &mut Frame<'_>, area: Rect, confirm: &ConfirmState) {
    // Sized to the text: a short question stays small, a file preview gets the room it needs.
    let width = if area.width < 74 { area.width } else { 70 };
    let inner_width = usize::from(width).saturating_sub(4);
    let mut lines = Vec::new();
    for (index, text) in confirm.body.iter().enumerate() {
        if text.is_empty() {
            lines.push(Line::default());
            continue;
        }
        lines.extend(wrap_plain(text, Style::new(), inner_width));
        // Prose paragraphs are spaced out; indented rows (a list, a file preview) stay together.
        let next_is_row = confirm.body.get(index + 1).is_some_and(|next| next.starts_with("  ") || next.is_empty());
        if !text.starts_with("  ") && !next_is_row {
            lines.push(Line::default());
        }
    }
    let wanted = u16::try_from(lines.len() + 3).unwrap_or(u16::MAX).clamp(6, 40);
    let rect = overlay_area(area, 70, wanted);
    let body_rows = usize::from(rect.height).saturating_sub(2);
    lines.truncate(body_rows.saturating_sub(1).max(1));
    let keys: &[(&str, &str)] = if matches!(confirm.command, crate::form::Confirmed::StartRuntime(_)) {
        &[("y", "yes"), ("a", "yes, and at every launch"), ("n", "no")]
    } else {
        &[("y", "yes"), ("n", "no")]
    };
    lines.push(legend(keys, inner_width));
    boxed(frame, rect, &confirm.title, lines);
}

const HELP: &[(&str, &str)] = &[
    ("Enter", "Send"),
    ("Alt+Enter, Ctrl+J", "New line"),
    ("Ctrl+Backspace", "Delete the word before the cursor (also Alt+Backspace; Ctrl+W back to a space)"),
    ("Esc", "Stop the agent, or close a window"),
    ("Ctrl+K", "Commands"),
    ("Ctrl+L", "Switch model"),
    ("Ctrl+E", "MCP servers, skills and tools"),
    ("Tab", "Next tab on the home screen"),
    ("Ctrl+B", "Side panel: session, files, hidden"),
    ("Ctrl+T", "Show or hide reasoning"),
    ("Ctrl+O", "Last tool output"),
    ("Shift+Tab", "Permission mode: ask, accept edits, plan, auto (sandbox)"),
    ("Enter while working", "Queue the message; it's sent after the current tool calls, or next"),
    ("Esc Esc", "Go back to an earlier request (also /rewind)"),
    ("Drag", "Select and copy text anywhere on screen (Ctrl+C copies again)"),
    ("Shift+Drag", "Select with the terminal itself; F2 hands it the mouse for good"),
    ("Ctrl+R", "Search earlier prompts"),
    ("Ctrl+X", "Transcript mode: search the whole conversation; [ writes it into the terminal's scrollback"),
    ("! command", "Run a command in the sandbox yourself (!! with network, a trailing & for the background)"),
    ("y / a / p / n", "Approve once, always this session, always for this project, deny"),
    ("Click", "Open or close a tool's output, or reasoning"),
    ("Drag", "Select text and copy it"),
    ("Double, triple click", "Copy a word, or a whole message"),
    ("Middle-click", "Paste the selection"),
    ("Ctrl+End, Ctrl+click", "Jump to the latest"),
    ("/  @", "Commands, and mention a file"),
    ("Up on empty prompt", "Earlier prompts"),
    ("Ctrl+Z", "Undo the last turn's file changes"),
    ("Ctrl+Y", "Copy the last code block"),
    ("PgUp, PgDn, wheel", "Scroll"),
    ("Home, End", "Start, or latest output"),
    ("F2", "Select text with the terminal"),
    ("Ctrl+C", "Copy a selection, or clear input. Twice to quit."),
];

/// `/tasks`: the background commands, with their state and last line.
fn draw_tasks(frame: &mut Frame<'_>, area: Rect, tasks: &[crate::link::TaskInfo], selected: usize) {
    let wanted = u16::try_from(tasks.len().clamp(1, 10) * 2 + 4).unwrap_or(16);
    let rect = overlay_area(area, 84, wanted);
    let inner = usize::from(rect.width).saturating_sub(4);
    let rows = usize::from(rect.height).saturating_sub(4).max(2) / 2;
    let first = (selected + 1).saturating_sub(rows);
    let mut lines = Vec::new();
    if tasks.is_empty() {
        lines.extend(wrap_plain(
            "No background commands. The model starts one with shell's background option; you can with `!command &`.",
            theme::muted(),
            inner,
        ));
    }
    for (index, task) in tasks.iter().enumerate().skip(first).take(rows) {
        let base = if index == selected {
            Style::new().bg(theme::color(Token::UserBg)).add_modifier(Modifier::BOLD)
        } else {
            Style::new()
        };
        let status_style = match task.status.as_str() {
            "running" => theme::accent(),
            "finished" => Style::new().fg(theme::color(Token::Success)),
            _ => theme::attention(),
        };
        let status = format!("{} · {} s", task.status, task.elapsed_secs);
        let room = inner.saturating_sub(2 + status.width() + 2);
        let head = truncate(&format!("{}  {}", task.id, task.command), room.max(8));
        let fill = inner.saturating_sub(2 + head.width() + status.width());
        lines.push(Line::from(vec![
            Span::styled(if index == selected { "❯ " } else { "  " }, theme::accent().patch(base)),
            Span::styled(head, base),
            Span::styled(" ".repeat(fill), base),
            Span::styled(status, status_style.patch(base)),
        ]));
        lines.push(Line::styled(format!("    {}", truncate(&task.last_line, inner.saturating_sub(4))), theme::muted()));
    }
    lines.push(Line::default());
    lines.push(legend(&[("enter", "output"), ("k", "stop"), ("esc", "close")], inner));
    boxed(frame, rect, "Background commands", lines);
}

/// Ctrl+X: the whole conversation in a full-screen pager with search. Returns the clamped offset.
#[allow(clippy::too_many_arguments)]
fn draw_transcript_mode(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    offset: usize,
    typing: Option<&str>,
    query: &str,
    matches: &[usize],
    current: usize,
) -> usize {
    let inner = usize::from(area.width).saturating_sub(4);
    let total = app.transcript.total_lines(inner, app.options);
    let mut lines = app.transcript.lines_between(inner, app.options, 0, total);
    for line_index in matches {
        if let Some(line) = lines.get_mut(*line_index) {
            for span in &mut line.spans {
                span.style = span.style.add_modifier(Modifier::REVERSED);
            }
        }
    }
    let body = usize::from(area.height).saturating_sub(2);
    let offset = offset.min(lines.len().saturating_sub(body));
    let mut heading = format!("Transcript · line {} of {}", offset + 1, lines.len());
    match typing {
        Some(text) => heading.push_str(&format!(" · search: {text}▏")),
        None if !query.is_empty() => heading.push_str(&format!(
            " · \"{query}\": {} of {}",
            if matches.is_empty() { 0 } else { current + 1 },
            matches.len()
        )),
        None => {}
    }
    if area.width >= 70 && typing.is_none() {
        heading.push_str("  (/ search · n/N · [ to scrollback · v editor · q closes)");
    }
    let visible: Vec<_> = lines.into_iter().skip(offset).take(body).collect();
    boxed(frame, area, &heading, visible);
    offset
}

/// Ctrl+R: earlier prompts, filtered as the user types.
fn draw_history_search(frame: &mut Frame<'_>, area: Rect, app: &App, query: &str, all_projects: bool, selected: usize) {
    let matches = app.history_matches(query, all_projects);
    let wanted = u16::try_from(matches.len().clamp(1, 12) + 5).unwrap_or(17);
    let rect = overlay_area(area, 84, wanted);
    let inner = usize::from(rect.width).saturating_sub(4);
    let rows = usize::from(rect.height).saturating_sub(5).max(1);
    let selected = selected.min(matches.len().saturating_sub(1));
    let first = (selected + 1).saturating_sub(rows);
    let scope = if all_projects { "every project" } else { "this project" };
    let mut lines = vec![Line::from(vec![
        Span::styled("search ", theme::muted()),
        Span::raw(query.to_owned()),
        Span::styled("▏", theme::accent()),
        Span::styled(format!("   {scope}"), theme::faint()),
    ])];
    if matches.is_empty() {
        let text = if all_projects && app.history_all.is_none() {
            "Loading prompts from every project…"
        } else if query.is_empty() {
            "No earlier prompts here yet."
        } else {
            "Nothing matches."
        };
        lines.push(Line::styled(text, theme::muted()));
    }
    for (index, prompt) in matches.iter().enumerate().skip(first).take(rows) {
        let base = if index == selected {
            Style::new().bg(theme::color(Token::UserBg)).add_modifier(Modifier::BOLD)
        } else {
            Style::new()
        };
        let line = prompt.lines().next().unwrap_or_default();
        lines.push(Line::from(vec![
            Span::styled(if index == selected { "❯ " } else { "  " }, theme::accent().patch(base)),
            Span::styled(truncate(line, inner.saturating_sub(2)), base),
        ]));
    }
    lines.push(Line::default());
    lines.push(legend(&[("enter", "use it"), ("tab", "scope"), ("esc", "close")], inner));
    boxed(frame, rect, "Earlier prompts", lines);
}

/// Esc Esc: which request to go back to, and what to restore.
fn draw_rewind(frame: &mut Frame<'_>, area: Rect, turns: &[(usize, String)], selected: usize, choice: usize) {
    let wanted = u16::try_from(turns.len().clamp(1, 10) + 9).unwrap_or(19);
    let rect = overlay_area(area, 84, wanted);
    let inner = usize::from(rect.width).saturating_sub(4);
    let rows = usize::from(rect.height).saturating_sub(9).max(1);
    let first = (selected + 1).saturating_sub(rows);
    let mut lines = vec![Line::styled("Go back to before this request:", theme::muted())];
    for (index, (turn, title)) in turns.iter().enumerate().skip(first).take(rows) {
        let base = if index == selected {
            Style::new().bg(theme::color(Token::UserBg)).add_modifier(Modifier::BOLD)
        } else {
            Style::new()
        };
        let label = format!("{}. {}", turn + 1, title);
        lines.push(Line::from(vec![
            Span::styled(if index == selected { "❯ " } else { "  " }, theme::accent().patch(base)),
            Span::styled(truncate(&label, inner.saturating_sub(2)), base),
        ]));
    }
    lines.push(Line::default());
    let choices = [
        "Restore the conversation (files stay as they are)",
        "Restore the conversation and put the files back",
        "Summarize from here: keep a summary of what's removed",
    ];
    for (index, text) in choices.iter().enumerate() {
        let marker = if index == choice { "◉ " } else { "○ " };
        let style = if index == choice { theme::accent() } else { theme::muted() };
        lines.push(Line::styled(format!("{marker}{}", truncate(text, inner.saturating_sub(2))), style));
    }
    lines.push(Line::default());
    lines.push(legend(&[("↑↓", "request"), ("←→", "what to restore"), ("enter", "go"), ("esc", "cancel")], inner));
    boxed(frame, rect, "Rewind", lines);
}

/// `/permissions`: the mode, what this session allowed, and what the project remembers.
fn draw_permissions(
    frame: &mut Frame<'_>,
    area: Rect,
    mode: recode_core::protocol::PermissionMode,
    session: &[String],
    project: &[(String, String)],
    selected: usize,
) {
    let wanted = u16::try_from(session.len() + project.len() + 10).unwrap_or(20).min(30);
    let rect = overlay_area(area, 84, wanted);
    let inner = usize::from(rect.width).saturating_sub(4);
    let mut lines = Vec::new();
    lines.extend(wrap_plain(&format!("Mode: {} — {}", mode.label(), mode.describe()), Style::new(), inner));
    lines.push(Line::default());
    lines.push(Line::styled("Allowed for this session (y/a on an approval):", theme::muted()));
    if session.is_empty() {
        lines.push(Line::styled("  nothing yet", theme::faint()));
    }
    for grant in session {
        lines.push(Line::raw(format!("  {}", truncate(grant, inner.saturating_sub(2)))));
    }
    lines.push(Line::default());
    lines.push(Line::styled("Remembered for this project (p on an approval; d forgets):", theme::muted()));
    if project.is_empty() {
        lines.push(Line::styled("  nothing yet", theme::faint()));
    }
    for (index, (_, label)) in project.iter().enumerate() {
        let base = if index == selected {
            Style::new().bg(theme::color(Token::UserBg)).add_modifier(Modifier::BOLD)
        } else {
            Style::new()
        };
        lines.push(Line::from(vec![
            Span::styled(if index == selected { "❯ " } else { "  " }, theme::accent().patch(base)),
            Span::styled(truncate(label, inner.saturating_sub(2)), base),
        ]));
    }
    lines.push(Line::default());
    lines.push(legend(&[("⇧tab", "next mode"), ("d", "forget"), ("esc", "close")], inner));
    boxed(frame, rect, "Permissions", lines);
}

/// `/memory`: the instruction files in force, with their cost, trust and problems.
fn draw_memory(
    frame: &mut Frame<'_>,
    area: Rect,
    files: &[crate::link::InstructionFileInfo],
    user_path: &str,
    project_path: Option<&str>,
    selected: usize,
) {
    let wanted = u16::try_from(files.len().clamp(1, 10) * 2 + 7).unwrap_or(18);
    let rect = overlay_area(area, 84, wanted);
    let inner = usize::from(rect.width).saturating_sub(4);
    let rows = usize::from(rect.height).saturating_sub(7).max(2) / 2;
    let first = (selected + 1).saturating_sub(rows);
    let mut lines = Vec::new();
    if files.is_empty() {
        lines.extend(wrap_plain(
            "No instruction files or memories yet. Instruction files are read with every request: RE-CODE.md (also \
             AGENTS.md or CLAUDE.md) in the project, and your own in re-code's config folder. Memories are what \
             re-code saves on its own while it works.",
            theme::muted(),
            inner,
        ));
    }
    for (index, file) in files.iter().enumerate().skip(first).take(rows) {
        let base = if index == selected {
            Style::new().bg(theme::color(Token::UserBg)).add_modifier(Modifier::BOLD)
        } else {
            Style::new()
        };
        let status = match (&file.problem, file.trusted) {
            (Some(problem), _) => problem.clone(),
            (None, false) => "off until /trust".to_owned(),
            (None, true) => format!("~{} tokens", compact_tokens(file.tokens)),
        };
        let status_style = if file.problem.is_some() || !file.trusted { theme::attention() } else { theme::muted() };
        let room = inner.saturating_sub(2 + status.width() + 2);
        let shown = file.memory.as_ref().map_or(file.path.as_str(), |(_, name)| name.as_str());
        let path = truncate(shown, room.max(8));
        let fill = inner.saturating_sub(2 + path.width() + status.width());
        lines.push(Line::from(vec![
            Span::styled(if index == selected { "❯ " } else { "  " }, theme::accent().patch(base)),
            Span::styled(path, base),
            Span::styled(" ".repeat(fill), base),
            Span::styled(status, status_style.patch(base)),
        ]));
        lines.push(Line::styled(format!("    {}", file.scope), theme::muted()));
    }
    lines.push(Line::default());
    lines.extend(wrap_plain(&format!("Yours: {user_path}"), theme::muted(), inner));
    if let Some(project) = project_path {
        lines.extend(wrap_plain(&format!("Project: {project}"), theme::muted(), inner));
    }
    let on_memory = files.get(selected).is_some_and(|file| file.memory.is_some());
    let keys: &[(&str, &str)] = if on_memory {
        &[("enter", "edit"), ("d", "forget"), ("u", "yours"), ("esc", "close")]
    } else {
        &[("enter", "edit"), ("n", "new for this project"), ("u", "yours"), ("esc", "close")]
    };
    lines.push(legend(keys, inner));
    boxed(frame, rect, "Instructions and memory", lines);
}

/// A `# note`: which file it goes to.
fn draw_note_target(frame: &mut Frame<'_>, area: Rect, text: &str, selected: usize) {
    use crate::link::NoteScope;
    let rect = overlay_area(area, 72, 11);
    let inner = usize::from(rect.width).saturating_sub(4);
    let mut lines = wrap_plain(&format!("Remember: {text}"), Style::new(), inner);
    lines.truncate(3);
    lines.push(Line::default());
    for (index, scope) in NoteScope::ALL.iter().enumerate() {
        let base = if index == selected {
            Style::new().bg(theme::color(Token::UserBg)).add_modifier(Modifier::BOLD)
        } else {
            Style::new()
        };
        lines.push(Line::from(vec![
            Span::styled(if index == selected { "❯ " } else { "  " }, theme::accent().patch(base)),
            Span::styled(truncate(scope.label(), inner.saturating_sub(2)), base),
        ]));
    }
    lines.push(Line::default());
    lines.push(legend(&[("enter", "add it"), ("esc", "back to typing")], inner));
    boxed(frame, rect, "Add a note to the instructions", lines);
}

/// `/compactions`: what each compaction did, newest first; Enter shows what the summary replaced.
fn draw_compactions(
    frame: &mut Frame<'_>,
    area: Rect,
    reports: &[(String, recode_core::usage::CompactionReport)],
    selected: usize,
) {
    let wanted = u16::try_from(reports.len().clamp(1, 10) * 2 + 4).unwrap_or(16);
    let rect = overlay_area(area, 84, wanted);
    let inner = usize::from(rect.width).saturating_sub(4);
    let rows = usize::from(rect.height).saturating_sub(4).max(2) / 2;
    let first = (selected + 1).saturating_sub(rows);
    let mut lines = Vec::new();
    if reports.is_empty() {
        lines.extend(wrap_plain(
            "No compactions in this session yet. The conversation is compacted on its own as it nears the window, or \
             now with /compact.",
            theme::muted(),
            inner,
        ));
    }
    for (index, (when, report)) in reports.iter().enumerate().skip(first).take(rows) {
        let base = if index == selected {
            Style::new().bg(theme::color(Token::UserBg)).add_modifier(Modifier::BOLD)
        } else {
            Style::new()
        };
        let mut what = Vec::new();
        if report.elided > 0 {
            what.push(format!("shortened {}", report.elided));
        }
        if report.summarised > 0 {
            what.push(format!("summarised {}", report.summarised));
        }
        let head = format!(
            "#{} · {when} · ~{} → ~{} · {}",
            report.seq,
            compact_tokens(report.before_tokens),
            compact_tokens(report.after_tokens),
            what.join(", ")
        );
        lines.push(Line::from(vec![
            Span::styled(if index == selected { "❯ " } else { "  " }, theme::accent().patch(base)),
            Span::styled(truncate(&head, inner.saturating_sub(2)), base),
        ]));
        let mut detail = match &report.focus {
            Some(focus) => format!("focus: {focus}"),
            None => format!("kept: {}", report.kept_objective.lines().next().unwrap_or_default()),
        };
        if let Some(problem) = &report.problem {
            detail = format!("{problem} · {detail}");
        }
        lines.push(Line::styled(format!("    {}", truncate(&detail, inner.saturating_sub(4))), theme::muted()));
    }
    lines.push(Line::default());
    lines.push(legend(&[("enter", "what it replaced"), ("esc", "close")], inner));
    boxed(frame, rect, "Compactions", lines);
}

/// `/context`: what the last request held, part by part, against the window.
fn draw_context(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let ledger = app.ledger.clone();
    let rows = ledger.as_ref().map_or(0, |l| l.components.len() + l.largest_results.len().min(3));
    let rect = overlay_area(area, 76, u16::try_from(rows + 9).unwrap_or(16));
    let inner = usize::from(rect.width).saturating_sub(4);
    let body_rows = usize::from(rect.height).saturating_sub(2);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let Some(ledger) = ledger else {
        lines.extend(wrap_plain("Nothing has been sent yet.", theme::muted(), inner));
        boxed(frame, rect, "Context", lines);
        return;
    };
    let total = ledger.tokens();
    let window_text = ledger.window.map_or_else(|| "limit unknown".to_owned(), |w| format!("/ {}", compact_tokens(w)));
    let mut heading = format!("Context ~{} {window_text} (estimated)", compact_tokens(total));
    if let Some(usage) = &app.last_usage
        && let Some(prompt) = usage.prompt_tokens
    {
        let cached = usage.cached_prompt_tokens.map(|c| format!(", {c} from cache")).unwrap_or_default();
        heading.push_str(&format!(" · last reported {prompt} in{cached}"));
    }
    lines.extend(wrap_plain(&heading, Style::new().add_modifier(Modifier::BOLD), inner));
    lines.push(Line::default());
    // Rows: label, meter, tokens. When short on rows, the biggest parts come first.
    let mut components = ledger.components.clone();
    let available = body_rows.saturating_sub(lines.len() + 4);
    if components.len() > available {
        components.sort_by_key(|component| std::cmp::Reverse(component.tokens));
        components.truncate(available.max(1));
    }
    let scale = ledger.window.unwrap_or(total).max(1);
    let label_width = 22.min(inner / 2);
    let figure_width = 14;
    let meter_width = inner.saturating_sub(label_width + figure_width + 2);
    for component in &components {
        let label = truncate(component.kind.label(), label_width);
        let figure = if component.kind == recode_core::usage::ComponentKind::Images {
            format!("{} not counted", component.count)
        } else {
            format!("~{}", compact_tokens(component.tokens))
        };
        let mut spans = vec![Span::raw(format!("{label:<label_width$} "))];
        #[allow(clippy::cast_precision_loss)]
        let fraction = component.tokens as f64 / scale as f64;
        if meter_width >= 4 {
            spans.extend(meter(fraction, meter_width));
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(figure, theme::muted()));
        lines.push(Line::from(spans));
    }
    if !ledger.largest_results.is_empty() && lines.len() + 2 < body_rows {
        lines.push(Line::default());
        for (what, tokens) in ledger.largest_results.iter().take(body_rows.saturating_sub(lines.len() + 3)) {
            lines.push(Line::styled(
                format!("  {} · ~{}", truncate(what, inner.saturating_sub(12)), compact_tokens(*tokens)),
                theme::muted(),
            ));
        }
    }
    lines.push(Line::default());
    let reserve = format!("Reserved for the reply: {}", compact_tokens(ledger.reserve));
    let room = match ledger.trigger {
        Some(trigger) if trigger > total => format!("Free until compaction: ~{}", compact_tokens(trigger - total)),
        Some(_) => "Compaction is due on the next request".to_owned(),
        None if ledger.window.is_some() => "Automatic compaction is off".to_owned(),
        None => "No window known, so nothing compacts on its own".to_owned(),
    };
    lines.extend(wrap_plain(&format!("{reserve} · {room}"), theme::muted(), inner));
    lines.push(legend(&[("c", "compact now"), ("r", "raw request"), ("esc", "close")], inner));
    boxed(frame, rect, "Context", lines);
}

/// Draws the key list, windowed when the terminal is shorter than the list. Returns the clamped offset.
fn draw_help(frame: &mut Frame<'_>, area: Rect, offset: usize) -> usize {
    let rect = overlay_area(area, 60, u16::try_from(HELP.len()).unwrap_or(0) + 3);
    let inner = usize::from(rect.width).saturating_sub(4);
    let mut lines = Vec::new();
    for (keys, what) in HELP {
        if inner >= 40 {
            let what = truncate(what, inner.saturating_sub(20));
            lines.push(Line::from(vec![
                Span::styled(format!("{keys:<20}"), Style::new().add_modifier(Modifier::BOLD)),
                Span::raw(what),
            ]));
        } else {
            lines.push(Line::styled(truncate(keys, inner), Style::new().add_modifier(Modifier::BOLD)));
            lines.push(Line::styled(truncate(what, inner), theme::muted()));
        }
    }
    // Short terminals get a window onto the list, with the position in the title.
    let body = usize::from(rect.height).saturating_sub(2);
    let scrolls = lines.len() > body && body > 1;
    let offset = if scrolls { offset.min(lines.len().saturating_sub(body.saturating_sub(1))) } else { 0 };
    let title =
        if scrolls { format!("Keys · {} of {} · ↑↓ scroll", offset + 1, lines.len()) } else { "Keys".to_owned() };
    let visible: Vec<Line<'static>> =
        if scrolls { lines.into_iter().skip(offset).take(body.saturating_sub(1)).collect() } else { lines };
    boxed(frame, rect, &title, visible);
    offset
}

fn draw_palette(frame: &mut Frame<'_>, area: Rect, query: &str, selected: usize, app: &App) {
    let matches = app.palette_entries(query);
    let rect = overlay_area(area, 56, u16::try_from(crate::app::PALETTE.len()).unwrap_or(0) + 4);
    let inner = usize::from(rect.width).saturating_sub(4);
    let selected = selected.min(matches.len().saturating_sub(1));
    let mut lines = vec![
        Line::from(vec![Span::styled("❯ ", theme::accent()), Span::raw(truncate(query, inner.saturating_sub(2)))]),
        Line::default(),
    ];
    if matches.is_empty() {
        lines.push(Line::styled(truncate("No matching commands", inner), theme::muted()));
    }
    // On a short screen the list scrolls to keep the highlighted command in view.
    let rows = usize::from(rect.height).saturating_sub(4).max(1);
    let first = (selected + 1).saturating_sub(rows);
    for (index, (_, name, keys)) in matches.iter().enumerate().skip(first).take(rows) {
        let show_keys = inner >= name.width() + keys.width() + 2;
        let name = truncate(name, inner);
        let fill = inner.saturating_sub(name.width() + if show_keys { keys.width() } else { 0 });
        let base = if index == selected {
            Style::new().bg(theme::color(Token::UserBg)).add_modifier(Modifier::BOLD)
        } else {
            Style::new()
        };
        let mut spans = vec![Span::styled(name, base), Span::styled(" ".repeat(fill), base)];
        if show_keys {
            spans.push(Span::styled((*keys).to_owned(), theme::muted().patch(base)));
        }
        lines.push(Line::from(spans));
    }
    boxed(frame, rect, "Commands", lines);
    if rect.height >= 2 && rect.width >= 6 {
        let x = rect.x + 4 + u16::try_from(query.width()).unwrap_or(0);
        if x < rect.x + rect.width.saturating_sub(2) {
            frame.set_cursor_position((x, rect.y + 1));
        }
    }
}

/// The pager's lines as drawn at this width: a diff (one or two columns) or wrapped text.
pub(crate) fn pager_lines(text: &str, is_diff: bool, side_by_side: bool, inner: usize) -> Vec<Line<'static>> {
    if is_diff && side_by_side && inner >= 100 {
        crate::transcript::render_diff_side_by_side(text, inner)
    } else if is_diff {
        render_diff(text, inner, usize::MAX)
    } else {
        wrap_plain(text, Style::new(), inner)
    }
}

/// The same lines as plain text, for searching.
pub(crate) fn pager_plain_lines(text: &str, is_diff: bool, side_by_side: bool, inner: usize) -> Vec<String> {
    pager_lines(text, is_diff, side_by_side, inner)
        .iter()
        .map(|line| line.spans.iter().map(|span| span.content.as_ref()).collect::<String>())
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn draw_pager(
    frame: &mut Frame<'_>,
    area: Rect,
    title: &str,
    text: &str,
    is_diff: bool,
    side_by_side: bool,
    offset: usize,
    search: &crate::app::PagerSearch,
) -> usize {
    let inner = usize::from(area.width).saturating_sub(4);
    let lines = pager_lines(text, is_diff, side_by_side, inner);
    let body = usize::from(area.height).saturating_sub(2);
    let offset = offset.min(lines.len().saturating_sub(body));
    let mut heading = String::from(title);
    if lines.len() > body && body > 0 {
        let _ = write!(heading, " · line {} of {}", offset + 1, lines.len());
    }
    if let Some(typing) = &search.typing {
        let _ = write!(heading, " · /{typing}▏");
    } else if !search.query.is_empty() {
        if search.matches.is_empty() {
            let _ = write!(heading, " · /{}: no matches", search.query);
        } else {
            let _ = write!(heading, " · /{} {} of {} (n/N)", search.query, search.current + 1, search.matches.len());
        }
    }
    let hint = match (area.width >= 50, is_diff, search.typing.is_some() || !search.query.is_empty()) {
        (_, _, true) => "",
        (true, true, _) => "  (q closes · / search · n/p files · s side by side · e edit)",
        (true, false, _) => "  (q closes · / search)",
        _ => "",
    };
    heading.push_str(hint);
    let visible: Vec<_> = lines.into_iter().skip(offset).take(body).collect();
    boxed(frame, area, &heading, visible);
    offset
}

/// The registry search: a query line, then hits with how each would run.
fn draw_mcp_search(
    frame: &mut Frame<'_>,
    area: Rect,
    query: &str,
    hits: &[recode_core::extensions::McpSearchHit],
    selected: usize,
    searching: bool,
    error: Option<&str>,
) {
    let rows = u16::try_from(hits.len().min(8) * 2).unwrap_or(16);
    let rect = overlay_area(area, 90, rows + 7);
    let inner = usize::from(rect.width).saturating_sub(4);
    let mut lines = vec![Line::from(vec![
        Span::styled("Search  ", theme::muted()),
        Span::raw(query.to_owned()),
        Span::styled("▏", theme::accent()),
    ])];
    lines.push(Line::default());
    if searching {
        lines.push(Line::styled("Searching the registry…", theme::muted()));
    } else if let Some(error) = error {
        lines.extend(wrap_plain(error, Style::new().fg(theme::color(Token::Danger)), inner));
    } else if hits.is_empty() {
        lines.push(Line::styled(
            "Type part of a server's name and press Enter. Results come from registry.modelcontextprotocol.io.",
            theme::muted(),
        ));
    }
    let first = selected.saturating_sub(7);
    for (index, hit) in hits.iter().enumerate().skip(first).take(8) {
        let base = if index == selected {
            Style::new().bg(theme::color(Token::UserBg)).add_modifier(Modifier::BOLD)
        } else {
            Style::new()
        };
        let version = hit.version.as_deref().map(|v| format!("  {v}")).unwrap_or_default();
        let how = match (&hit.server, &hit.unsupported) {
            (Some(server), _) => server.connection_text(),
            (None, Some(why)) => format!("can't be added: {why}"),
            (None, None) => "no package re-code can run".to_owned(),
        };
        let how_style =
            if hit.server.is_some() { theme::muted() } else { Style::new().fg(theme::color(Token::Danger)) };
        lines.push(Line::from(vec![
            Span::styled(if index == selected { "❯ " } else { "  " }, theme::accent().patch(base)),
            Span::styled(truncate(&format!("{}{version}", hit.name), inner.saturating_sub(2)), base),
        ]));
        let detail = if hit.description.is_empty() { how.clone() } else { format!("{}  ·  {how}", hit.description) };
        lines.push(Line::styled(format!("    {}", truncate(&detail, inner.saturating_sub(4))), how_style));
    }
    lines.push(Line::default());
    let keys: &[(&str, &str)] = if hits.is_empty() {
        &[("enter", "search"), ("esc", "cancel")]
    } else {
        &[("enter", "add"), ("↑↓", "choose"), ("type", "to search again"), ("esc", "cancel")]
    };
    lines.push(legend(keys, inner));
    boxed(frame, rect, "Search the MCP registry", lines);
}

/// Which of a server's tools the model may use.
fn draw_tool_filter(frame: &mut Frame<'_>, area: Rect, server: &str, tools: &[(String, bool)], selected: usize) {
    let rows = u16::try_from(tools.len().min(14)).unwrap_or(14);
    let rect = overlay_area(area, 72, rows + 6);
    let inner = usize::from(rect.width).saturating_sub(4);
    let on = tools.iter().filter(|tool| tool.1).count();
    let mut lines = vec![Line::styled(
        format!("{on} of {} tools offered to the model. Space toggles, a all, n none.", tools.len()),
        theme::muted(),
    )];
    lines.push(Line::default());
    let first = selected.saturating_sub(13);
    for (index, (name, enabled)) in tools.iter().enumerate().skip(first).take(14) {
        let base = if index == selected {
            Style::new().bg(theme::color(Token::UserBg)).add_modifier(Modifier::BOLD)
        } else {
            Style::new()
        };
        let (mark, mark_style) =
            if *enabled { ("☑", Style::new().fg(theme::color(Token::Success))) } else { ("☐", theme::muted()) };
        lines.push(Line::from(vec![
            Span::styled(if index == selected { "❯ " } else { "  " }, theme::accent().patch(base)),
            Span::styled(format!("{mark} "), mark_style.patch(base)),
            Span::styled(truncate(name, inner.saturating_sub(4)), base),
        ]));
    }
    lines.push(Line::default());
    lines.push(legend(&[("enter", "save"), ("space", "toggle"), ("esc", "cancel")], inner));
    boxed(frame, rect, &format!("Tools of {server}"), lines);
}
