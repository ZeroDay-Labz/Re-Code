//! `/export`: the conversation as Markdown, for a file or the clipboard. What the user saw, in order:
//! requests, answers, tool calls with their output (cut at a few thousand characters), notices and
//! each turn's figures. Reasoning stays out unless it was shown.

use crate::transcript::{Entry, ToolStatus, Transcript};
use std::fmt::Write;

/// The most of a tool's output kept per call.
const OUTPUT_CAP: usize = 4_000;

/// The conversation as Markdown.
#[must_use]
pub fn markdown(transcript: &Transcript, title: &str, model: &str, show_reasoning: bool) -> String {
    let mut out = String::new();
    let heading = if title.trim().is_empty() { "re-code session" } else { title.trim() };
    let _ = writeln!(out, "# {heading}\n");
    if !model.is_empty() {
        let _ = writeln!(out, "_Model: {model}_\n");
    }
    for entry in transcript.iter() {
        match entry {
            Entry::User(text) => {
                let _ = writeln!(out, "## You\n\n{}\n", text.trim_end());
            }
            Entry::Assistant(text) => {
                let _ = writeln!(out, "{}\n", text.trim_end());
            }
            Entry::Reasoning(text) if show_reasoning => {
                let _ = writeln!(out, "<details><summary>Reasoning</summary>\n\n{}\n\n</details>\n", text.trim_end());
            }
            Entry::Reasoning(_) => {}
            Entry::Notice(text) => {
                for line in text.lines() {
                    let _ = writeln!(out, "> {line}");
                }
                out.push('\n');
            }
            Entry::Tool(tool) => {
                let status = match &tool.status {
                    ToolStatus::Succeeded { summary } => format!("ok, {summary}"),
                    ToolStatus::Failed { summary } => format!("failed, {summary}"),
                    ToolStatus::Denied => "denied".to_owned(),
                    ToolStatus::Interrupted => "interrupted".to_owned(),
                    ToolStatus::Running | ToolStatus::AwaitingApproval { .. } => "unfinished".to_owned(),
                };
                let _ = writeln!(out, "**{} {}** — {status}\n", tool.name, tool.target.trim());
                if let Some(diff) = &tool.diff {
                    let _ = writeln!(out, "```diff\n{}\n```\n", fence_safe(diff.trim_end()));
                } else if !tool.output.trim().is_empty() {
                    let mut text = tool.output.trim_end().to_owned();
                    if text.chars().count() > OUTPUT_CAP {
                        text = text.chars().take(OUTPUT_CAP).collect();
                        text.push_str("\n… (cut)");
                    }
                    let _ = writeln!(out, "```\n{}\n```\n", fence_safe(&text));
                }
            }
            Entry::TurnEnd(summary) => {
                let mut parts = Vec::new();
                if let Some(elapsed) = summary.elapsed {
                    parts.push(format!("{} s", elapsed.as_secs()));
                }
                if summary.tools > 0 {
                    parts.push(format!("{} tool call{}", summary.tools, if summary.tools == 1 { "" } else { "s" }));
                }
                if let (Some(prompt), Some(output)) = (summary.prompt_tokens, summary.output_tokens) {
                    parts.push(format!("{prompt} in / {output} out (reported)"));
                }
                if !parts.is_empty() {
                    let _ = writeln!(out, "_{}_\n", parts.join(" · "));
                }
            }
        }
    }
    out
}

/// Keeps a code fence intact when the text itself contains one.
fn fence_safe(text: &str) -> String {
    if text.contains("```") { text.replace("```", "`\u{200b}``") } else { text.to_owned() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcript::{ToolEntry, TurnSummary};
    use recode_core::protocol::TurnOutcome;

    #[test]
    fn the_export_keeps_the_order_and_cuts_long_output() {
        let mut transcript = Transcript::default();
        transcript.push(Entry::User("fix the retry loop".into()));
        transcript.push(Entry::Reasoning("thinking…".into()));
        transcript.push(Entry::Tool(ToolEntry {
            id: "1".into(),
            name: "shell".into(),
            target: "cargo test".into(),
            status: ToolStatus::Succeeded { summary: "exit 0".into() },
            output: "x".repeat(5000),
            diff: None,
            elapsed: None,
        }));
        transcript.push(Entry::Assistant("Done. ```rust\nlet x = 1;\n```".into()));
        transcript.push(Entry::Notice("Compacted the conversation".into()));
        transcript.push(Entry::TurnEnd(TurnSummary {
            outcome: TurnOutcome::Completed,
            elapsed: Some(std::time::Duration::from_secs(12)),
            tools: 1,
            prompt_tokens: Some(900),
            cached_tokens: None,
            output_tokens: Some(40),
        }));
        let text = markdown(&transcript, "fix the retry loop", "gpt-oss:20b", false);
        assert!(
            text.starts_with("# fix the retry loop\n\n_Model: gpt-oss:20b_\n\n## You\n\nfix the retry loop\n"),
            "{text}"
        );
        assert!(!text.contains("thinking…"), "reasoning stays out unless shown");
        assert!(text.contains("**shell cargo test** — ok, exit 0"), "{text}");
        assert!(text.contains("… (cut)"), "{text}");
        assert!(text.contains("> Compacted the conversation"), "{text}");
        assert!(text.contains("_12 s · 1 tool call · 900 in / 40 out (reported)_"), "{text}");
        let shown = markdown(&transcript, "", "", true);
        assert!(shown.contains("<details><summary>Reasoning</summary>") && shown.starts_with("# re-code session"));
    }
}
