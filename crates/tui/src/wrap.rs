//! Word wrapping for styled text, measured in terminal cells.
//!
//! The guarantee every layout relies on: no produced line is wider than the requested width, for
//! any text, including wide characters and emoji at very narrow widths.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

struct Cell {
    text: String,
    style: Style,
    width: usize,
}

fn cells(spans: &[Span<'_>], limit: usize) -> Vec<Cell> {
    let mut out = Vec::new();
    for span in spans {
        for grapheme in span.content.graphemes(true) {
            let width = grapheme.width();
            if width == 0 {
                continue;
            }
            // A character wider than the whole line can never fit, so it's replaced.
            let (text, width) = if width > limit { ("?".to_owned(), 1) } else { (grapheme.to_owned(), width) };
            out.push(Cell { text, style: span.style, width });
        }
    }
    out
}

fn to_spans(cells: &[Cell]) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    for cell in cells {
        match spans.last_mut() {
            Some(last) if last.style == cell.style => last.content.to_mut().push_str(&cell.text),
            _ => spans.push(Span::styled(cell.text.clone(), cell.style)),
        }
    }
    spans
}

fn is_space(cell: &Cell) -> bool {
    cell.text == " "
}

/// Wraps spans to `width` cells. `first` prefixes the first line and `rest` every other line. A prefix
/// that would leave no room for text is dropped.
#[must_use]
pub fn wrap(spans: &[Span<'_>], width: usize, first: &[Span<'static>], rest: &[Span<'static>]) -> Vec<Line<'static>> {
    if width == 0 {
        return Vec::new();
    }
    let prefix_width = |prefix: &[Span<'static>]| prefix.iter().map(|span| span.content.width()).sum::<usize>();
    let usable = |prefix: &[Span<'static>]| prefix_width(prefix) < width;
    let first: &[Span<'static>] = if usable(first) { first } else { &[] };
    let rest: &[Span<'static>] = if usable(rest) { rest } else { &[] };
    let limit = width - prefix_width(first).max(prefix_width(rest));
    let all = cells(spans, limit);

    let mut lines = Vec::new();
    let mut start = 0;
    let mut is_first = true;
    loop {
        let prefix = if is_first { first } else { rest };
        let available = width - prefix_width(prefix);
        // Find how many cells fit, remembering the last place the line could break.
        let mut used = 0;
        let mut end = start;
        let mut last_space = None;
        while end < all.len() && used + all[end].width <= available {
            if is_space(&all[end]) {
                last_space = Some(end);
            }
            used += all[end].width;
            end += 1;
        }
        let break_at = if end == all.len() { end } else { last_space.filter(|&space| space > start).unwrap_or(end) };

        let mut spans: Vec<Span<'static>> = prefix.to_vec();
        spans.extend(to_spans(&all[start..break_at]));
        lines.push(Line::from(spans));

        start = break_at;
        while start < all.len() && is_space(&all[start]) {
            start += 1;
        }
        is_first = false;
        if start >= all.len() {
            return lines;
        }
    }
}

/// Wraps a single plain string.
#[must_use]
pub fn wrap_plain(text: &str, style: Style, width: usize) -> Vec<Line<'static>> {
    text.split('\n').flat_map(|line| wrap(&[Span::styled(line.to_owned(), style)], width, &[], &[])).collect()
}

/// Display width of a line.
#[must_use]
pub fn line_width(line: &Line<'_>) -> usize {
    line.spans.iter().map(|span| span.content.width()).sum()
}

/// Shortens text to fit, ending with `…` if anything was removed.
#[must_use]
pub fn truncate(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for grapheme in text.graphemes(true) {
        let cell = grapheme.width();
        if used + cell + 1 > width {
            break;
        }
        out.push_str(grapheme);
        used += cell;
    }
    out.push('…');
    out
}

/// Pads a line with styled spaces to fill `width` cells.
#[must_use]
pub fn pad(mut line: Line<'static>, width: usize, style: Style) -> Line<'static> {
    let missing = width.saturating_sub(line_width(&line));
    if missing > 0 {
        line.spans.push(Span::styled(" ".repeat(missing), style));
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(lines: &[Line<'_>]) -> Vec<String> {
        lines.iter().map(ToString::to_string).collect()
    }

    fn wrapped(text: &str, width: usize) -> Vec<String> {
        plain(&wrap_plain(text, Style::new(), width))
    }

    #[test]
    fn breaks_at_spaces_when_it_can() {
        assert_eq!(wrapped("the quick brown fox", 10), ["the quick", "brown fox"]);
    }

    #[test]
    fn breaks_long_words_when_it_must() {
        assert_eq!(wrapped("abcdefghij", 4), ["abcd", "efgh", "ij"]);
    }

    #[test]
    fn wide_characters_count_as_two_cells() {
        assert_eq!(wrapped("世界世界", 4), ["世界", "世界"]);
        assert_eq!(wrapped("世界", 3), ["世", "界"]);
    }

    #[test]
    fn a_character_too_wide_for_the_line_is_replaced() {
        assert_eq!(wrapped("世", 1), ["?"]);
    }

    #[test]
    fn empty_text_is_one_empty_line() {
        assert_eq!(wrapped("", 10), [""]);
        assert_eq!(wrapped("a\n\nb", 10), ["a", "", "b"]);
    }

    #[test]
    fn prefixes_apply_and_are_dropped_when_they_do_not_fit() {
        let lines = wrap(&[Span::raw("aaaa bbbb")], 7, &[Span::raw("- ")], &[Span::raw("  ")]);
        assert_eq!(plain(&lines), ["- aaaa", "  bbbb"]);
        let lines = wrap(&[Span::raw("ab")], 2, &[Span::raw("--- ")], &[]);
        assert_eq!(plain(&lines), ["ab"]);
    }

    #[test]
    fn no_line_ever_exceeds_the_width() {
        let samples = [
            "plain words of ordinary length that need wrapping",
            "世界世界世界 mixed with latin 🦀🦀🦀 and emoji",
            "e\u{301}e\u{301}e\u{301} combining marks 👨‍👩‍👧‍👦 and a family",
            "     leading spaces and trailing     ",
            "averyveryveryveryveryverylongwordwithnobreaks",
        ];
        for text in samples {
            for width in 1..60 {
                for line in wrap(&[Span::raw(text)], width, &[Span::raw("│ ")], &[Span::raw("  ")]) {
                    assert!(line_width(&line) <= width, "{text:?} at {width}: {line:?}");
                }
            }
        }
    }

    #[test]
    fn no_text_is_lost_except_spaces_at_breaks() {
        let text = "alpha beta 世界 gamma averyveryverylongword";
        for width in 2..40 {
            let joined: String = wrapped(text, width).concat();
            assert_eq!(joined.replace(' ', ""), text.replace(' ', ""), "width {width}");
        }
    }

    #[test]
    fn truncate_fits_and_marks_removal() {
        assert_eq!(truncate("hello", 5), "hello");
        assert_eq!(truncate("hello", 4), "hel…");
        assert_eq!(truncate("世界世界", 5), "世界…");
        assert_eq!(truncate("hello", 1), "…");
        assert_eq!(truncate("hello", 0), "");
    }
}
