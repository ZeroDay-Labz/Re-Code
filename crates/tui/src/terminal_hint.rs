//! Which key lets the terminal select text natively while re-code has the mouse. Most terminals use
//! Shift; iTerm2 uses Option and Terminal.app Fn. Inside tmux the terminal can't be told apart, so the
//! hint names tmux's own copy mode too.

/// The hint for the terminal re-code is running in.
#[must_use]
pub fn native_selection_hint() -> &'static str {
    hint_for(std::env::var("TERM_PROGRAM").ok().as_deref(), std::env::var_os("TMUX").is_some())
}

/// The hint for a terminal program name (`TERM_PROGRAM`) and whether tmux is in between.
#[must_use]
pub fn hint_for(term_program: Option<&str>, in_tmux: bool) -> &'static str {
    if in_tmux {
        return "Shift+drag selects natively (or tmux prefix+[)";
    }
    match term_program {
        Some("iTerm.app") => "Option+drag selects natively",
        Some("Apple_Terminal") => "Fn+drag selects natively",
        _ => "Shift+drag selects natively",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hint_names_the_right_key() {
        assert_eq!(hint_for(None, false), "Shift+drag selects natively");
        assert_eq!(hint_for(Some("WezTerm"), false), "Shift+drag selects natively");
        assert_eq!(hint_for(Some("iTerm.app"), false), "Option+drag selects natively");
        assert_eq!(hint_for(Some("Apple_Terminal"), false), "Fn+drag selects natively");
        assert!(hint_for(Some("iTerm.app"), true).contains("tmux"));
    }
}
