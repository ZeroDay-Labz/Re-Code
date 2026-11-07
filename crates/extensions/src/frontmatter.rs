//! The YAML front matter of a `SKILL.md`, as the Agent Skills specification (agentskills.io) defines
//! it: `name` and `description` are required, `license`, `compatibility`, `metadata` (a string map)
//! and `allowed-tools` are optional. Only that subset of YAML is read, on purpose: a skill file is
//! untrusted input and a full YAML parser is a large surface for very little gain.

use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FrontMatter {
    pub name: Option<String>,
    pub description: Option<String>,
    pub license: Option<String>,
    pub compatibility: Option<String>,
    pub metadata: BTreeMap<String, String>,
    /// `allowed-tools`, split on whitespace.
    pub allowed_tools: Vec<String>,
    /// Keys the specification doesn't define, kept so they can be shown.
    pub extra: BTreeMap<String, String>,
}

/// Splits a skill file into its front matter and body. Without front matter, the whole text is body.
#[must_use]
pub fn split(text: &str) -> (Option<&str>, &str) {
    let mut lines = text.split_inclusive('\n');
    let Some(first) = lines.next() else { return (None, text) };
    if first.trim() != "---" {
        return (None, text);
    }
    let mut offset = first.len();
    for line in lines {
        if line.trim() == "---" {
            let front = &text[first.len()..offset];
            let body = &text[offset + line.len()..];
            return (Some(front), body);
        }
        offset += line.len();
    }
    (None, text)
}

/// Parses the front matter block (the text between the `---` lines).
#[must_use]
pub fn parse(front: &str) -> FrontMatter {
    let lines: Vec<&str> = front.lines().collect();
    let mut out = FrontMatter::default();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        index += 1;
        let trimmed = line.trim_end();
        if trimmed.trim().is_empty() || trimmed.trim_start().starts_with('#') || line.starts_with([' ', '\t']) {
            continue;
        }
        let Some((key, raw_value)) = trimmed.split_once(':') else { continue };
        let key = key.trim();
        let raw_value = raw_value.trim();
        // A nested block: `metadata:` followed by indented `k: v` lines.
        if raw_value.is_empty() {
            let mut map = BTreeMap::new();
            while index < lines.len() && (lines[index].starts_with([' ', '\t']) || lines[index].trim().is_empty()) {
                let nested = lines[index].trim();
                index += 1;
                if nested.is_empty() || nested.starts_with('#') {
                    continue;
                }
                if let Some((k, v)) = nested.split_once(':') {
                    map.insert(k.trim().to_owned(), unquote(v.trim()));
                }
            }
            if key == "metadata" {
                out.metadata = map;
            } else {
                out.extra.insert(
                    key.to_owned(),
                    map.into_iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(", "),
                );
            }
            continue;
        }
        // Folded or literal block scalars: the indented lines that follow.
        let value = if matches!(raw_value, ">" | "|" | ">-" | "|-" | ">+" | "|+") {
            let mut parts = Vec::new();
            while index < lines.len() && (lines[index].starts_with([' ', '\t']) || lines[index].trim().is_empty()) {
                parts.push(lines[index].trim());
                index += 1;
            }
            let joiner = if raw_value.starts_with('|') { "\n" } else { " " };
            parts.join(joiner).trim().to_owned()
        } else {
            unquote(raw_value)
        };
        match key {
            "name" => out.name = Some(value).filter(|v| !v.is_empty()),
            "description" => out.description = Some(value).filter(|v| !v.is_empty()),
            "license" => out.license = Some(value).filter(|v| !v.is_empty()),
            "compatibility" => out.compatibility = Some(value).filter(|v| !v.is_empty()),
            "allowed-tools" | "allowed_tools" => {
                out.allowed_tools = value.split_whitespace().map(str::to_owned).collect();
            }
            other => {
                out.extra.insert(other.to_owned(), value);
            }
        }
    }
    out
}

fn unquote(value: &str) -> String {
    let value = value.trim();
    let inner = value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')));
    inner.unwrap_or(value).to_owned()
}

/// Checks a skill name against the specification: 1–64 characters of `a-z`, `0-9` and `-`, not
/// starting or ending with a hyphen, no `--`, and the same as its directory's name.
pub fn check_name(name: &str, directory: Option<&str>) -> Result<(), String> {
    if name.is_empty() || name.chars().count() > 64 {
        return Err("The name must be 1 to 64 characters.".to_owned());
    }
    if !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return Err(format!("The name `{name}` may only use lowercase letters, digits and hyphens."));
    }
    if name.starts_with('-') || name.ends_with('-') || name.contains("--") {
        return Err(format!("The name `{name}` can't start or end with a hyphen, or repeat one."));
    }
    if let Some(dir) = directory
        && dir != name
    {
        return Err(format!("The name `{name}` must match its folder's name `{dir}`."));
    }
    Ok(())
}

/// Checks a description against the specification: present, at most 1024 characters.
pub fn check_description(description: Option<&str>) -> Result<(), String> {
    match description {
        None | Some("") => {
            Err("Has no description in its front matter, so a model couldn't tell when to use it.".to_owned())
        }
        Some(text) if text.chars().count() > 1024 => Err("The description is longer than 1024 characters.".to_owned()),
        Some(_) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_specification_examples_parse() {
        let text = "---\nname: pdf-processing\ndescription: Extract PDF text, fill forms, merge files. Use when handling PDFs.\nlicense: Apache-2.0\ncompatibility: Requires Python 3.14+ and uv\nmetadata:\n  author: example-org\n  version: \"1.0\"\nallowed-tools: Bash(git:*) Bash(jq:*) Read\n---\n\n# Body\nSteps.\n";
        let (front, body) = split(text);
        let front = parse(front.expect("front matter"));
        assert_eq!(front.name.as_deref(), Some("pdf-processing"));
        assert!(front.description.as_deref().unwrap().starts_with("Extract PDF text"));
        assert_eq!(front.license.as_deref(), Some("Apache-2.0"));
        assert_eq!(front.compatibility.as_deref(), Some("Requires Python 3.14+ and uv"));
        assert_eq!(front.metadata.get("author").map(String::as_str), Some("example-org"));
        assert_eq!(front.metadata.get("version").map(String::as_str), Some("1.0"));
        assert_eq!(front.allowed_tools, ["Bash(git:*)", "Bash(jq:*)", "Read"]);
        assert_eq!(body.trim(), "# Body\nSteps.");
    }

    #[test]
    fn folded_and_literal_values_and_missing_front_matter_are_handled() {
        let front = parse("name: x\ndescription: >\n  First part\n  second part.\nnotes: |\n  one\n  two\n");
        assert_eq!(front.description.as_deref(), Some("First part second part."));
        assert_eq!(front.extra.get("notes").map(String::as_str), Some("one\ntwo"));
        assert_eq!(split("# Just a body\n"), (None, "# Just a body\n"));
        assert_eq!(split("---\nname: x\n"), (None, "---\nname: x\n"), "an unterminated block isn't front matter");
    }

    #[test]
    fn names_follow_the_rules() {
        assert!(check_name("pdf-processing", Some("pdf-processing")).is_ok());
        assert!(check_name("PDF-Processing", None).is_err());
        assert!(check_name("-pdf", None).is_err());
        assert!(check_name("pdf--processing", None).is_err());
        assert!(check_name("pdf-processing", Some("pdf")).unwrap_err().contains("folder"));
        assert!(check_name(&"a".repeat(65), None).is_err());
        assert!(check_description(Some("Does things.")).is_ok());
        assert!(check_description(None).is_err());
        assert!(check_description(Some(&"x".repeat(1025))).is_err());
    }
}
