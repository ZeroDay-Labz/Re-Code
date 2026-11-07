//! The conversation: entries, cached rendering, and scrolling.
//!
//! Each entry caches its wrapped lines per width, and only visible lines are copied each frame, so a
//! very long session costs little more to draw than a short one.

use crate::markdown;
use crate::theme::{self, Token, color};
use crate::wrap::{line_width, pad, truncate, wrap, wrap_plain};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use recode_core::ids::ApprovalId;
use recode_core::protocol::TurnOutcome;
use unicode_width::UnicodeWidthStr;

/// How a tool call stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolStatus {
    Running,
    AwaitingApproval {
        approval: ApprovalId,
        description: String,
        diff: Option<String>,
        /// What "always" (the a key) would allow; `None` when it isn't offered.
        always: Option<String>,
    },
    Succeeded {
        summary: String,
    },
    Failed {
        summary: String,
    },
    Denied,
    Interrupted,
}

/// A tool call in the conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolEntry {
    pub id: String,
    pub name: String,
    pub target: String,
    pub status: ToolStatus,
    pub output: String,
    pub diff: Option<String>,
    /// How long it ran, once finished.
    pub elapsed: Option<std::time::Duration>,
}

/// What a finished turn cost: time, tool calls and tokens (as the provider reported them).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnSummary {
    pub outcome: TurnOutcome,
    pub elapsed: Option<std::time::Duration>,
    pub tools: usize,
    pub prompt_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

impl ToolEntry {
    /// A finished tool with output or a diff to show when expanded.
    #[must_use]
    pub fn expandable(&self) -> bool {
        matches!(self.status, ToolStatus::Succeeded { .. } | ToolStatus::Failed { .. })
            && (!self.output.trim().is_empty() || self.diff.is_some())
    }

    /// What "always" would allow for this pending approval, if it's offered.
    #[must_use]
    pub fn always(&self) -> Option<&str> {
        match &self.status {
            ToolStatus::AwaitingApproval { always, .. } => always.as_deref(),
            _ => None,
        }
    }
}

/// One item in the conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    Notice(String),
    User(String),
    Reasoning(String),
    Assistant(String),
    Tool(ToolEntry),
    TurnEnd(TurnSummary),
}

/// Rendering options that change how entries look.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RenderOptions {
    pub show_reasoning: bool,
    /// No blank line between entries (the theme's density).
    pub compact: bool,
    /// Number the lines of code blocks.
    pub line_numbers: bool,
}

struct Slot {
    entry: Entry,
    /// For a reasoning block: whether this specific block is expanded (independent of the global toggle).
    expanded: bool,
    /// Bumped on every change to the entry.
    version: u64,
    cached: Option<Cached>,
    /// For streaming text: the rendered form of the finished blocks.
    stable: Option<Stable>,
}

struct Cached {
    version: u64,
    width: usize,
    options: RenderOptions,
    gap: bool,
    lines: Vec<Line<'static>>,
}

struct Stable {
    width: usize,
    bytes: usize,
    line_numbers: bool,
    lines: Vec<Line<'static>>,
}

/// Where the view is. While following, new output keeps the end in view. Otherwise the view stays attached to an
/// entry, which is what keeps your place through a resize.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Scroll {
    Follow,
    At { entry: usize, line: usize, entry_lines: usize },
}

/// The conversation.
pub struct Transcript {
    slots: Vec<Slot>,
    pub scroll: Scroll,
}

impl Default for Transcript {
    fn default() -> Self {
        Self { slots: Vec::new(), scroll: Scroll::Follow }
    }
}

impl Transcript {
    pub fn push(&mut self, entry: Entry) {
        self.slots.push(Slot { entry, version: 0, expanded: false, cached: None, stable: None });
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    #[must_use]
    pub fn last(&self) -> Option<&Entry> {
        self.slots.last().map(|slot| &slot.entry)
    }

    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &Entry> {
        self.slots.iter().map(|slot| &slot.entry)
    }

    /// Every rendered line as plain text at this width, for searching and for the terminal's scrollback.
    pub fn plain_lines(&mut self, width: usize, options: RenderOptions) -> Vec<String> {
        let total = self.total_lines(width, options);
        self.lines_between(width, options, 0, total)
            .iter()
            .map(|line| line.spans.iter().map(|span| span.content.as_ref()).collect::<String>().trim_end().to_owned())
            .collect()
    }

    /// Removes everything from the `turn`-th request (from 0) onwards. Returns whether anything went.
    pub fn truncate_to_turn(&mut self, turn: usize) -> bool {
        let Some(at) = self
            .slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| matches!(slot.entry, Entry::User(_)))
            .nth(turn)
            .map(|(index, _)| index)
        else {
            return false;
        };
        self.slots.truncate(at);
        self.scroll = Scroll::Follow;
        true
    }

    /// The requests so far, as (turn number from 0, first line), newest last.
    pub fn requests(&self) -> Vec<(usize, String)> {
        self.slots
            .iter()
            .filter_map(|slot| match &slot.entry {
                Entry::User(text) => Some(text.lines().next().unwrap_or_default().chars().take(70).collect::<String>()),
                _ => None,
            })
            .enumerate()
            .collect()
    }

    /// Makes every entry render again, for when its colours depend on something that changed (the theme).
    pub fn invalidate(&mut self) {
        for slot in &mut self.slots {
            slot.version += 1;
            slot.stable = None;
        }
    }

    /// Changes the last entry.
    pub fn with_last(&mut self, change: impl FnOnce(&mut Entry)) {
        if let Some(slot) = self.slots.last_mut() {
            change(&mut slot.entry);
            slot.version += 1;
        }
    }

    /// Changes every entry that the closure reports as changed.
    pub fn update_all(&mut self, mut change: impl FnMut(&mut Entry) -> bool) {
        for slot in &mut self.slots {
            if change(&mut slot.entry) {
                slot.version += 1;
            }
        }
    }

    fn ensure(&mut self, width: usize, options: RenderOptions) {
        let mut previous_is_tool = false;
        for index in 0..self.slots.len() {
            let is_tool = matches!(self.slots[index].entry, Entry::Tool(_));
            let gap = index > 0 && !(is_tool && previous_is_tool) && !options.compact;
            previous_is_tool = is_tool;
            let slot = &mut self.slots[index];
            let current = slot.cached.as_ref().is_some_and(|cached| {
                cached.version == slot.version
                    && cached.width == width
                    && cached.options == options
                    && cached.gap == gap
            });
            if current {
                continue;
            }
            let expanded = slot.expanded;
            let mut lines = if gap { vec![Line::default()] } else { Vec::new() };
            lines.extend(render_slot(&slot.entry, &mut slot.stable, width, options, expanded));
            slot.cached = Some(Cached { version: slot.version, width, options, gap, lines });
        }
    }

    fn lines_of(&self, index: usize) -> &[Line<'static>] {
        self.slots[index].cached.as_ref().map_or(&[], |cached| &cached.lines)
    }

    /// Total rendered lines at this width.
    pub fn total_lines(&mut self, width: usize, options: RenderOptions) -> usize {
        self.ensure(width, options);
        (0..self.slots.len()).map(|index| self.lines_of(index).len()).sum()
    }

    /// How many lines of the conversation are below the view (0 when following the latest).
    pub fn lines_below(&mut self, width: usize, height: usize, options: RenderOptions) -> usize {
        let total = self.total_lines(width, options);
        let top = self.top_line(total, height);
        total.saturating_sub(top + height)
    }

    /// The absolute line at the top of the view.
    fn top_line(&self, total: usize, height: usize) -> usize {
        let bottom = total.saturating_sub(height);
        match self.scroll {
            Scroll::Follow => bottom,
            Scroll::At { entry, line, entry_lines } => {
                let before: usize = (0..entry.min(self.slots.len())).map(|index| self.lines_of(index).len()).sum();
                let current = if entry < self.slots.len() { self.lines_of(entry).len() } else { 0 };
                // Re-wrapping changes an entry's height, so the position within it is kept proportionally.
                let offset = (line * current).checked_div(entry_lines).unwrap_or(0);
                (before + offset).min(bottom)
            }
        }
    }

    /// The lines to display in a view of this size.
    /// The lines to display, with the transcript entry index each line came from (for click targeting).
    pub fn visible_mapped(
        &mut self,
        width: usize,
        height: usize,
        options: RenderOptions,
    ) -> (Vec<Line<'static>>, Vec<usize>) {
        let total = self.total_lines(width, options);
        let top = self.top_line(total, height);
        let mut out = Vec::with_capacity(height);
        let mut owners = Vec::with_capacity(height);
        let mut position = 0;
        for index in 0..self.slots.len() {
            let lines = self.lines_of(index);
            let end = position + lines.len();
            if end > top && position < top + height {
                let from = top.saturating_sub(position);
                let to = lines.len().min(top + height - position);
                out.extend_from_slice(&lines[from..to]);
                owners.extend(std::iter::repeat_n(index, to - from));
            }
            position = end;
            if position >= top + height {
                break;
            }
        }
        (out, owners)
    }

    /// Where an entry's lines are in the whole conversation at this width: `start..end`.
    pub fn entry_lines(&mut self, entry: usize, width: usize, options: RenderOptions) -> Option<(usize, usize)> {
        self.ensure(width, options);
        if entry >= self.slots.len() {
            return None;
        }
        let start: usize = (0..entry).map(|index| self.lines_of(index).len()).sum();
        Some((start, start + self.lines_of(entry).len()))
    }

    /// The line of the whole conversation shown at the top of a view of this size.
    pub fn top(&mut self, width: usize, height: usize, options: RenderOptions) -> usize {
        let total = self.total_lines(width, options);
        self.top_line(total, height)
    }

    /// Lines `from..to` of the whole conversation as rendered at this width, wherever the view is.
    pub fn lines_between(
        &mut self,
        width: usize,
        options: RenderOptions,
        from: usize,
        to: usize,
    ) -> Vec<Line<'static>> {
        self.ensure(width, options);
        let mut out = Vec::new();
        let mut position = 0;
        for index in 0..self.slots.len() {
            let lines = self.lines_of(index);
            let end = position + lines.len();
            if end > from && position < to {
                let start = from.saturating_sub(position);
                let stop = lines.len().min(to - position);
                out.extend_from_slice(&lines[start..stop]);
            }
            position = end;
            if position >= to {
                break;
            }
        }
        out
    }

    /// The lines to display in a view of this size.
    pub fn visible(&mut self, width: usize, height: usize, options: RenderOptions) -> Vec<Line<'static>> {
        self.visible_mapped(width, height, options).0
    }

    /// Toggles a reasoning block's expansion. Returns true if the entry was a reasoning block.
    pub fn toggle_reasoning(&mut self, entry: usize) -> bool {
        self.toggle(entry)
    }

    /// Expands or collapses an entry that has more to show: a reasoning block, or a finished tool's
    /// output or diff. Returns whether anything changed.
    pub fn toggle(&mut self, entry: usize) -> bool {
        match self.slots.get_mut(entry) {
            Some(slot)
                if matches!(&slot.entry, Entry::Reasoning(_))
                    || matches!(&slot.entry, Entry::Tool(tool) if tool.expandable()) =>
            {
                slot.expanded = !slot.expanded;
                slot.version += 1;
                true
            }
            _ => false,
        }
    }

    /// Scrolls by whole lines. Reaching the end resumes following.
    pub fn scroll_by(&mut self, delta: isize, width: usize, height: usize, options: RenderOptions) {
        let total = self.total_lines(width, options);
        let bottom = total.saturating_sub(height);
        let top = self.top_line(total, height);
        let target = top.saturating_add_signed(delta).min(bottom);
        self.scroll_to_line(target, bottom);
    }

    fn scroll_to_line(&mut self, target: usize, bottom: usize) {
        if target >= bottom {
            self.scroll = Scroll::Follow;
            return;
        }
        let mut position = 0;
        for index in 0..self.slots.len() {
            let count = self.lines_of(index).len();
            if position + count > target {
                self.scroll = Scroll::At { entry: index, line: target - position, entry_lines: count };
                return;
            }
            position += count;
        }
        self.scroll = Scroll::Follow;
    }

    /// Jumps to the start of the conversation.
    pub fn scroll_to_top(&mut self, width: usize, height: usize, options: RenderOptions) {
        let total = self.total_lines(width, options);
        self.scroll_to_line(0, total.saturating_sub(height));
    }

    pub fn follow(&mut self) {
        self.scroll = Scroll::Follow;
    }
}

fn render_slot(
    entry: &Entry,
    stable: &mut Option<Stable>,
    width: usize,
    options: RenderOptions,
    expanded: bool,
) -> Vec<Line<'static>> {
    match entry {
        Entry::Notice(text) => {
            // A marker and a hanging indent set re-code's own messages apart from the conversation.
            let first = [Span::styled("• ", Style::new().fg(theme::color(Token::Sky)))];
            let rest = [Span::raw("  ")];
            if width < 6 {
                return wrap_plain(text, theme::muted(), width);
            }
            text.split('\n')
                .enumerate()
                .flat_map(|(index, line)| {
                    let lead: &[Span<'static>] = if index == 0 { &first } else { &rest };
                    wrap(&[Span::styled(line.to_owned(), theme::muted())], width, lead, &rest)
                })
                .collect()
        }
        Entry::User(text) => render_user(text, width),
        Entry::Reasoning(text) => render_reasoning(text, width, options, expanded),
        Entry::Assistant(text) => render_assistant(text, stable, width, options.line_numbers),
        Entry::Tool(tool) => render_tool(tool, width, expanded),
        Entry::TurnEnd(summary) => render_turn_end(summary, width),
    }
}

fn render_user(text: &str, width: usize) -> Vec<Line<'static>> {
    let background = Style::new().bg(color(Token::UserBg));
    let first = [Span::styled("▌ ", theme::accent().bg(color(Token::UserBg)))];
    let rest = [Span::styled("▌ ", theme::accent().bg(color(Token::UserBg)))];
    text.split('\n')
        .flat_map(|line| wrap(&[Span::styled(line.to_owned(), background)], width, &first, &rest))
        .map(|line| pad(line, width, background))
        .collect()
}

fn render_reasoning(text: &str, width: usize, options: RenderOptions, expanded: bool) -> Vec<Line<'static>> {
    let count = text.lines().filter(|line| !line.trim().is_empty()).count().max(1);
    let open = options.show_reasoning || expanded;
    let style = theme::faint().add_modifier(Modifier::ITALIC);
    if !open {
        // Collapsed, the thought itself is shown in brief, so steps read as what the model was doing.
        let first = text.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("Thinking");
        let more = if count > 1 { format!(" ({count} lines)") } else { String::new() };
        let room = width.saturating_sub(2 + more.width());
        let brief = truncate(first, room);
        let marker = if count > 1 || brief.width() < first.width() { "▸ " } else { "· " };
        let line = Line::from(vec![
            Span::styled(marker, theme::faint()),
            Span::styled(brief, style),
            Span::styled(more, theme::faint()),
        ]);
        return vec![truncate_line(line, width)];
    }
    let plural = if count == 1 { "line" } else { "lines" };
    let mut lines = wrap_plain(&format!("▾ Reasoning, {count} {plural}"), theme::faint(), width);
    let rule = [Span::styled("│ ", theme::faint())];
    let body = theme::muted().add_modifier(Modifier::ITALIC);
    for line in text.split('\n') {
        lines.extend(wrap(&[Span::styled(line.to_owned(), body)], width, &rule, &rule));
    }
    lines
}

/// Cuts a line to `width` columns (for a one-line summary that must never wrap).
fn truncate_line(line: Line<'static>, width: usize) -> Line<'static> {
    let mut used = 0;
    let mut spans = Vec::new();
    for span in line.spans {
        let room = width.saturating_sub(used);
        if room == 0 {
            break;
        }
        let text = truncate(&span.content, room);
        used += text.width();
        spans.push(Span::styled(text, span.style));
    }
    Line::from(spans)
}

fn render_assistant(text: &str, stable: &mut Option<Stable>, width: usize, line_numbers: bool) -> Vec<Line<'static>> {
    let split = markdown::stable_prefix_len(text);
    // Only the block still being written is parsed again, so long responses stay cheap to stream.
    let reuse = stable
        .as_ref()
        .is_some_and(|cached| cached.width == width && cached.bytes == split && cached.line_numbers == line_numbers);
    if !reuse {
        *stable = Some(Stable {
            width,
            bytes: split,
            line_numbers,
            lines: markdown::render_with(&text[..split], width, line_numbers),
        });
    }
    let mut lines = stable.as_ref().map_or_else(Vec::new, |cached| cached.lines.clone());
    let tail = markdown::render_with(&text[split..], width, line_numbers);
    if !lines.is_empty() && !tail.is_empty() {
        lines.push(Line::default());
    }
    lines.extend(tail);
    lines
}

/// Lines of a finished tool's output shown when it's expanded; Ctrl+O shows the rest.
const EXPANDED_OUTPUT_LINES: usize = 40;

fn render_tool(tool: &ToolEntry, width: usize, expanded: bool) -> Vec<Line<'static>> {
    let mut lines = render_tool_row(tool, width, expanded);
    if !expanded || !tool.expandable() {
        return lines;
    }
    if let Some(diff) = &tool.diff {
        lines.extend(render_diff(diff, width, EXPANDED_OUTPUT_LINES));
        return lines;
    }
    let rule = [Span::styled("  │ ", theme::faint())];
    let all: Vec<&str> = tool.output.lines().collect();
    for line in all.iter().take(EXPANDED_OUTPUT_LINES) {
        lines.extend(wrap(&[Span::styled((*line).to_owned(), theme::muted())], width, &rule, &rule));
    }
    if all.len() > EXPANDED_OUTPUT_LINES {
        let more =
            format!("  … {} more lines. Ctrl+O shows all of the latest output.", all.len() - EXPANDED_OUTPUT_LINES);
        lines.extend(wrap_plain(&more, theme::faint(), width));
    }
    lines
}

fn render_tool_row(tool: &ToolEntry, width: usize, expanded: bool) -> Vec<Line<'static>> {
    // A finished tool with something to show says so: ▸ opens it, ▾ closes it.
    let marked = |summary: &str| {
        if tool.expandable() {
            format!("{summary} {}", if expanded { "▾" } else { "▸" })
        } else {
            summary.to_owned()
        }
    };
    match &tool.status {
        ToolStatus::AwaitingApproval { description, diff, .. } => {
            render_approval_card(tool, description, diff.as_deref(), width)
        }
        ToolStatus::Running => {
            let mut lines = tool_row("▶", theme::accent(), tool, "", width);
            let tail: Vec<&str> = tool.output.lines().rev().take(4).collect();
            let rule = [Span::styled("  │ ", theme::faint())];
            for line in tail.into_iter().rev() {
                let text = truncate(line, width.saturating_sub(4));
                lines.extend(wrap(&[Span::styled(text, theme::muted())], width, &rule, &rule));
            }
            lines
        }
        ToolStatus::Succeeded { summary } => {
            tool_row("✓", Style::new().fg(color(Token::Success)), tool, &marked(summary), width)
        }
        ToolStatus::Failed { summary } => {
            tool_row("✗", Style::new().fg(color(Token::Danger)), tool, &marked(summary), width)
        }
        ToolStatus::Denied => tool_row("⊘", theme::muted(), tool, "denied", width),
        ToolStatus::Interrupted => tool_row("■", theme::muted(), tool, "interrupted", width),
    }
}

/// `✓ read  src/auth.rs              214 lines`, dropping detail from the right as width shrinks.
fn tool_row(icon: &str, icon_style: Style, tool: &ToolEntry, summary: &str, width: usize) -> Vec<Line<'static>> {
    let head = format!("{icon} {:<5} ", tool.name);
    let head_width = head.width();
    if width <= head_width + 1 {
        return vec![Line::styled(truncate(&format!("{icon} {}", tool.name), width), icon_style)];
    }
    let room = width - head_width;
    let summary_width = summary.width();
    let show_summary = !summary.is_empty() && tool.target.width() + summary_width + 2 <= room;
    let target_room = if show_summary { room - summary_width - 2 } else { room };
    let target = truncate(&tool.target, target_room);
    let mut spans = vec![
        Span::styled(format!("{icon} "), icon_style),
        Span::styled(format!("{:<5} ", tool.name), Style::new().fg(theme::color(Token::Sky))),
        Span::raw(target.clone()),
    ];
    if show_summary {
        let fill = room - target.width() - summary_width;
        spans.push(Span::raw(" ".repeat(fill)));
        spans.push(Span::styled(summary.to_owned(), theme::muted()));
    }
    vec![Line::from(spans)]
}

const CARD_DIFF_LINES: usize = 12;

fn render_approval_card(tool: &ToolEntry, description: &str, diff: Option<&str>, width: usize) -> Vec<Line<'static>> {
    let border = Style::new().fg(color(Token::Attention));
    // Below this width a border costs more than it is worth.
    if width < 24 {
        let mut lines = wrap_plain(&format!("▲ {description}"), theme::attention(), width);
        if let Some(diff) = diff {
            lines.extend(render_diff(diff, width, CARD_DIFF_LINES));
        }
        return lines;
    }
    let inner = width - 4;
    let label = " needs approval ";
    let title = truncate(&format!(" {} {} ", tool.name, tool.target), width.saturating_sub(4 + label.width()));
    let fill = width.saturating_sub(2 + title.width() + label.width());
    let mut lines = vec![Line::from(vec![
        Span::styled(theme::corners().0, border),
        Span::styled(title, theme::attention()),
        Span::styled("─".repeat(fill), border),
        Span::styled(label, theme::attention()),
        Span::styled(theme::corners().1, border),
    ])];
    let mut body = wrap_plain(description, Style::new(), inner);
    if let Some(diff) = diff {
        body.push(Line::default());
        body.extend(render_diff(diff, inner, CARD_DIFF_LINES));
    }
    for line in body {
        let missing = inner.saturating_sub(line_width(&line));
        let mut spans = vec![Span::styled("│ ", border)];
        spans.extend(line.spans);
        spans.push(Span::raw(" ".repeat(missing)));
        spans.push(Span::styled(" │", border));
        lines.push(Line::from(spans));
    }
    let (_, _, bottom_left, bottom_right) = theme::corners();
    lines.push(Line::styled(format!("{bottom_left}{}{bottom_right}", "─".repeat(width - 2)), border));
    lines
}

/// Renders a unified diff, limited to `limit` lines.
#[must_use]
pub fn render_diff(diff: &str, width: usize, limit: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let all: Vec<&str> = diff.lines().collect();
    let mut language = String::new();
    for text in all.iter().take(limit) {
        if let Some(path) = text.strip_prefix("+++ ") {
            language = diff_language(path);
        }
        let (style, fill) = match text.as_bytes().first() {
            Some(b'+') => {
                (Style::new().fg(color(Token::Success)).bg(color(Token::AddBg)), Style::new().bg(color(Token::AddBg)))
            }
            Some(b'-') => (
                Style::new().fg(color(Token::Danger)).bg(color(Token::RemoveBg)),
                Style::new().bg(color(Token::RemoveBg)),
            ),
            Some(b'@') => (theme::muted(), Style::new()),
            _ => (Style::new(), Style::new()),
        };
        let spans = diff_line_spans(text, &language, style, fill);
        let rest = [Span::styled("↪", theme::faint().patch(fill))];
        for line in wrap(&spans, width, &[], &rest) {
            lines.push(pad(line, width, fill));
        }
    }
    if all.len() > limit {
        let more = format!("… {} more lines, press d for the full diff", all.len() - limit);
        lines.extend(wrap_plain(&more, theme::muted(), width));
    }
    lines
}

/// The language of a diff's file, from its `+++ b/path` header: the extension, or the file name.
fn diff_language(path: &str) -> String {
    let path = path.trim().trim_start_matches("b/");
    let name = path.rsplit('/').next().unwrap_or(path);
    match name.rsplit_once('.') {
        Some((_, extension)) if !extension.is_empty() => extension.to_owned(),
        _ => name.to_owned(),
    }
}

/// A diff line as spans: the +/- marker, then the code highlighted for its language, on the line's
/// background. Headers and hunk markers stay plain.
fn diff_line_spans(text: &str, language: &str, style: Style, fill: Style) -> Vec<Span<'static>> {
    let is_code = matches!(text.as_bytes().first(), Some(b'+' | b'-' | b' '))
        && !text.starts_with("+++")
        && !text.starts_with("---");
    if !is_code || language.is_empty() {
        return vec![Span::styled(text.to_owned(), style)];
    }
    let (marker, code) = text.split_at(1);
    let mut spans = vec![Span::styled(marker.to_owned(), style)];
    for span in markdown::highlight_line(language, code) {
        spans.push(Span::styled(span.content.into_owned(), span.style.patch(fill)));
    }
    spans
}

/// A diff as two columns, old on the left and new on the right, for a wide window. Removed and added
/// lines in the same hunk sit beside each other; a line with no counterpart faces a blank.
#[must_use]
pub fn render_diff_side_by_side(diff: &str, width: usize) -> Vec<Line<'static>> {
    let column = width.saturating_sub(3) / 2;
    if column < 10 {
        return render_diff(diff, width, usize::MAX);
    }
    let mut out = Vec::new();
    let mut language = String::new();
    let mut removed: Vec<&str> = Vec::new();
    let mut added: Vec<&str> = Vec::new();
    let cell = |text: &str, style: Style, fill: Style, language: &str| -> Line<'static> {
        let spans = diff_line_spans(text, language, style, fill);
        let line = Line::from(spans);
        let truncated = truncate(&line.to_string(), column);
        // Highlighting is kept where the line fits; a cut line is shown plain, marked with …
        if truncated.width() < line.to_string().width() {
            pad(Line::styled(truncated, style), column, fill)
        } else {
            pad(line, column, fill)
        }
    };
    let flush = |removed: &mut Vec<&str>, added: &mut Vec<&str>, out: &mut Vec<Line<'static>>, language: &str| {
        let rows = removed.len().max(added.len());
        for index in 0..rows {
            let left = match removed.get(index) {
                Some(text) => cell(
                    text,
                    Style::new().fg(color(Token::Danger)).bg(color(Token::RemoveBg)),
                    Style::new().bg(color(Token::RemoveBg)),
                    language,
                ),
                None => pad(Line::default(), column, Style::new()),
            };
            let right = match added.get(index) {
                Some(text) => cell(
                    text,
                    Style::new().fg(color(Token::Success)).bg(color(Token::AddBg)),
                    Style::new().bg(color(Token::AddBg)),
                    language,
                ),
                None => pad(Line::default(), column, Style::new()),
            };
            let mut spans = left.spans;
            spans.push(Span::styled(" │ ", theme::faint()));
            spans.extend(right.spans);
            out.push(Line::from(spans));
        }
        removed.clear();
        added.clear();
    };
    for text in diff.lines() {
        if let Some(path) = text.strip_prefix("+++ ") {
            language = diff_language(path);
        }
        match text.as_bytes().first() {
            Some(b'-') if !text.starts_with("---") => removed.push(text),
            Some(b'+') if !text.starts_with("+++") => added.push(text),
            _ => {
                flush(&mut removed, &mut added, &mut out, &language);
                let style = if text.starts_with('@') { theme::muted() } else { Style::new() };
                let both = text.strip_prefix(' ').unwrap_or(text);
                if text.starts_with(' ') {
                    // Context shows on both sides.
                    let left = cell(text, Style::new(), Style::new(), &language);
                    let mut spans = left.spans;
                    spans.push(Span::styled(" │ ", theme::faint()));
                    spans.extend(cell(&format!(" {both}"), Style::new(), Style::new(), &language).spans);
                    out.push(Line::from(spans));
                } else {
                    out.push(Line::styled(truncate(text, width), style));
                }
            }
        }
    }
    flush(&mut removed, &mut added, &mut out, &language);
    out
}

fn render_turn_end(summary: &TurnSummary, width: usize) -> Vec<Line<'static>> {
    match &summary.outcome {
        TurnOutcome::Completed => {
            // A quiet footer: how long, how many tools, how many tokens — only what's known.
            let mut parts = Vec::new();
            if let Some(elapsed) = summary.elapsed.filter(|elapsed| elapsed.as_millis() >= 500) {
                parts.push(duration_text(elapsed));
            }
            if summary.tools > 0 {
                parts.push(format!("{} tool{}", summary.tools, if summary.tools == 1 { "" } else { "s" }));
            }
            match (summary.prompt_tokens, summary.output_tokens) {
                (Some(prompt), Some(output)) => {
                    let cached =
                        summary.cached_tokens.filter(|c| *c > 0).map(|c| format!(" ({} cached)", tokens_text(c)));
                    parts.push(format!(
                        "{} in{} / {} out",
                        tokens_text(prompt),
                        cached.unwrap_or_default(),
                        tokens_text(output)
                    ));
                }
                (Some(prompt), None) => parts.push(format!("{} in", tokens_text(prompt))),
                (None, Some(output)) => parts.push(format!("{} out", tokens_text(output))),
                (None, None) => {}
            }
            if parts.is_empty() || width < 30 {
                return Vec::new();
            }
            let text = parts.join(" · ");
            let text = truncate(&text, width.saturating_sub(6));
            vec![Line::styled(format!("── {text} ──"), theme::faint())]
        }
        TurnOutcome::Interrupted => wrap_plain("■ Interrupted", theme::muted(), width),
        TurnOutcome::Failed { reason } => {
            wrap_plain(&format!("✗ Failed: {reason}"), Style::new().fg(color(Token::Danger)), width)
        }
    }
}

/// "0.8 s", "42 s" or "3 min 5 s".
#[must_use]
pub fn duration_text(elapsed: std::time::Duration) -> String {
    let secs = elapsed.as_secs_f64();
    if secs < 10.0 {
        format!("{secs:.1} s")
    } else if secs < 90.0 {
        format!("{secs:.0} s")
    } else {
        format!("{} min {} s", elapsed.as_secs() / 60, elapsed.as_secs() % 60)
    }
}

/// "820" or "3.1k".
fn tokens_text(tokens: u64) -> String {
    if tokens >= 1000 { format!("{:.1}k", tokens as f64 / 1000.0) } else { tokens.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPTIONS: RenderOptions = RenderOptions { show_reasoning: false, compact: false, line_numbers: false };

    fn long_transcript(entries: usize) -> Transcript {
        let mut transcript = Transcript::default();
        for index in 0..entries {
            transcript.push(Entry::User(format!("message {index} {}", "word ".repeat(30))));
        }
        transcript
    }

    fn text(lines: &[Line<'_>]) -> String {
        lines.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn following_shows_the_end_and_new_output() {
        let mut transcript = long_transcript(20);
        assert!(text(&transcript.visible(40, 5, OPTIONS)).contains("word"));
        transcript.push(Entry::Notice("the newest thing".into()));
        assert!(text(&transcript.visible(40, 5, OPTIONS)).contains("the newest thing"));
    }

    #[test]
    fn scrolled_view_stays_put_when_output_arrives() {
        let mut transcript = long_transcript(20);
        transcript.scroll_by(-30, 40, 5, OPTIONS);
        let before = text(&transcript.visible(40, 5, OPTIONS));
        transcript.push(Entry::Notice("new".into()));
        assert_eq!(before, text(&transcript.visible(40, 5, OPTIONS)));
    }

    #[test]
    fn scrolling_to_the_end_resumes_following() {
        let mut transcript = long_transcript(20);
        transcript.scroll_by(-10, 40, 5, OPTIONS);
        assert_ne!(transcript.scroll, Scroll::Follow);
        transcript.scroll_by(1000, 40, 5, OPTIONS);
        assert_eq!(transcript.scroll, Scroll::Follow);
    }

    #[test]
    fn a_message_at_the_top_of_the_view_stays_there_through_resizes() {
        let mut transcript = long_transcript(30);
        transcript.total_lines(80, OPTIONS);
        transcript.scroll = Scroll::At { entry: 7, line: 0, entry_lines: 4 };
        for width in [30, 120, 15, 200, 80] {
            let view = transcript.visible(width, 10, OPTIONS);
            let top = text(&view[..2]);
            assert!(top.contains("message 7 "), "lost the message at width {width}:\n{top}");
        }
    }

    #[test]
    fn a_position_inside_a_message_is_kept_proportionally() {
        let mut transcript = long_transcript(30);
        let height_at = |transcript: &mut Transcript, width| {
            transcript.total_lines(width, OPTIONS);
            transcript.lines_of(7).len()
        };
        let wide = height_at(&mut transcript, 80);
        // Halfway through message 7, measured at the wide layout.
        transcript.scroll = Scroll::At { entry: 7, line: wide / 2, entry_lines: wide };
        let narrow = height_at(&mut transcript, 20);
        assert!(narrow > wide);
        let total = transcript.total_lines(20, OPTIONS);
        let before: usize = (0..7).map(|index| transcript.lines_of(index).len()).sum();
        assert_eq!(transcript.top_line(total, 10), before + (wide / 2) * narrow / wide);
    }

    #[test]
    fn scrolling_past_the_start_stops_there() {
        let mut transcript = long_transcript(5);
        transcript.scroll_by(-100_000, 40, 5, OPTIONS);
        assert!(text(&transcript.visible(40, 5, OPTIONS)).contains("message 0 "));
    }

    #[test]
    fn view_is_never_taller_or_wider_than_requested() {
        let mut transcript = long_transcript(5);
        transcript.push(Entry::Assistant("# Head\n\n```rust\nfn main() {}\n```\n".into()));
        transcript.push(Entry::Tool(ToolEntry {
            id: "1".into(),
            name: "edit".into(),
            target: "src/really/quite/long/path/to/some/file.rs".into(),
            status: ToolStatus::AwaitingApproval {
                approval: ApprovalId::new(),
                description: "Change a file".into(),
                diff: Some("@@ -1 +1 @@\n-old line of text\n+new line of text".into()),
                always: Some("always edit in this project".into()),
            },
            output: String::new(),
            diff: None,
            elapsed: None,
        }));
        for width in 1..100 {
            for height in [0, 1, 7, 50] {
                let lines = transcript.visible(width, height, OPTIONS);
                assert!(lines.len() <= height);
                for line in &lines {
                    assert!(line_width(line) <= width, "width {width}: {line:?}");
                }
            }
        }
    }

    #[test]
    fn a_reasoning_block_expands_per_block_and_maps_to_its_entry() {
        let mut transcript = Transcript::default();
        transcript.push(Entry::User("go".into()));
        transcript.push(Entry::Reasoning("secret thought one\nsecret thought two".into()));
        // Collapsed: just the first thought, in brief, and how much more there is.
        let collapsed = text(&transcript.visible(60, 20, OPTIONS));
        assert!(collapsed.contains("▸ secret thought one (2 lines)"), "{collapsed}");
        assert!(!collapsed.contains("secret thought two"));
        // The reasoning header maps to entry index 1.
        let (_, owners) = transcript.visible_mapped(60, 20, OPTIONS);
        let header_line = owners.iter().position(|&e| e == 1).unwrap();
        assert!(toggle_entry_at(&mut transcript, owners[header_line]));
        let expanded = text(&transcript.visible(60, 20, OPTIONS));
        assert!(expanded.contains("secret thought two"));
        assert!(expanded.contains("▾ Reasoning"));
    }

    #[test]
    fn a_one_line_thought_reads_inline_without_a_heading() {
        let mut transcript = Transcript::default();
        transcript.push(Entry::Reasoning("Check the retry delay.".into()));
        let shown = text(&transcript.visible(60, 5, OPTIONS));
        assert!(shown.contains("· Check the retry delay."), "{shown}");
        assert!(!shown.contains("Reasoning,"));
        // Too long for the width: shortened, and marked as openable.
        let narrow = text(&transcript.visible(14, 5, OPTIONS));
        assert!(narrow.contains('▸') && narrow.contains('…'), "{narrow}");
    }

    #[test]
    fn code_blocks_can_be_numbered_and_diffs_are_highlighted_and_paired() {
        let text = "```rust\nfn main() {\n    println!(\"hi\");\n}\n```";
        let plain = markdown::render_with(text, 60, false);
        let numbered = markdown::render_with(text, 60, true);
        assert!(numbered.iter().any(|l| l.to_string().starts_with("1 │ fn main")), "{numbered:?}");
        assert!(!plain.iter().any(|l| l.to_string().contains("│ fn main")));
        assert!(
            markdown::render_with(text, 30, true).iter().all(|l| !l.to_string().contains("1 │")),
            "no gutter when narrow"
        );

        let diff = "--- a/x.rs\n+++ b/x.rs\n@@ -1 +1 @@\n-let a = 1;\n+let a = 2;\n context\n";
        let lines = render_diff(diff, 60, usize::MAX);
        let removed = lines.iter().find(|l| l.to_string().starts_with("-let")).unwrap();
        assert!(removed.spans.len() > 2, "the code is highlighted in pieces: {removed:?}");
        let side = render_diff_side_by_side(diff, 100);
        let paired = side.iter().find(|l| l.to_string().contains("-let a = 1;")).unwrap().to_string();
        assert!(paired.contains("│") && paired.contains("+let a = 2;"), "old and new on one row: {paired}");
        let context = side.iter().find(|l| l.to_string().contains("context")).unwrap().to_string();
        assert_eq!(context.matches("context").count(), 2, "context shows on both sides");
    }

    fn toggle_entry_at(transcript: &mut Transcript, entry: usize) -> bool {
        transcript.toggle_reasoning(entry)
    }

    #[test]
    fn toggling_a_non_reasoning_entry_does_nothing() {
        let mut transcript = Transcript::default();
        transcript.push(Entry::User("x".into()));
        assert!(!transcript.toggle_reasoning(0));
    }

    #[test]
    fn streaming_output_matches_a_full_render() {
        let source = "Intro.\n\n- one\n- two\n\n```rust\nfn main() {}\n```\n\nEnd of the response.";
        let mut transcript = Transcript::default();
        transcript.push(Entry::Assistant(String::new()));
        for character in source.chars() {
            transcript.with_last(|entry| {
                if let Entry::Assistant(text) = entry {
                    text.push(character);
                }
            });
            transcript.total_lines(40, OPTIONS);
        }
        let streamed = text(&transcript.visible(40, 100, OPTIONS));
        let mut fresh = Transcript::default();
        fresh.push(Entry::Assistant(source.into()));
        assert_eq!(streamed, text(&fresh.visible(40, 100, OPTIONS)));
    }
}
