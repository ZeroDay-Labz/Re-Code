//! The `todo` tool: the model keeps a short list of what it's doing, shown in the side panel and kept
//! through compaction and restarts. The whole list is replaced on each call, which is the simplest
//! contract for small local models to get right.

use recode_core::message::{ToolCall, ToolDefinition};
use recode_core::protocol::{TodoItem, TodoStatus};
use serde_json::json;

pub const TOOL: &str = "todo";
const MAX_ITEMS: usize = 20;

#[must_use]
pub fn tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: TOOL.to_owned(),
        description: "Keep your task list for a job with several steps: call it with the whole list each time \
                      something changes (a step done, a new step found). The user sees it. Skip it for quick, \
                      single-step requests."
            .to_owned(),
        parameters: json!({
            "type": "object",
            "properties": {
                "items": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "text": {"type": "string", "description": "The task, in a few words."},
                            "status": {"type": "string", "enum": ["pending", "in_progress", "done"]}
                        },
                        "required": ["text", "status"]
                    }
                }
            },
            "required": ["items"]
        }),
    }
}

/// The list a call carries, checked.
pub fn parse(call: &ToolCall) -> Result<Vec<TodoItem>, String> {
    let items = call.arguments.get("items").and_then(|v| v.as_array()).ok_or("todo needs an 'items' array")?;
    let mut out = Vec::new();
    for item in items.iter().take(MAX_ITEMS) {
        let text = item.get("text").and_then(|v| v.as_str()).map(str::trim).unwrap_or_default();
        if text.is_empty() {
            continue;
        }
        let status = match item.get("status").and_then(|v| v.as_str()).unwrap_or("pending") {
            "done" | "completed" | "complete" => TodoStatus::Done,
            "in_progress" | "in-progress" | "doing" | "active" => TodoStatus::InProgress,
            _ => TodoStatus::Pending,
        };
        out.push(TodoItem { text: text.chars().take(120).collect(), status });
    }
    Ok(out)
}

/// The list as text, for the model (after a compaction) and for the tool's result.
#[must_use]
pub fn describe(items: &[TodoItem]) -> String {
    if items.is_empty() {
        return "The task list is empty.".to_owned();
    }
    let lines: Vec<String> = items
        .iter()
        .map(|item| {
            let mark = match item.status {
                TodoStatus::Pending => "[ ]",
                TodoStatus::InProgress => "[~]",
                TodoStatus::Done => "[x]",
            };
            format!("{mark} {}", item.text)
        })
        .collect();
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_list_is_read_leniently_and_described_plainly() {
        let call = ToolCall {
            id: "1".into(),
            name: TOOL.into(),
            arguments: json!({"items": [
                {"text": "Read the code", "status": "done"},
                {"text": "  Fix the bug ", "status": "in-progress"},
                {"text": "", "status": "pending"},
                {"text": "Run tests"}
            ]}),
        };
        let items = parse(&call).unwrap();
        assert_eq!(items.len(), 3, "blank items are dropped");
        assert_eq!(items[1], TodoItem { text: "Fix the bug".into(), status: TodoStatus::InProgress });
        assert_eq!(items[2].status, TodoStatus::Pending, "no status means pending");
        assert_eq!(describe(&items), "[x] Read the code\n[~] Fix the bug\n[ ] Run tests");
        let bad = ToolCall { id: "2".into(), name: TOOL.into(), arguments: json!({"tasks": []}) };
        assert!(parse(&bad).unwrap_err().contains("items"));
    }
}
