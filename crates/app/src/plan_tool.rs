//! The `plan` tool of plan mode: the agent writes what it would do instead of doing it. The plan
//! lives outside the project (so trust and git are untouched) and `/plan` shows it.

use recode_core::message::{ToolCall, ToolDefinition};
use serde_json::json;
use std::path::Path;

pub const TOOL: &str = "plan";

#[must_use]
pub fn tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: TOOL.to_owned(),
        description: "Write the plan for what you would do (plan mode: nothing else may change). Give the whole plan \
                      as Markdown: the goal, the steps with the files each touches, and how to verify. Replaces any \
                      earlier plan."
            .to_owned(),
        parameters: json!({
            "type": "object",
            "properties": {"text": {"type": "string", "description": "The plan, in Markdown."}},
            "required": ["text"]
        }),
    }
}

/// Writes the plan. Returns the text for the model and whether it succeeded.
#[must_use]
pub fn run(path: &Path, call: &ToolCall) -> (String, bool) {
    let text = call.arguments.get("text").and_then(|v| v.as_str()).unwrap_or_default().trim();
    if text.is_empty() {
        return ("Error: the plan is empty. Give the plan as Markdown in `text`.".to_owned(), false);
    }
    if let Some(parent) = path.parent()
        && let Err(error) = std::fs::create_dir_all(parent)
    {
        return (format!("Error: can't create {}: {error}", parent.display()), false);
    }
    match std::fs::write(path, format!("{text}\n")) {
        Ok(()) => (
            format!(
                "Plan saved ({} lines). The user reads it with /plan and carries it out after leaving plan mode.",
                text.lines().count()
            ),
            true,
        ),
        Err(error) => (format!("Error: can't write the plan: {error}"), false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plan_is_written_and_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plans/abc.md");
        let call =
            |text: &str| ToolCall { id: "c".into(), name: TOOL.into(), arguments: json!({"text": text}), extra: None };
        let (text, ok) = run(&path, &call("# Goal\n1. a\n2. b"));
        assert!(ok && text.contains("3 lines"), "{text}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "# Goal\n1. a\n2. b\n");
        let (_, ok) = run(&path, &call("# New"));
        assert!(ok);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "# New\n");
        let (text, ok) = run(&path, &call("   "));
        assert!(!ok && text.contains("empty"));
    }
}
