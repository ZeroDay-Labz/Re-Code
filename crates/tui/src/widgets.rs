//! Shared visual pieces: rounded panels, key legends, chips and meters.

use crate::theme::{self, Token, color, gradient};
use crate::wrap::line_width;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders};

/// Draws a rounded panel with its titles set into the border, and returns the space inside.
pub fn panel(
    frame: &mut Frame<'_>,
    area: Rect,
    left: Line<'static>,
    right: Option<Line<'static>>,
    bottom: Option<Line<'static>>,
    focused: bool,
) -> Rect {
    if area.width < 3 || area.height < 2 {
        return area;
    }
    let border = Style::new().fg(if focused { color(Token::Accent) } else { color(Token::Faint) });
    let mut block = Block::new().borders(Borders::ALL).border_type(border_type()).border_style(border).title(left);
    if let Some(right) = right {
        block = block.title(right.right_aligned());
    }
    if let Some(bottom) = bottom {
        block = block.title_bottom(bottom);
    }
    let inner = block.inner(area);
    frame.render_widget(block, area);
    inner
}

/// The border style the theme asks for.
#[must_use]
pub fn border_type() -> BorderType {
    match theme::resolved().borders {
        theme::Borders::Rounded => BorderType::Rounded,
        theme::Borders::Square => BorderType::Plain,
    }
}

/// Readable text on a colour, for small labels.
#[must_use]
pub fn chip(text: impl Into<String>, background: Color) -> Span<'static> {
    Span::styled(
        format!(" {} ", text.into()),
        Style::new().fg(theme::contrast_for(background)).bg(background).add_modifier(Modifier::BOLD),
    )
}

/// A key legend: as many (key, meaning) pairs as fit, most important first. The keys are always visible.
#[must_use]
pub fn legend(items: &[(&str, &str)], width: usize) -> Line<'static> {
    let mut spans = Vec::new();
    let mut used = 0;
    for (key, meaning) in items {
        let separator = if spans.is_empty() { 0 } else { 2 };
        let size = separator + line_width(&Line::raw(format!("{key} {meaning}")));
        if used + size > width {
            continue;
        }
        if separator > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled((*key).to_owned(), theme::accent().add_modifier(Modifier::BOLD)));
        spans.push(Span::styled(format!(" {meaning}"), theme::muted()));
        used += size;
    }
    Line::from(spans)
}

/// A horizontal meter drawn in the signature gradient.
#[must_use]
pub fn meter(fraction: f64, width: usize) -> Vec<Span<'static>> {
    if width == 0 {
        return Vec::new();
    }
    // Finite and clamped, so the conversion is exact enough for choosing a cell.
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let filled = ((fraction.clamp(0.0, 1.0) * width as f64).round() as usize).min(width);
    let mut spans = Vec::with_capacity(width);
    for cell in 0..width {
        #[allow(clippy::cast_precision_loss)]
        let position = cell as f32 / width as f32;
        if cell + 1 == filled {
            spans.push(Span::styled("●", Style::new().fg(theme::color(Token::Text))));
        } else if cell < filled {
            spans.push(Span::styled("━", Style::new().fg(gradient(position))));
        } else {
            spans.push(Span::styled("─", theme::faint()));
        }
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_legend_never_overflows_and_drops_whole_entries() {
        let items = [("enter", "send"), ("esc", "stop"), ("^k", "commands")];
        for width in 0..40 {
            let line = legend(&items, width);
            assert!(line_width(&line) <= width, "width {width}");
            let text = line.to_string();
            assert!(!text.ends_with("comm"), "an entry was cut: {text}");
        }
        assert_eq!(legend(&items, 10).to_string(), "enter send");
    }

    #[test]
    fn a_meter_is_exactly_as_wide_as_requested_and_handles_bad_input() {
        for fraction in [0.0, 0.5, 1.0, 1.7, -1.0, f64::NAN] {
            for width in [0, 1, 9] {
                assert_eq!(meter(fraction, width).len(), width);
            }
        }
    }
}
