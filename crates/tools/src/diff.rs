//! Unified diffs for file changes, shown in approval cards and the tool timeline.

use similar::TextDiff;

/// A unified diff between two versions of a file, with a `--- / +++` header.
#[must_use]
pub fn unified_diff(path: &str, before: &str, after: &str) -> String {
    if before == after {
        return String::new();
    }
    let diff = TextDiff::from_lines(before, after);
    diff.unified_diff().context_radius(3).header(&format!("a/{path}"), &format!("b/{path}")).to_string()
}

#[cfg(test)]
mod tests {
    use super::unified_diff;

    #[test]
    fn shows_added_and_removed_lines() {
        let diff = unified_diff("f.rs", "a\nb\nc\n", "a\nB\nc\n");
        assert!(diff.contains("-b"));
        assert!(diff.contains("+B"));
    }

    #[test]
    fn identical_content_has_no_diff() {
        assert!(unified_diff("f", "same\n", "same\n").is_empty());
    }
}
