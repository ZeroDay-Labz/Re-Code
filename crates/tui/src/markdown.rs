//! Renders markdown to terminal lines at a given width.

use crate::theme::{self, Token, color};
use crate::wrap::{line_width, pad, wrap};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::sync::OnceLock;
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;
use unicode_width::UnicodeWidthStr;

/// Renders markdown. Output is never wider than `width`.
#[must_use]
pub fn render(text: &str, width: usize) -> Vec<Line<'static>> {
    render_with(text, width, false)
}

/// [`render`], with line numbers in code blocks when asked (and there's room for them).
#[must_use]
pub fn render_with(text: &str, width: usize, line_numbers: bool) -> Vec<Line<'static>> {
    if width == 0 {
        return Vec::new();
    }
    let mut renderer = Renderer::new(width);
    renderer.line_numbers = line_numbers;
    for event in Parser::new_ext(text, Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH) {
        renderer.event(event);
    }
    renderer.finish()
}

/// Finds where finished blocks end, so streaming only re-renders the block still being written.
#[must_use]
pub fn stable_prefix_len(text: &str) -> usize {
    let mut fence: Option<&str> = None;
    let mut boundary = 0;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim();
        match fence {
            Some(marker) => {
                if trimmed.starts_with(marker) && trimmed.trim_start_matches(marker).is_empty() {
                    fence = None;
                }
            }
            None => {
                if trimmed.starts_with("```") {
                    fence = Some("```");
                } else if trimmed.starts_with("~~~") {
                    fence = Some("~~~");
                } else if trimmed.is_empty() && line.ends_with('\n') {
                    boundary = offset + line.len();
                }
            }
        }
        offset += line.len();
    }
    boundary
}

#[derive(Default)]
struct Table {
    rows: Vec<Vec<String>>,
    row: Vec<String>,
    cell: String,
}

struct Renderer {
    width: usize,
    out: Vec<Line<'static>>,
    inline: Vec<Span<'static>>,
    styles: Vec<Style>,
    lists: Vec<Option<u64>>,
    item_marker: Option<String>,
    quote_depth: usize,
    code: Option<(String, String)>,
    table: Option<Table>,
    heading: bool,
    wrote_block: bool,
    /// Open links: the address, and the text written for it so far.
    links: Vec<(String, String)>,
    /// Number the lines of code blocks.
    line_numbers: bool,
}

impl Renderer {
    fn new(width: usize) -> Self {
        Self {
            width,
            out: Vec::new(),
            inline: Vec::new(),
            styles: vec![Style::new()],
            lists: Vec::new(),
            item_marker: None,
            quote_depth: 0,
            code: None,
            table: None,
            heading: false,
            wrote_block: false,
            links: Vec::new(),
            line_numbers: false,
        }
    }

    fn style(&self) -> Style {
        self.styles.last().copied().unwrap_or_default()
    }

    fn push_style(&mut self, change: impl FnOnce(Style) -> Style) {
        self.styles.push(change(self.style()));
    }

    fn pop_style(&mut self) {
        if self.styles.len() > 1 {
            self.styles.pop();
        }
    }

    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => self.text(&text),
            Event::Code(text) => {
                if let Some(table) = &mut self.table {
                    table.cell.push_str(&text);
                } else {
                    self.inline.push(Span::styled(
                        text.into_string(),
                        Style::new().fg(theme::color(Token::Attention)).bg(color(Token::CodeBg)),
                    ));
                }
            }
            Event::SoftBreak => self.text(" "),
            Event::HardBreak => self.flush(),
            Event::Rule => {
                self.gap();
                let width = self.width;
                self.out.push(Line::styled("─".repeat(width), theme::faint()));
                self.wrote_block = true;
            }
            // Raw HTML is shown as text and never interpreted.
            Event::Html(text) | Event::InlineHtml(text) => self.text(&text),
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Heading { level, .. } => {
                self.gap();
                self.heading = true;
                self.push_style(|style| {
                    let style = style.add_modifier(Modifier::BOLD);
                    if level == HeadingLevel::H1 || level == HeadingLevel::H2 {
                        style.fg(color(Token::Accent))
                    } else {
                        style
                    }
                });
            }
            Tag::Paragraph if self.item_marker.is_none() && !self.tight_list_paragraph() => self.gap(),
            Tag::BlockQuote(_) => {
                self.flush();
                self.gap();
                self.quote_depth += 1;
            }
            Tag::CodeBlock(kind) => {
                self.flush();
                self.gap();
                let language = match kind {
                    CodeBlockKind::Fenced(info) => info.split_whitespace().next().unwrap_or("").to_owned(),
                    CodeBlockKind::Indented => String::new(),
                };
                self.code = Some((language, String::new()));
            }
            Tag::List(start) => {
                self.flush();
                if self.lists.is_empty() {
                    self.gap();
                }
                self.lists.push(start);
            }
            Tag::Item => {
                self.flush();
                let marker = match self.lists.last_mut() {
                    Some(Some(number)) => {
                        let marker = format!("{number}. ");
                        *number += 1;
                        marker
                    }
                    _ => "• ".to_owned(),
                };
                self.item_marker = Some(marker);
            }
            Tag::Table(_) => {
                self.flush();
                self.gap();
                self.table = Some(Table::default());
            }
            Tag::Emphasis => self.push_style(|style| style.add_modifier(Modifier::ITALIC)),
            Tag::Strong => self.push_style(|style| style.add_modifier(Modifier::BOLD)),
            Tag::Strikethrough => self.push_style(|style| style.add_modifier(Modifier::CROSSED_OUT)),
            Tag::Link { dest_url, .. } => {
                self.links.push((dest_url.to_string(), String::new()));
                self.push_style(|style| style.fg(theme::color(Token::Sky)).add_modifier(Modifier::UNDERLINED));
            }
            _ => {}
        }
    }

    fn tight_list_paragraph(&self) -> bool {
        !self.lists.is_empty()
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Heading(_) => {
                self.flush();
                self.pop_style();
                self.heading = false;
                self.wrote_block = true;
            }
            TagEnd::Paragraph | TagEnd::Item => {
                self.flush();
                self.wrote_block = true;
            }
            TagEnd::BlockQuote(_) => {
                self.flush();
                self.quote_depth = self.quote_depth.saturating_sub(1);
            }
            TagEnd::CodeBlock => {
                if let Some((language, code)) = self.code.take() {
                    self.code_block(&language, &code);
                }
                self.wrote_block = true;
            }
            TagEnd::List(_) => {
                self.flush();
                self.lists.pop();
                self.wrote_block = true;
            }
            TagEnd::TableCell => {
                if let Some(table) = &mut self.table {
                    let cell = std::mem::take(&mut table.cell);
                    table.row.push(cell);
                }
            }
            TagEnd::TableHead | TagEnd::TableRow => {
                if let Some(table) = &mut self.table {
                    let row = std::mem::take(&mut table.row);
                    table.rows.push(row);
                }
            }
            TagEnd::Table => {
                if let Some(table) = self.table.take() {
                    self.render_table(&table);
                }
                self.wrote_block = true;
            }
            TagEnd::Link => {
                self.pop_style();
                // The address is shown after the text, unless the text already is the address.
                if let Some((url, text)) = self.links.pop()
                    && !url.is_empty()
                    && text.trim() != url
                    && self.code.is_none()
                    && self.table.is_none()
                {
                    self.inline.push(Span::styled(format!(" ({url})"), theme::faint()));
                }
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => self.pop_style(),
            _ => {}
        }
    }

    fn text(&mut self, text: &str) {
        if let Some((_, written)) = self.links.last_mut() {
            written.push_str(text);
        }
        if let Some((_, code)) = &mut self.code {
            code.push_str(text);
        } else if let Some(table) = &mut self.table {
            table.cell.push_str(text);
        } else {
            let style = self.style();
            self.inline.push(Span::styled(text.to_owned(), style));
        }
    }

    fn gap(&mut self) {
        if self.wrote_block {
            self.out.push(self.quote_line());
            self.wrote_block = false;
        }
    }

    fn quote_line(&self) -> Line<'static> {
        Line::from(self.quote_prefix())
    }

    fn quote_prefix(&self) -> Vec<Span<'static>> {
        if self.quote_depth == 0 || self.width < 4 {
            Vec::new()
        } else {
            let depth = self.quote_depth.min((self.width - 1) / 2);
            vec![Span::styled("│ ".repeat(depth), theme::faint())]
        }
    }

    /// Writes pending inline text as wrapped lines with the current list and quote prefixes.
    fn flush(&mut self) {
        if self.inline.is_empty() {
            return;
        }
        let spans = std::mem::take(&mut self.inline);
        let indent = " ".repeat(self.lists.len().saturating_sub(1) * 2);
        let quote = self.quote_prefix();
        let marker = self.item_marker.take();

        let mut first = quote.clone();
        let mut rest = quote;
        if !self.lists.is_empty() {
            let marker = marker.unwrap_or_else(|| "  ".to_owned());
            let marker_width = marker.width();
            first.push(Span::raw(indent.clone()));
            first.push(Span::styled(marker, theme::accent()));
            rest.push(Span::raw(indent + &" ".repeat(marker_width)));
        }
        self.out.extend(wrap(&spans, self.width, &first, &rest));
    }

    fn code_block(&mut self, language: &str, code: &str) {
        let background = Style::new().bg(color(Token::CodeBg));
        if !language.is_empty() {
            let label = crate::wrap::truncate(&format!(" {language}"), self.width);
            self.out.push(pad(
                Line::styled(label, Style::new().fg(theme::color(Token::Attention)).bg(color(Token::CodeBg))),
                self.width,
                background,
            ));
        }
        let lines = highlight(language, code);
        // A gutter of line numbers, when asked for and there's room; wrapped parts get a blank gutter.
        let numbered = self.line_numbers && self.width >= 40;
        let digits = if numbered { lines.len().max(1).to_string().len() } else { 0 };
        let gutter = theme::faint().bg(color(Token::CodeBg));
        for (index, spans) in lines.into_iter().enumerate() {
            let (first, rest): (Vec<Span<'static>>, Vec<Span<'static>>) = if numbered {
                (
                    vec![Span::styled(format!("{:>digits$} │ ", index + 1), gutter)],
                    vec![Span::styled(format!("{} ↪ ", " ".repeat(digits)), gutter)],
                )
            } else {
                (vec![Span::styled(" ", background)], vec![Span::styled("↪", gutter)])
            };
            for line in wrap(&spans, self.width, &first, &rest) {
                self.out.push(pad(line, self.width, background));
            }
        }
    }

    fn render_table(&mut self, table: &Table) {
        let columns = table.rows.iter().map(Vec::len).max().unwrap_or(0);
        if columns == 0 {
            return;
        }
        let mut widths = vec![0; columns];
        for row in &table.rows {
            for (index, cell) in row.iter().enumerate() {
                widths[index] = widths[index].max(cell.width());
            }
        }
        let total: usize = widths.iter().sum::<usize>() + (columns - 1) * 3;
        if total <= self.width {
            self.table_grid(table, &widths);
        } else {
            self.table_stacked(table);
        }
    }

    fn table_grid(&mut self, table: &Table, widths: &[usize]) {
        for (index, row) in table.rows.iter().enumerate() {
            let mut spans = Vec::new();
            for (column, width) in widths.iter().enumerate() {
                let cell = row.get(column).map_or("", String::as_str);
                let style = if index == 0 { Style::new().add_modifier(Modifier::BOLD) } else { Style::new() };
                spans.push(Span::styled(format!("{cell}{}", " ".repeat(width - cell.width())), style));
                if column + 1 < widths.len() {
                    spans.push(Span::styled(" │ ", theme::faint()));
                }
            }
            self.out.push(Line::from(spans));
            if index == 0 {
                let rule: Vec<String> = widths.iter().map(|width| "─".repeat(*width)).collect();
                self.out.push(Line::styled(rule.join("─┼─"), theme::faint()));
            }
        }
    }

    /// The narrow form: each row becomes a stack of "header: value" lines.
    fn table_stacked(&mut self, table: &Table) {
        let Some((header, rows)) = table.rows.split_first() else { return };
        for (index, row) in rows.iter().enumerate() {
            if index > 0 {
                self.out.push(Line::styled("─".repeat(self.width.min(8)), theme::faint()));
            }
            for (column, cell) in row.iter().enumerate() {
                let name = header.get(column).map_or("", String::as_str);
                let spans = [
                    Span::styled(format!("{name}: "), Style::new().add_modifier(Modifier::BOLD)),
                    Span::raw(cell.clone()),
                ];
                self.out.extend(wrap(&spans, self.width, &[], &[Span::raw("  ")]));
            }
        }
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        self.flush();
        // Unclosed code fences occur constantly while a response is streaming.
        if let Some((language, code)) = self.code.take() {
            self.code_block(&language, &code);
        }
        debug_assert!(self.out.iter().all(|line| line_width(line) <= self.width));
        self.out
    }
}

/// One line of code highlighted for its language, with no background, for diffs. The language is
/// a file extension or name ("rs", "py", "Makefile"); unknown ones come back plain.
#[must_use]
pub fn highlight_line(language: &str, text: &str) -> Vec<Span<'static>> {
    let tools = highlighter();
    let Some(syntax) = tools.syntaxes.find_syntax_by_token(language) else {
        return vec![Span::raw(text.to_owned())];
    };
    let resolved = crate::theme::resolved();
    let derived;
    let theme: &syntect::highlighting::Theme = if resolved.syntax == "derived" {
        derived = derived_theme(&resolved);
        &derived
    } else {
        tools.themes.themes.get(resolved.syntax).unwrap_or_else(|| &tools.themes.themes["base16-ocean.dark"])
    };
    let mut lines = HighlightLines::new(syntax, theme);
    match lines.highlight_line(text, &tools.syntaxes) {
        Ok(parts) => parts
            .into_iter()
            .map(|(style, part)| {
                let foreground = Color::Rgb(style.foreground.r, style.foreground.g, style.foreground.b);
                Span::styled(part.to_owned(), Style::new().fg(foreground))
            })
            .collect(),
        Err(_) => vec![Span::raw(text.to_owned())],
    }
}

/// A syntect theme made from the active theme's tokens, cached per set of colours.
fn derived_theme(resolved: &crate::theme::Resolved) -> std::sync::Arc<syntect::highlighting::Theme> {
    use syntect::highlighting::{ScopeSelectors, StyleModifier, Theme, ThemeItem, ThemeSettings};
    static CACHE: std::sync::RwLock<Option<([Color; 14], std::sync::Arc<Theme>)>> = std::sync::RwLock::new(None);
    if let Some((colors, theme)) = CACHE.read().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref()
        && *colors == resolved.colors
    {
        return std::sync::Arc::clone(theme);
    }
    let syntect_color = |color: Color| match color {
        Color::Rgb(r, g, b) => syntect::highlighting::Color { r, g, b, a: 255 },
        _ => syntect::highlighting::Color::WHITE,
    };
    let token = |token: Token| syntect_color(resolved.colors[token as usize]);
    let item = |scopes: &str, color: Token| ThemeItem {
        scope: scopes.parse::<ScopeSelectors>().unwrap_or_default(),
        style: StyleModifier { foreground: Some(token(color)), background: None, font_style: None },
    };
    let theme = Theme {
        name: Some("derived".to_owned()),
        author: None,
        settings: ThemeSettings { foreground: Some(token(Token::Text)), ..ThemeSettings::default() },
        scopes: vec![
            item("comment", Token::Muted),
            item("keyword, storage.modifier", Token::Violet),
            item("string", Token::Success),
            item("constant.numeric, constant.language, constant.character", Token::Pink),
            item("entity.name.function, support.function, meta.function-call", Token::Sky),
            item("storage.type, entity.name.type, entity.name.class, support.type, support.class", Token::Attention),
            item("variable.parameter, entity.name.tag", Token::Accent),
            item("punctuation", Token::Muted),
        ],
    };
    let theme = std::sync::Arc::new(theme);
    *CACHE.write().unwrap_or_else(std::sync::PoisonError::into_inner) =
        Some((resolved.colors, std::sync::Arc::clone(&theme)));
    theme
}

struct Highlighter {
    syntaxes: SyntaxSet,
    themes: ThemeSet,
}

fn highlighter() -> &'static Highlighter {
    static HIGHLIGHTER: OnceLock<Highlighter> = OnceLock::new();
    HIGHLIGHTER.get_or_init(|| Highlighter {
        syntaxes: SyntaxSet::load_defaults_newlines(),
        themes: ThemeSet::load_defaults(),
    })
}

fn highlight(language: &str, code: &str) -> Vec<Vec<Span<'static>>> {
    let background = Style::new().bg(color(Token::CodeBg));
    let plain = |line: &str| vec![Span::styled(line.trim_end_matches('\n').to_owned(), background)];
    if language.is_empty() {
        return code.split_inclusive('\n').map(plain).collect();
    }
    let tools = highlighter();
    let Some(syntax) = tools.syntaxes.find_syntax_by_token(language) else {
        return code.split_inclusive('\n').map(plain).collect();
    };
    // Code colours come from the theme: one of syntect's, or one built from the theme's own tokens.
    let resolved = crate::theme::resolved();
    let derived;
    let theme: &syntect::highlighting::Theme = if resolved.syntax == "derived" {
        derived = derived_theme(&resolved);
        &derived
    } else {
        tools.themes.themes.get(resolved.syntax).unwrap_or_else(|| &tools.themes.themes["base16-ocean.dark"])
    };
    let mut lines = HighlightLines::new(syntax, theme);
    code.split_inclusive('\n')
        .map(|line| match lines.highlight_line(line, &tools.syntaxes) {
            Ok(parts) => parts
                .into_iter()
                .map(|(style, text)| {
                    let foreground = Color::Rgb(style.foreground.r, style.foreground.g, style.foreground.b);
                    Span::styled(
                        text.trim_end_matches('\n').to_owned(),
                        Style::new().fg(foreground).bg(color(Token::CodeBg)),
                    )
                })
                .collect(),
            Err(_) => plain(line),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(text: &str, width: usize) -> Vec<String> {
        render(text, width).iter().map(ToString::to_string).collect()
    }

    const EVERYTHING: &str = "# Title\n\nSome *emphasis*, **bold** and `code`.\n\n- one\n- two\n  - nested item with several words\n\n1. first\n2. second\n\n> quoted text that goes on for a while\n\n```rust\nfn main() { println!(\"hi\"); }\n```\n\n| Name | Value |\n| --- | --- |\n| alpha | 1 |\n| 世界 | 2 |\n\n---\n\nEnd.\n";

    #[test]
    fn lists_are_marked_and_nested() {
        assert_eq!(plain("- a\n- b\n  - c\n", 20), ["• a", "• b", "  • c"]);
        assert_eq!(plain("3. a\n4. b\n", 20), ["3. a", "4. b"]);
    }

    #[test]
    fn list_text_wraps_under_itself() {
        assert_eq!(plain("- alpha beta\n", 8), ["• alpha", "  beta"]);
    }

    #[test]
    fn blocks_are_separated_by_one_blank_line() {
        assert_eq!(plain("one\n\ntwo\n", 20), ["one", "", "two"]);
    }

    #[test]
    fn code_keeps_its_indentation_and_is_filled_to_width() {
        let lines = plain("```\n  x\n```\n", 6);
        assert_eq!(lines, ["   x  "]);
    }

    #[test]
    fn an_unclosed_fence_still_renders() {
        let lines = plain("```rust\nlet x = 1;", 20);
        assert!(lines.iter().any(|line| line.contains("let x = 1;")));
    }

    #[test]
    fn tables_use_a_grid_when_they_fit_and_stack_when_they_do_not() {
        let table = "| Name | Value |\n| --- | --- |\n| alpha | 1 |\n";
        assert_eq!(plain(table, 20), ["Name  │ Value", "──────┼──────", "alpha │ 1    "]);
        assert_eq!(plain(table, 8), ["Name:", "  alpha", "Value: 1"]);
    }

    #[test]
    fn nothing_is_ever_wider_than_requested() {
        for width in 1..80 {
            for line in render(EVERYTHING, width) {
                assert!(line_width(&line) <= width, "width {width}: {line:?}");
            }
        }
    }

    #[test]
    fn html_is_shown_not_interpreted() {
        assert!(plain("<script>x</script>\n", 40).concat().contains("<script>"));
    }

    #[test]
    fn stable_prefix_stops_before_the_block_being_written() {
        assert_eq!(stable_prefix_len("one"), 0);
        assert_eq!(stable_prefix_len("one\n\ntwo"), 5);
        assert_eq!(stable_prefix_len("one\n\n"), 5);
        // A blank line inside an open code fence isn't a boundary.
        let open = "a\n\n```\nx\n\ny";
        assert_eq!(stable_prefix_len(open), 3);
        let closed = "a\n\n```\nx\n\ny\n```\n\nz";
        assert_eq!(stable_prefix_len(closed), closed.len() - 1);
    }

    #[test]
    fn rendering_in_two_parts_matches_rendering_at_once() {
        let split = stable_prefix_len(EVERYTHING);
        assert!(split > 0);
        let whole = plain(EVERYTHING, 40);
        let stable = plain(&EVERYTHING[..split], 40);
        assert_eq!(whole[..stable.len()], stable[..]);
    }

    #[test]
    fn a_link_shows_where_it_goes() {
        let lines = render("See [the docs](https://example.com/docs) or <https://example.com>.", 80);
        let text: String = lines.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
        assert!(text.contains("the docs (https://example.com/docs)"), "{text}");
        assert!(!text.contains("https://example.com (https://example.com)"), "no repeat when the text is the address");
    }
}
