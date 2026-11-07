//! The `skill` tool: progressive disclosure for skills. The system prompt carries only each skill's
//! name and description; the model loads a skill's instructions with this tool when a task matches,
//! and can read one of the skill's reference files the same way. That keeps unused skills at a few
//! tokens each, however many are installed.

use recode_core::extensions::Skill;
use recode_core::message::{ToolCall, ToolDefinition};
use serde_json::json;
use std::path::{Path, PathBuf};

pub const TOOL: &str = "skill";
/// The most of a skill file handed to the model in one call.
const MAX_BYTES: usize = 64 * 1024;
/// The most of each description shown in the index.
const INDEX_DESCRIPTION_CHARS: usize = 240;

#[must_use]
pub fn tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: TOOL.to_owned(),
        description: "Load a skill's full instructions when the task matches its description in the skills list. \
                      Give its name. To read one of the skill's own files afterwards (a reference or script it \
                      mentions), give the name and the file's relative path."
            .to_owned(),
        parameters: json!({
            "type": "object",
            "properties": {
                "name": {"type": "string", "description": "The skill's name, exactly as listed."},
                "path": {"type": "string", "description": "Optional: a file inside the skill, relative to its folder, such as references/REFERENCE.md."}
            },
            "required": ["name"]
        }),
    }
}

/// The index for the system prompt: one line per usable skill. Empty when there are none.
#[must_use]
pub fn index(skills: &[Skill]) -> String {
    let mut usable: Vec<&Skill> = skills.iter().filter(|skill| skill.enabled && skill.problem.is_none()).collect();
    if usable.is_empty() {
        return String::new();
    }
    usable.sort_by(|a, b| a.name.cmp(&b.name));
    let mut out = String::from(
        "Skills (reusable instructions). When a task matches one, make a real tool call to the skill tool with \
         its name before starting (never write the call as text), then follow what it says. Don't load skills \
         that don't match.\n",
    );
    for skill in usable {
        let description: String = skill.description.chars().take(INDEX_DESCRIPTION_CHARS).collect();
        let cut = if skill.description.chars().count() > INDEX_DESCRIPTION_CHARS { "…" } else { "" };
        out.push_str(&format!("- {}: {description}{cut}\n", skill.name));
    }
    out.trim_end().to_owned()
}

/// Runs a call. Returns the text for the model and whether it succeeded.
#[must_use]
pub fn run(skills: &[Skill], call: &ToolCall) -> (String, bool) {
    let name = call.arguments.get("name").and_then(|v| v.as_str()).unwrap_or_default().trim();
    let Some(skill) = skills.iter().find(|skill| skill.name == name && skill.enabled && skill.problem.is_none()) else {
        let known: Vec<&str> =
            skills.iter().filter(|s| s.enabled && s.problem.is_none()).map(|s| s.name.as_str()).collect();
        return (
            format!(
                "Error: there's no skill called '{name}'. Available: {}.",
                if known.is_empty() { "none".to_owned() } else { known.join(", ") }
            ),
            false,
        );
    };
    let skill_file = Path::new(&skill.path);
    let Some(dir) = skill_file.parent() else { return ("Error: the skill's folder can't be found.".to_owned(), false) };
    match call.arguments.get("path").and_then(|v| v.as_str()).map(str::trim).filter(|p| !p.is_empty()) {
        None => match recode_extensions::skills::read_body(skill_file) {
            Some(body) => (format!("# Skill: {}\n\n{}", skill.name, clip(&body)), true),
            None => (format!("Error: the skill '{}' has no instructions in its SKILL.md.", skill.name), false),
        },
        Some(relative) => match confined(dir, relative) {
            Ok(path) => match std::fs::read(&path) {
                Ok(bytes) => match String::from_utf8(bytes) {
                    Ok(text) => (format!("# {}/{relative}\n\n{}", skill.name, clip(&text)), true),
                    Err(_) => (format!("Error: {relative} isn't a text file."), false),
                },
                Err(error) => (format!("Error: can't read {relative} in skill '{}': {error}", skill.name), false),
            },
            Err(reason) => (format!("Error: {reason}"), false),
        },
    }
}

/// Resolves a path inside the skill's folder, refusing anything that leaves it (including through
/// a symbolic link).
fn confined(dir: &Path, relative: &str) -> Result<PathBuf, String> {
    let candidate = Path::new(relative);
    if candidate.is_absolute() || candidate.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return Err(format!("{relative} isn't inside the skill's folder; give a path relative to it."));
    }
    let root = std::fs::canonicalize(dir).map_err(|error| format!("the skill's folder can't be opened: {error}"))?;
    let full =
        std::fs::canonicalize(dir.join(candidate)).map_err(|_| format!("there's no {relative} in this skill."))?;
    if !full.starts_with(&root) {
        return Err(format!("{relative} points outside the skill's folder, which isn't allowed."));
    }
    Ok(full)
}

fn clip(text: &str) -> String {
    if text.len() <= MAX_BYTES {
        return text.to_owned();
    }
    let mut end = MAX_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n\n[cut here: the file is longer than {} KB]", &text[..end], MAX_BYTES / 1024)
}

#[cfg(test)]
mod tests {
    use super::*;
    use recode_core::extensions::SkillSource;

    fn skill(dir: &Path, name: &str) -> Skill {
        Skill {
            name: name.to_owned(),
            description: "Review code for bugs. Use when asked to review.".to_owned(),
            path: dir.join("SKILL.md").display().to_string(),
            source: SkillSource::User,
            enabled: true,
            estimated_tokens: 10,
            problem: None,
            ..Skill::default()
        }
    }

    fn call(args: serde_json::Value) -> ToolCall {
        ToolCall { id: "c1".to_owned(), name: TOOL.to_owned(), arguments: args }
    }

    #[test]
    fn the_index_lists_only_usable_skills() {
        let dir = tempfile::tempdir().unwrap();
        let mut off = skill(dir.path(), "off");
        off.enabled = false;
        let mut broken = skill(dir.path(), "broken");
        broken.problem = Some("x".to_owned());
        let text = index(&[skill(dir.path(), "review"), off.clone(), broken]);
        assert!(text.contains("- review: Review code"), "{text}");
        assert!(!text.contains("off") && !text.contains("broken"), "{text}");
        assert_eq!(index(&[off.clone()]), "");
    }

    #[test]
    fn loads_the_body_and_confined_files_only() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("review");
        std::fs::create_dir_all(root.join("references")).unwrap();
        std::fs::write(root.join("SKILL.md"), "---\nname: review\ndescription: d\n---\n\nLook for bugs.\n").unwrap();
        std::fs::write(root.join("references/CHECKLIST.md"), "- nulls\n").unwrap();
        std::fs::write(dir.path().join("outside.txt"), "secret").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.path().join("outside.txt"), root.join("link.txt")).unwrap();
        let skills = [skill(&root, "review")];

        let (text, ok) = run(&skills, &call(json!({"name": "review"})));
        assert!(ok && text.contains("Look for bugs."), "{text}");
        let (text, ok) = run(&skills, &call(json!({"name": "review", "path": "references/CHECKLIST.md"})));
        assert!(ok && text.contains("- nulls"), "{text}");
        let (text, ok) = run(&skills, &call(json!({"name": "review", "path": "../outside.txt"})));
        assert!(!ok && text.contains("relative to it"), "{text}");
        #[cfg(unix)]
        {
            let (text, ok) = run(&skills, &call(json!({"name": "review", "path": "link.txt"})));
            assert!(!ok && text.contains("outside the skill's folder"), "{text}");
        }
        let (text, ok) = run(&skills, &call(json!({"name": "nope"})));
        assert!(!ok && text.contains("Available: review"), "{text}");
    }
}
