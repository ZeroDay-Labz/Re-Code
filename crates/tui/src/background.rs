//! Finds out whether the terminal has a light or dark background, so colours can suit it.
//!
//! The terminal is asked with OSC 11 before the interface starts reading keys. A terminal that doesn't
//! answer within a moment is treated as dark, which is what re-code's palette was designed for.

use std::io::Write;
use std::time::{Duration, Instant};

/// How the user wants colours chosen (`theme` in `config.toml`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ThemeChoice {
    /// Driftwave or Driftwave Light, whichever suits the terminal; a theme picked with /theme wins.
    #[default]
    Auto,
    /// A theme chosen in `config.toml`; it wins over /theme's choice.
    Named(crate::theme::ThemeSelection),
}

impl ThemeChoice {
    /// Reads `auto`, `dark`, `light`, or a theme such as `nord`, `nord:bright` or `nord+#ff8800`.
    pub fn parse(text: &str) -> Result<Self, String> {
        use crate::theme::ThemeSelection;
        match text.trim().to_ascii_lowercase().as_str() {
            "" | "auto" => Ok(Self::Auto),
            "dark" => Ok(Self::Named(ThemeSelection::named("Driftwave"))),
            "light" => Ok(Self::Named(ThemeSelection::named("Driftwave Light"))),
            other => ThemeSelection::parse(other)
                .map(Self::Named)
                .map_err(|error| format!("{error} Or use auto, dark or light.")),
        }
    }

    /// The theme to start with, and whether `config.toml` fixed it. Asks the terminal for its
    /// background when automatic.
    #[must_use]
    pub fn resolve(self) -> (crate::theme::ThemeSelection, bool) {
        match self {
            Self::Named(selection) => (selection, true),
            Self::Auto => {
                let light = query_background(Duration::from_millis(150)).is_some_and(|luminance| luminance > 0.5);
                (crate::theme::ThemeSelection::named(if light { "Driftwave Light" } else { "Driftwave" }), false)
            }
        }
    }
}

/// Asks the terminal for its background colour and returns its luminance (0 dark to 1 light). The
/// terminal must already be in raw mode, and nothing else may be reading input yet.
fn query_background(timeout: Duration) -> Option<f32> {
    let mut out = std::io::stdout();
    out.write_all(b"\x1b]11;?\x1b\\").ok()?;
    out.flush().ok()?;
    let stdin = rustix::stdio::stdin();
    let deadline = Instant::now() + timeout;
    let mut reply = Vec::new();
    while Instant::now() < deadline && !(reply.ends_with(b"\x07") || reply.ends_with(b"\x1b\\")) {
        let left = deadline.saturating_duration_since(Instant::now());
        let wait = rustix::event::Timespec {
            tv_sec: 0,
            tv_nsec: i64::try_from(left.as_nanos().min(999_999_999)).unwrap_or(0),
        };
        let mut fds = [rustix::event::PollFd::new(&stdin, rustix::event::PollFlags::IN)];
        if rustix::event::poll(&mut fds, Some(&wait)).ok()? == 0 {
            break;
        }
        let mut buffer = [0u8; 64];
        let read = rustix::io::read(stdin, &mut buffer).ok()?;
        if read == 0 {
            break;
        }
        reply.extend_from_slice(&buffer[..read]);
    }
    luminance(&String::from_utf8_lossy(&reply))
}

/// Reads `…11;rgb:RRRR/GGGG/BBBB…` (1 to 4 hex digits per channel) into a luminance.
fn luminance(reply: &str) -> Option<f32> {
    let rgb = reply.split("rgb:").nth(1)?;
    let channels: Vec<f32> = rgb
        .split('/')
        .take(3)
        .map(|part| {
            let digits: String = part.chars().take_while(char::is_ascii_hexdigit).collect();
            let max = 16f32.powi(i32::try_from(digits.len()).unwrap_or(4)) - 1.0;
            u32::from_str_radix(&digits, 16).ok().map(|value| value as f32 / max)
        })
        .collect::<Option<_>>()?;
    let [r, g, b] = channels[..] else { return None };
    Some(0.2126 * r + 0.7152 * g + 0.0722 * b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_replies_are_read_in_their_usual_forms() {
        let white = luminance("\x1b]11;rgb:ffff/ffff/ffff\x1b\\").unwrap();
        let black = luminance("\x1b]11;rgb:0000/0000/0000\x07").unwrap();
        let solarized_light = luminance("\x1b]11;rgb:fd/f6/e3\x07").unwrap();
        let dracula = luminance("\x1b]11;rgb:2828/2a2a/3636\x1b\\").unwrap();
        assert!(white > 0.99 && black < 0.01);
        assert!(solarized_light > 0.5 && dracula < 0.5);
        assert_eq!(luminance("garbage"), None);
    }

    #[test]
    fn a_theme_can_be_named_in_the_config() {
        assert_eq!(ThemeChoice::parse("auto"), Ok(ThemeChoice::Auto));
        assert!(matches!(ThemeChoice::parse("light"), Ok(ThemeChoice::Named(sel)) if sel.name == "Driftwave Light"));
        assert!(matches!(ThemeChoice::parse("Catppuccin Mocha"), Ok(ThemeChoice::Named(_))));
        assert!(
            matches!(ThemeChoice::parse("tokyo-night:dim"), Ok(ThemeChoice::Named(sel)) if sel.variant == crate::theme::Variant::Dim)
        );
        assert!(ThemeChoice::parse("purple").unwrap_err().contains("Gruvbox"), "the error lists the themes");
    }
}
