//! Fits labelled segments on one row. When space runs out, the least important segments disappear
//! whole rather than being clipped.

use crate::wrap::truncate;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

/// One piece of a status row.
#[derive(Debug, Clone)]
pub struct Segment {
    /// Lower numbers are kept first.
    pub priority: u8,
    pub text: String,
    /// A shorter form tried before the segment is removed.
    pub short: Option<String>,
    pub style: Style,
}

impl Segment {
    #[must_use]
    pub fn new(priority: u8, text: impl Into<String>, style: Style) -> Self {
        Self { priority, text: text.into(), short: None, style }
    }

    #[must_use]
    pub fn or_short(mut self, short: impl Into<String>) -> Self {
        self.short = Some(short.into());
        self
    }
}

const SEPARATOR: &str = " │ ";

/// Chooses which segments fit, keeping their original left-to-right order.
#[must_use]
pub fn fit(segments: &[Segment], width: usize) -> Vec<(String, Style)> {
    let mut order: Vec<usize> = (0..segments.len()).collect();
    order.sort_by_key(|&index| segments[index].priority);
    let mut chosen: Vec<Option<String>> = vec![None; segments.len()];
    let mut used = 0;
    let mut count = 0;
    for index in order {
        let segment = &segments[index];
        let separator = if count == 0 { 0 } else { SEPARATOR.width() };
        let fits = |text: &str| used + separator + text.width() <= width;
        let text = if fits(&segment.text) {
            Some(segment.text.clone())
        } else {
            segment.short.as_ref().filter(|short| fits(short)).cloned()
        };
        if let Some(text) = text {
            used += separator + text.width();
            count += 1;
            chosen[index] = Some(text);
        } else if count == 0 {
            // The most important segment always appears, even if it has to be shortened.
            let text = segment.short.as_ref().unwrap_or(&segment.text);
            let text = truncate(text, width);
            used = text.width();
            count = 1;
            chosen[index] = Some(text);
        }
    }
    chosen.into_iter().zip(segments).filter_map(|(text, segment)| text.map(|text| (text, segment.style))).collect()
}

/// Renders segments as one line. The result is never wider than `width`.
#[must_use]
pub fn line(segments: &[Segment], width: usize, separator_style: Style) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, (text, style)) in fit(segments, width).into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(SEPARATOR, separator_style));
        }
        spans.push(Span::styled(text, style));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wrap::line_width;

    fn sample() -> Vec<Segment> {
        vec![
            Segment::new(0, "Responding", Style::new()).or_short("Resp"),
            Segment::new(3, "demo-model", Style::new()),
            Segment::new(1, "esc stop", Style::new()),
            Segment::new(2, "ctx ~9k/32k", Style::new()),
        ]
    }

    fn texts(width: usize) -> Vec<String> {
        fit(&sample(), width).into_iter().map(|(text, _)| text).collect()
    }

    #[test]
    fn everything_shows_when_there_is_room() {
        assert_eq!(texts(80), ["Responding", "demo-model", "esc stop", "ctx ~9k/32k"]);
    }

    #[test]
    fn least_important_segments_go_first_and_order_is_kept() {
        assert_eq!(texts(35), ["Responding", "esc stop", "ctx ~9k/32k"]);
        assert_eq!(texts(21), ["Responding", "esc stop"]);
        assert_eq!(texts(10), ["Responding"]);
    }

    #[test]
    fn short_forms_are_used_before_clipping() {
        assert_eq!(texts(5), ["Resp"]);
        assert_eq!(texts(2), ["R…"]);
    }

    #[test]
    fn output_never_exceeds_the_width() {
        for width in 0..100 {
            assert!(line_width(&line(&sample(), width, Style::new())) <= width, "width {width}");
        }
    }
}
