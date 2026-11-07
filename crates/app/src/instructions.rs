//! Instruction files: standing guidance the model gets with every request. re-code's own file is
//! `RE-CODE.md`; `AGENTS.md` and `CLAUDE.md` are read too, so a project set up for another agent works
//! as it is. Levels, in the order they're sent: the user's own (in re-code's config folder, plus
//! Claude Code's `~/.claude/CLAUDE.md` when present), the project root, `.re-code/instructions.md`, and
//! files in subdirectories, which load only once the model touches something below them.
//!
//! Project files are untrusted input: they load only after the folder is trusted (`trust.rs`), and
//! `@path` imports never leave the project (or the home folder, for the user's files).

use recode_core::usage::ComponentKind;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The file names read at each level, in order. Duplicated content is dropped.
pub const ROOT_NAMES: &[&str] = &["RE-CODE.md", "AGENTS.md", "CLAUDE.md", "RE-CODE.local.md", "CLAUDE.local.md"];
const PROJECT_EXTRA: &str = ".re-code/instructions.md";
const MAX_IMPORT_DEPTH: usize = 5;
/// Files longer than this aren't sent; a path reference and a notice stand in.
pub const MAX_BYTES: usize = 32 * 1024;
/// Files costing more than this (estimated) get a size warning.
pub const WARN_TOKENS: u64 = 4_000;

/// Where a file comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    User,
    Project,
    /// A subdirectory's file, loaded when the model works below it.
    Nested,
}

impl Scope {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::User => "yours",
            Self::Project => "project",
            Self::Nested => "folder",
        }
    }
}

/// One instruction file, read and with its imports resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstructionFile {
    pub scope: Scope,
    pub path: PathBuf,
    /// The text sent to the model, imports included; empty when `problem` says why.
    pub text: String,
    pub tokens: u64,
    pub trusted: bool,
    pub imports: Vec<PathBuf>,
    pub problem: Option<String>,
}

/// Everything found, with what's actually sent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Instructions {
    pub files: Vec<InstructionFile>,
    /// A fingerprint of the rendered text, to notice changes between turns.
    pub hash: u64,
}

impl Instructions {
    /// The block for the system prompt: each usable file under a heading naming it.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        for file in self.files.iter().filter(|file| file.trusted && file.problem.is_none() && !file.text.is_empty()) {
            if !out.is_empty() {
                out.push_str("\n\n");
            }
            out.push_str(&format!(
                "Instructions from {} ({}):\n{}",
                file.path.display(),
                file.scope.label(),
                file.text.trim()
            ));
        }
        out
    }

    /// The part for the system prompt, when there is anything to send.
    #[must_use]
    pub fn part(&self) -> Option<(ComponentKind, String)> {
        let text = self.render();
        (!text.is_empty()).then_some((ComponentKind::ProjectInstructions, text))
    }

    /// The "Compact instructions" section of the usable files, if any has one.
    #[must_use]
    pub fn compact_instructions(&self) -> Option<String> {
        let sections: Vec<String> = self
            .files
            .iter()
            .filter(|file| file.trusted && file.problem.is_none())
            .filter_map(|file| section(&file.text, "compact instructions"))
            .collect();
        (!sections.is_empty()).then(|| sections.join("\n"))
    }

    /// Project files held back because the folder isn't trusted.
    #[must_use]
    pub fn untrusted(&self) -> Vec<&InstructionFile> {
        self.files.iter().filter(|file| !file.trusted).collect()
    }

    /// Files whose estimated cost deserves a warning.
    #[must_use]
    pub fn oversized(&self) -> Vec<&InstructionFile> {
        self.files.iter().filter(|file| file.trusted && (file.tokens > WARN_TOKENS || file.problem.is_some())).collect()
    }
}

/// Reads the user's and the project's instruction files. `trusted` says whether the project folder is.
#[must_use]
pub fn load(project: Option<&Path>, user_dir: &Path, home: Option<&Path>, trusted: bool) -> Instructions {
    let mut files = Vec::new();
    let mut seen = BTreeSet::new();
    for name in ROOT_NAMES {
        push_file(&mut files, &mut seen, Scope::User, &user_dir.join(name), user_dir, true);
    }
    if let Some(home) = home {
        push_file(&mut files, &mut seen, Scope::User, &home.join(".claude/CLAUDE.md"), home, true);
    }
    if let Some(project) = project {
        for name in ROOT_NAMES {
            push_file(&mut files, &mut seen, Scope::Project, &project.join(name), project, trusted);
        }
        push_file(&mut files, &mut seen, Scope::Project, &project.join(PROJECT_EXTRA), project, trusted);
    }
    let hash = fnv(Instructions { files: files.clone(), hash: 0 }.render().as_bytes());
    Instructions { files, hash }
}

/// Instruction files in the directories between the project root and `touched` (a path relative to
/// the root, or absolute inside it), not counting the root itself and not counting `already` loaded ones.
#[must_use]
pub fn nested_for(
    project: &Path,
    touched: &Path,
    already: &mut BTreeSet<PathBuf>,
    trusted: bool,
) -> Vec<InstructionFile> {
    let mut out = Vec::new();
    if !trusted {
        return out;
    }
    let relative = touched.strip_prefix(project).unwrap_or(touched);
    if relative.is_absolute() {
        return out;
    }
    let mut dir = project.to_path_buf();
    let mut seen = BTreeSet::new();
    for component in relative.parent().map(Path::components).into_iter().flatten() {
        dir.push(component);
        for name in ROOT_NAMES {
            let path = dir.join(name);
            if path.is_file() && already.insert(path.clone()) {
                push_file(&mut out, &mut seen, Scope::Nested, &path, project, true);
            }
        }
    }
    out
}

fn push_file(
    files: &mut Vec<InstructionFile>,
    seen: &mut BTreeSet<u64>,
    scope: Scope,
    path: &Path,
    confine_to: &Path,
    trusted: bool,
) {
    let Ok(metadata) = std::fs::symlink_metadata(path) else { return };
    if !metadata.is_file() {
        return;
    }
    let mut file = read_file(scope, path, confine_to, trusted);
    if file.problem.is_none() && !seen.insert(fnv(file.text.as_bytes())) {
        return; // the same content by another name (CLAUDE.md copied to AGENTS.md, say)
    }
    if file.problem.is_none() && file.text.trim().is_empty() {
        file.problem = Some("the file is empty".to_owned());
    }
    files.push(file);
}

fn read_file(scope: Scope, path: &Path, confine_to: &Path, trusted: bool) -> InstructionFile {
    let mut file = InstructionFile {
        scope,
        path: path.to_path_buf(),
        text: String::new(),
        tokens: 0,
        trusted,
        imports: Vec::new(),
        problem: None,
    };
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            file.problem = Some(format!("can't be read: {error}"));
            return file;
        }
    };
    if bytes.len() > MAX_BYTES {
        file.problem = Some(format!("{} KB is too long to send; only its path is mentioned", bytes.len() / 1024));
        file.tokens = crate::compact::tokens_at(bytes.len(), 3.0);
        return file;
    }
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let mut visiting = vec![path.to_path_buf()];
    let text =
        if trusted { resolve_imports(&text, path, confine_to, &mut visiting, &mut file.imports, 0) } else { text };
    file.tokens = crate::compact::tokens_at(text.chars().count(), 3.0);
    file.text = text;
    file
}

/// Replaces `@path` lines with the file they name. Paths are relative to the importing file or `~/`,
/// must stay inside `confine_to`, and may nest five deep without cycles.
fn resolve_imports(
    text: &str,
    from: &Path,
    confine_to: &Path,
    visiting: &mut Vec<PathBuf>,
    imports: &mut Vec<PathBuf>,
    depth: usize,
) -> String {
    let mut out = String::new();
    for line in text.lines() {
        let trimmed = line.trim();
        let Some(target) = trimmed.strip_prefix('@').filter(|rest| !rest.is_empty() && !rest.starts_with('@')) else {
            out.push_str(line);
            out.push('\n');
            continue;
        };
        // A bare mention like "@someone" or an email isn't an import; a path has a slash or an extension.
        if !target.contains('/') && !target.contains('.') {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        let base = from.parent().unwrap_or(confine_to);
        let candidate = if let Some(rest) = target.strip_prefix("~/") {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(rest)).unwrap_or_else(|| base.join(target))
        } else {
            base.join(target)
        };
        match import_file(&candidate, confine_to, visiting, depth) {
            Ok(imported) => {
                imports.push(candidate.clone());
                let nested = resolve_imports(&imported, &candidate, confine_to, visiting, imports, depth + 1);
                out.push_str(&format!("(from {})\n{}", target, nested.trim_end()));
                out.push('\n');
            }
            Err(reason) => {
                out.push_str(&format!("(the import @{target} was skipped: {reason})\n"));
            }
        }
    }
    out
}

fn import_file(
    candidate: &Path,
    confine_to: &Path,
    visiting: &mut Vec<PathBuf>,
    depth: usize,
) -> Result<String, String> {
    if depth >= MAX_IMPORT_DEPTH {
        return Err("imports nest too deep".to_owned());
    }
    let root = std::fs::canonicalize(confine_to).map_err(|error| error.to_string())?;
    let full = std::fs::canonicalize(candidate).map_err(|_| "no such file".to_owned())?;
    if !full.starts_with(&root) {
        return Err(format!("it's outside {}", confine_to.display()));
    }
    if visiting.contains(&full) {
        return Err("it imports itself".to_owned());
    }
    let bytes = std::fs::read(&full).map_err(|error| error.to_string())?;
    if bytes.len() > MAX_BYTES {
        return Err("it's too long".to_owned());
    }
    visiting.push(full);
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// The body under a Markdown heading whose text (lowercased) is `title`, up to the next heading of the
/// same or a higher level.
#[must_use]
pub fn section(text: &str, title: &str) -> Option<String> {
    let mut level = 0;
    let mut body = Vec::new();
    for line in text.lines() {
        let hashes = line.chars().take_while(|c| *c == '#').count();
        let is_heading = hashes > 0 && line.chars().nth(hashes).is_some_and(char::is_whitespace);
        if level == 0 {
            if is_heading && line[hashes..].trim().to_ascii_lowercase() == title {
                level = hashes;
            }
            continue;
        }
        if is_heading && hashes <= level {
            break;
        }
        body.push(line);
    }
    if level == 0 {
        return None;
    }
    let body = body.join("\n").trim().to_owned();
    (!body.is_empty()).then_some(body)
}

/// Adds a `# note` the user typed to an instruction file, under a `## Notes` heading (created at the
/// end if missing). A missing file is started with a title line first.
pub fn append_note(path: &Path, text: &str) -> std::io::Result<()> {
    let mut content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => "# Instructions for re-code\n".to_owned(),
        Err(error) => return Err(error),
    };
    let note = format!("- {}", text.trim());
    let heading_at = content.lines().position(|line| line.trim().eq_ignore_ascii_case("## notes"));
    match heading_at {
        Some(index) => {
            // Insert after the last non-empty line of the section.
            let mut lines: Vec<String> = content.lines().map(str::to_owned).collect();
            let mut end = index + 1;
            while end < lines.len() && !lines[end].starts_with('#') {
                end += 1;
            }
            let mut insert_at = end;
            while insert_at > index + 1 && lines[insert_at - 1].trim().is_empty() {
                insert_at -= 1;
            }
            lines.insert(insert_at, note);
            content = lines.join("\n");
            content.push('\n');
        }
        None => {
            if !content.ends_with('\n') {
                content.push('\n');
            }
            content.push_str(&format!("\n## Notes\n\n{note}\n"));
        }
    }
    std::fs::write(path, content)
}

fn fnv(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_load_in_order_without_duplicates_and_project_ones_wait_for_trust() {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("config");
        let project = dir.path().join("project");
        std::fs::create_dir_all(&user).unwrap();
        std::fs::create_dir_all(project.join(".re-code")).unwrap();
        std::fs::write(user.join("RE-CODE.md"), "Answer briefly.\n").unwrap();
        std::fs::write(project.join("AGENTS.md"), "Run cargo test.\n\n## Compact instructions\nKeep commands.\n")
            .unwrap();
        std::fs::write(project.join("CLAUDE.md"), "Run cargo test.\n\n## Compact instructions\nKeep commands.\n")
            .unwrap();
        std::fs::write(project.join(".re-code/instructions.md"), "Prefer small diffs.\n").unwrap();

        let untrusted = load(Some(&project), &user, None, false);
        let rendered = untrusted.render();
        assert!(rendered.contains("Answer briefly."), "{rendered}");
        assert!(!rendered.contains("cargo test"), "project files wait for trust: {rendered}");
        assert_eq!(untrusted.untrusted().len(), 2, "AGENTS.md and the .re-code file; CLAUDE.md is the same text");
        assert_eq!(untrusted.compact_instructions(), None);

        let trusted = load(Some(&project), &user, None, true);
        let rendered = trusted.render();
        let user_at = rendered.find("Answer briefly.").unwrap();
        let project_at = rendered.find("Run cargo test.").unwrap();
        let extra_at = rendered.find("Prefer small diffs.").unwrap();
        assert!(user_at < project_at && project_at < extra_at, "user, then project root, then .re-code: {rendered}");
        assert_eq!(rendered.matches("Run cargo test.").count(), 1, "duplicate content is sent once");
        assert_eq!(trusted.compact_instructions().as_deref(), Some("Keep commands."));
        assert_ne!(trusted.hash, untrusted.hash);
        assert_eq!(trusted.part().unwrap().0, ComponentKind::ProjectInstructions);
    }

    #[test]
    fn imports_are_resolved_inside_the_project_only_and_cycles_stop() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("p");
        std::fs::create_dir_all(project.join("docs")).unwrap();
        std::fs::write(dir.path().join("secret.md"), "top secret\n").unwrap();
        std::fs::write(project.join("RE-CODE.md"), "Top.\n@docs/style.md\n@../secret.md\n@missing.md\n@someone\n")
            .unwrap();
        std::fs::write(project.join("docs/style.md"), "Style rules.\n@../RE-CODE.md\n").unwrap();
        let loaded = load(Some(&project), &dir.path().join("nouser"), None, true);
        let text = &loaded.files[0].text;
        assert!(text.contains("Style rules."), "{text}");
        assert!(!text.contains("top secret"), "an import can't leave the project: {text}");
        assert!(text.contains("skipped: it's outside"), "{text}");
        assert!(text.contains("skipped: no such file"), "{text}");
        assert!(text.contains("@someone"), "a mention isn't an import: {text}");
        assert!(text.contains("skipped: it imports itself"), "{text}");
        assert_eq!(loaded.files[0].imports.len(), 1);
    }

    #[test]
    fn oversized_files_are_mentioned_not_sent_and_nested_files_load_once() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        std::fs::create_dir_all(project.join("src/deep")).unwrap();
        std::fs::write(project.join("RE-CODE.md"), "x".repeat(MAX_BYTES + 1)).unwrap();
        std::fs::write(project.join("src/AGENTS.md"), "In src, keep modules small.\n").unwrap();
        let loaded = load(Some(&project), &project.join("nouser"), None, true);
        assert!(loaded.files[0].problem.as_deref().unwrap().contains("too long"));
        assert_eq!(loaded.render(), "", "a file that's too long sends nothing");
        assert_eq!(loaded.oversized().len(), 1);

        let mut already = BTreeSet::new();
        let nested = nested_for(&project, Path::new("src/deep/lib.rs"), &mut already, true);
        assert_eq!(nested.len(), 1);
        assert_eq!(nested[0].scope, Scope::Nested);
        assert!(nested[0].text.contains("keep modules small"));
        assert!(nested_for(&project, Path::new("src/other.rs"), &mut already, true).is_empty(), "loaded once");
        assert!(nested_for(&project, Path::new("src/x.rs"), &mut BTreeSet::new(), false).is_empty(), "untrusted");
        assert!(nested_for(&project, Path::new("/etc/passwd"), &mut BTreeSet::new(), true).is_empty());
    }

    #[test]
    fn notes_are_appended_under_a_notes_heading_creating_what_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("RE-CODE.md");
        append_note(&path, "prefer small diffs").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text, "# Instructions for re-code\n\n## Notes\n\n- prefer small diffs\n");
        append_note(&path, "run the check script").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.ends_with("- prefer small diffs\n- run the check script\n"), "{text}");
        std::fs::write(&path, "# T\n\n## Notes\n\n- one\n\n## Other\nkeep\n").unwrap();
        append_note(&path, "two").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text, "# T\n\n## Notes\n\n- one\n- two\n\n## Other\nkeep\n");
    }

    #[test]
    fn sections_are_cut_at_the_next_heading_of_the_same_level() {
        let text = "# Title\nintro\n## Compact instructions\nkeep a\nkeep b\n### Sub\nmore\n## Other\nno\n";
        assert_eq!(section(text, "compact instructions").as_deref(), Some("keep a\nkeep b\n### Sub\nmore"));
        assert_eq!(section(text, "missing"), None);
        assert_eq!(section("## Compact instructions\n\n", "compact instructions"), None);
    }
}
