//! The `task` tool: the model delegates a bounded piece of work (research, a long read, a survey of
//! files) to a fresh conversation with its own context, and gets back only the result. The nested
//! agent shares the user's permissions, approvals and checkpoints, so nothing escapes the rules of the
//! parent; it defaults to reading tools, runs one level deep, and one at a time.

use recode_core::message::{ToolCall, ToolDefinition};
use serde_json::json;

pub const TOOL: &str = "task";
/// Steps a task may take.
pub const MAX_STEPS: usize = 12;
/// The most of a task's answer handed back; the rest is cut with a note.
pub const RESULT_CAP_CHARS: usize = 8_000;
/// Tools a task gets unless it asks for others.
const DEFAULT_TOOLS: &[&str] = &["read", "list", "grep", "git", "skill"];
/// Tools a task may never have, whatever it asks for.
const NEVER: &[&str] = &[TOOL, "plan"];

#[must_use]
pub fn tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: TOOL.to_owned(),
        description: "Delegate a self-contained piece of work to a helper with its own, empty context, and get \
                      back only its findings: research across many files, a long log to read, a survey. Give a \
                      short description and the full prompt with everything it needs (it knows nothing of this \
                      conversation). It can read, list, grep and run git by default; name other tools from your \
                      own set in `tools` if it needs them (the user approves as usual)."
            .to_owned(),
        parameters: json!({
            "type": "object",
            "properties": {
                "description": {"type": "string", "description": "A few words, shown to the user."},
                "prompt": {"type": "string", "description": "The complete task."},
                "tools": {"type": "array", "items": {"type": "string"}, "description": "Optional: tool names the helper may use."}
            },
            "required": ["description", "prompt"]
        }),
    }
}

/// The task's arguments, checked.
pub fn arguments(call: &ToolCall) -> Result<(String, String, Option<Vec<String>>), String> {
    let text =
        |key: &str| call.arguments.get(key).and_then(|v| v.as_str()).map(str::trim).unwrap_or_default().to_owned();
    let description = text("description");
    let prompt = text("prompt");
    if prompt.is_empty() {
        return Err("Error: the task needs a prompt with everything the helper should know.".to_owned());
    }
    let tools = call
        .arguments
        .get("tools")
        .and_then(|v| v.as_array())
        .map(|items| items.iter().filter_map(|item| item.as_str().map(str::to_owned)).collect::<Vec<_>>());
    Ok((if description.is_empty() { "task".to_owned() } else { description }, prompt, tools))
}

/// The tools a task gets, from the parent's set: the reading ones, or the ones it named.
#[must_use]
pub fn nested_tools(parent: &[ToolDefinition], requested: Option<&[String]>) -> Vec<ToolDefinition> {
    parent
        .iter()
        .filter(|tool| !NEVER.contains(&tool.name.as_str()))
        .filter(|tool| match requested {
            Some(names) => names.iter().any(|name| name == &tool.name) || tool.name == "skill",
            None => DEFAULT_TOOLS.contains(&tool.name.as_str()),
        })
        .cloned()
        .collect()
}

/// A task's answer, cut to size.
#[must_use]
pub fn cap_result(text: &str) -> String {
    let text = text.trim();
    if text.chars().count() <= RESULT_CAP_CHARS {
        return text.to_owned();
    }
    let kept: String = text.chars().take(RESULT_CAP_CHARS).collect();
    format!("{kept}\n\n[cut here: the task's answer was longer than {} characters]", RESULT_CAP_CHARS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tasks_get_reading_tools_by_default_and_never_nest() {
        let parent = recode_tools::tool_definitions();
        let names = |tools: &[ToolDefinition]| tools.iter().map(|t| t.name.clone()).collect::<Vec<_>>();
        let defaults = nested_tools(&parent, None);
        assert_eq!(names(&defaults), ["read", "list", "grep", "git"]);
        let mut with_task = parent.clone();
        with_task.push(tool_definition());
        let asked = nested_tools(&with_task, Some(&["shell".to_owned(), "task".to_owned(), "edit".to_owned()]));
        assert_eq!(names(&asked), ["edit", "shell"], "named tools, in the parent's order, never task");
        let call = ToolCall {
            id: "c".into(),
            name: TOOL.into(),
            arguments: json!({"description": "look", "prompt": "find x"}),
        };
        let (description, prompt, tools) = arguments(&call).unwrap();
        assert_eq!((description.as_str(), prompt.as_str(), tools), ("look", "find x", None));
        let empty = ToolCall { id: "c".into(), name: TOOL.into(), arguments: json!({"description": "look"}) };
        assert!(arguments(&empty).is_err());
        assert!(cap_result(&"x".repeat(9000)).ends_with("characters]"));
    }
}
