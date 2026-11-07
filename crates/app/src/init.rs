//! `/init`: a first `RE-CODE.md` for a project, written from what the tree shows and nothing else.
//! No model call and no guesses: a section appears only when a file proves it (a Cargo workspace, a
//! `scripts/check.sh`, `package.json` scripts, a Makefile). The user edits it from there.

use std::path::Path;

/// The file `/init` writes.
pub const FILE_NAME: &str = "RE-CODE.md";

/// The text for a new `RE-CODE.md`, or `None` when the folder can't be read at all.
#[must_use]
pub fn generate(project: &Path) -> Option<String> {
    let name = project.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut out = format!("# {name}\n\nInstructions for re-code (also read: AGENTS.md, CLAUDE.md).\n");
    let entries = std::fs::read_dir(project).ok()?;
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    for entry in entries.flatten() {
        let file_name = entry.file_name().to_string_lossy().into_owned();
        if file_name.starts_with('.') || matches!(file_name.as_str(), "target" | "node_modules" | "__pycache__") {
            continue;
        }
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => dirs.push(file_name),
            Ok(kind) if kind.is_file() => files.push(file_name),
            _ => {}
        }
    }
    dirs.sort();
    files.sort();

    let mut commands: Vec<(String, String)> = Vec::new();
    let mut about = Vec::new();
    if files.iter().any(|f| f == "Cargo.toml") {
        let cargo = std::fs::read_to_string(project.join("Cargo.toml")).unwrap_or_default();
        let members = toml_members(&cargo);
        if members.is_empty() {
            about.push("A Rust crate (`Cargo.toml`).".to_owned());
        } else {
            about.push(format!("A Rust workspace with crates: {}.", members.join(", ")));
        }
        commands.push(("Build".into(), "cargo build".into()));
        commands.push(("Test".into(), "cargo test --workspace".into()));
        commands.push(("Lint".into(), "cargo clippy --workspace --all-targets -- -D warnings".into()));
        commands.push(("Format".into(), "cargo fmt --all".into()));
    }
    if project.join("scripts/check.sh").is_file() {
        commands.push(("Full check before finishing".into(), "./scripts/check.sh".into()));
    }
    if files.iter().any(|f| f == "package.json") {
        let package = std::fs::read_to_string(project.join("package.json")).unwrap_or_default();
        let scripts = package_scripts(&package);
        about.push("A Node project (`package.json`).".to_owned());
        for script in scripts {
            commands.push((format!("npm run {script}"), format!("npm run {script}")));
        }
    }
    if files.iter().any(|f| f == "pyproject.toml") {
        about.push("A Python project (`pyproject.toml`).".to_owned());
        if project.join("tests").is_dir() {
            commands.push(("Test".into(), "pytest".into()));
        }
    }
    if files.iter().any(|f| f == "go.mod") {
        about.push("A Go module (`go.mod`).".to_owned());
        commands.push(("Build".into(), "go build ./...".into()));
        commands.push(("Test".into(), "go test ./...".into()));
    }
    if let Some(makefile) = files.iter().find(|f| *f == "Makefile" || *f == "makefile") {
        let text = std::fs::read_to_string(project.join(makefile)).unwrap_or_default();
        for target in make_targets(&text) {
            commands.push((format!("make {target}"), format!("make {target}")));
        }
    }

    if !about.is_empty() {
        out.push_str("\n## About\n\n");
        for line in &about {
            out.push_str(&format!("- {line}\n"));
        }
    }
    if !dirs.is_empty() {
        out.push_str("\n## Layout\n\n");
        for dir in dirs.iter().take(20) {
            out.push_str(&format!("- `{dir}/`\n"));
        }
    }
    if !commands.is_empty() {
        out.push_str("\n## Commands\n\n");
        let mut seen = std::collections::BTreeSet::new();
        for (what, command) in &commands {
            if seen.insert(command.clone()) {
                out.push_str(&format!("- {what}: `{command}`\n"));
            }
        }
    }
    let others: Vec<&String> = files.iter().filter(|f| matches!(f.as_str(), "AGENTS.md" | "CLAUDE.md")).collect();
    if !others.is_empty() {
        out.push_str("\n## See also\n\n");
        for file in others {
            out.push_str(&format!("- `{file}`, which re-code reads as well.\n"));
        }
    }
    out.push_str("\n## Compact instructions\n\nKeep the exact commands and file paths from this file.\n");
    Some(out)
}

/// `members = ["a", "b"]` from a workspace manifest, or nothing.
fn toml_members(cargo: &str) -> Vec<String> {
    let Some(start) = cargo.find("members") else { return Vec::new() };
    let rest = &cargo[start..];
    let Some(open) = rest.find('[') else { return Vec::new() };
    let Some(close) = rest[open..].find(']') else { return Vec::new() };
    rest[open + 1..open + close]
        .split(',')
        .map(|item| item.trim().trim_matches('"').trim().to_owned())
        .filter(|item| !item.is_empty() && !item.starts_with('#'))
        .collect()
}

/// The script names of a `package.json`.
fn package_scripts(package: &str) -> Vec<String> {
    let value: serde_json::Value = serde_json::from_str(package).unwrap_or_default();
    value
        .get("scripts")
        .and_then(|scripts| scripts.as_object())
        .map(|scripts| scripts.keys().cloned().collect())
        .unwrap_or_default()
}

/// Plain targets of a Makefile (`name:` at the start of a line), without pattern or special ones.
fn make_targets(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| {
            let (name, _) = line.split_once(':')?;
            let name = name.trim();
            (!name.is_empty()
                && !name.starts_with('.')
                && !name.starts_with('#')
                && !name.contains(['%', '$', ' ', '\t', '='])
                && !line.starts_with(['\t', ' ']))
            .then(|| name.to_owned())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rust_workspace_gets_its_crates_commands_and_the_check_script() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("myproj");
        std::fs::create_dir_all(root.join("crates")).unwrap();
        std::fs::create_dir_all(root.join("scripts")).unwrap();
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = [\"crates/core\", \"crates/app\"]\n").unwrap();
        std::fs::write(root.join("scripts/check.sh"), "#!/bin/sh\n").unwrap();
        std::fs::write(root.join("AGENTS.md"), "x\n").unwrap();
        let text = generate(&root).unwrap();
        assert!(text.starts_with("# myproj\n"), "{text}");
        assert!(text.contains("crates: crates/core, crates/app"), "{text}");
        assert!(text.contains("- `crates/`") && !text.contains("target/"), "{text}");
        assert!(text.contains("`./scripts/check.sh`") && text.contains("`cargo test --workspace`"), "{text}");
        assert!(text.contains("`AGENTS.md`, which re-code reads as well"), "{text}");
        assert!(text.ends_with("## Compact instructions\n\nKeep the exact commands and file paths from this file.\n"));
    }

    #[test]
    fn node_make_and_empty_folders_only_say_what_is_there() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), r#"{"scripts": {"test": "jest", "build": "tsc"}}"#).unwrap();
        std::fs::write(dir.path().join("Makefile"), "all: build\n\tgo\n.PHONY: all\n%.o: %.c\nlint:\n").unwrap();
        let text = generate(dir.path()).unwrap();
        assert!(text.contains("`npm run build`") && text.contains("`npm run test`"), "{text}");
        assert!(text.contains("`make all`") && text.contains("`make lint`") && !text.contains("PHONY"), "{text}");
        assert!(!text.contains("## Layout"), "no folders, no section: {text}");

        let empty = tempfile::tempdir().unwrap();
        let text = generate(empty.path()).unwrap();
        assert!(!text.contains("## About") && !text.contains("## Commands"), "{text}");
        assert!(text.contains("## Compact instructions"));
        assert_eq!(generate(Path::new("/nonexistent/folder")), None);
    }
}
