//! Removes terminal control sequences from untrusted text before it is displayed.
//!
//! Model and tool output must not be able to forge an approval prompt, move the cursor or write to
//! the clipboard. ratatui stores text in cells, but control characters must still never reach it.

/// Strips escape sequences and control characters, keeping newlines and expanding tabs.
#[must_use]
pub fn sanitize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\n' => out.push('\n'),
            '\t' => out.push_str("    "),
            '\u{1b}' => skip_escape(&mut chars),
            // C1 CSI and OSC introducers, which some terminals honour.
            '\u{9b}' => skip_until_final(&mut chars),
            '\u{9d}' | '\u{90}' | '\u{98}' | '\u{9e}' | '\u{9f}' => skip_string(&mut chars),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

type Chars<'a> = std::iter::Peekable<std::str::Chars<'a>>;

fn skip_escape(chars: &mut Chars<'_>) {
    match chars.next() {
        Some('[') => skip_until_final(chars),
        Some(']' | 'P' | 'X' | '^' | '_') => skip_string(chars),
        _ => {}
    }
}

/// Skips the rest of a CSI sequence, which ends with a byte from `@` to `~`.
fn skip_until_final(chars: &mut Chars<'_>) {
    for c in chars.by_ref() {
        if ('@'..='~').contains(&c) {
            break;
        }
    }
}

/// Skips a string sequence, which ends with BEL or ST.
fn skip_string(chars: &mut Chars<'_>) {
    while let Some(c) = chars.next() {
        match c {
            '\u{7}' | '\u{9c}' => break,
            '\u{1b}' => {
                if chars.peek() == Some(&'\\') {
                    chars.next();
                }
                break;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_sequences_that_could_fake_the_interface() {
        assert_eq!(sanitize("a\x1b[31mred\x1b[0mb"), "aredb");
        assert_eq!(sanitize("x\x1b[2J\x1b[Hy"), "xy");
        assert_eq!(sanitize("clip\x1b]52;c;ZXZpbA==\x07board"), "clipboard");
        assert_eq!(sanitize("title\x1b]0;pwned\x1b\\end"), "titleend");
        assert_eq!(sanitize("c1\u{9b}31mz"), "c1z");
        assert_eq!(sanitize("bell\x07 back\x08"), "bell back");
    }

    #[test]
    fn keeps_ordinary_text() {
        assert_eq!(sanitize("héllo 世界 🦀\nnext\tcol"), "héllo 世界 🦀\nnext    col");
    }

    #[test]
    fn unterminated_sequences_do_not_panic() {
        assert_eq!(sanitize("a\x1b"), "a");
        assert_eq!(sanitize("a\x1b["), "a");
        assert_eq!(sanitize("a\x1b]52"), "a");
    }
}
