//! Project trust: a repository can carry skills and other executable configuration, so nothing from
//! a project folder loads until the user has looked at it and said yes. Trust is pinned to the
//! content, so a change (a pull, a branch switch) asks again (`docs/SECURITY.md`, "Project trust").

use recode_store::Store;
use std::path::{Path, PathBuf};

const KEY_PREFIX: &str = "trust:";

/// What a project folder would load, and a fingerprint of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectConfig {
    /// Skill directories under `.re-code/skills`, by name.
    pub skills: Vec<String>,
    /// Other files under `.re-code/` that re-code reads (relative paths).
    pub files: Vec<String>,
    pub fingerprint: String,
}

/// Reads the project's `.re-code/` configuration and fingerprints it. Only text is hashed; the
/// fingerprint changes when any loadable file changes, appears or disappears.
#[must_use]
pub fn project_config(project: &Path) -> ProjectConfig {
    let root = project.join(".re-code");
    let mut skills = Vec::new();
    let mut files = Vec::new();
    let mut hasher = Fnv::new();
    let mut entries: Vec<PathBuf> = Vec::new();
    collect(&root, &mut entries, 0);
    // Instruction files at the project root load into every request, so trust covers them too.
    for name in crate::instructions::ROOT_NAMES {
        let path = project.join(name);
        if std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file()) {
            entries.push(path);
        }
    }
    entries.sort();
    for path in entries {
        let relative = match path.strip_prefix(&root) {
            Ok(inside) => inside.display().to_string(),
            Err(_) => format!("./{}", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()),
        };
        if let Some(name) = relative.strip_prefix("skills/").and_then(|rest| rest.strip_suffix("/SKILL.md")) {
            skills.push(name.to_owned());
        } else if !relative.starts_with("skills/") {
            files.push(relative.clone());
        }
        hasher.write(relative.as_bytes());
        hasher.write(&[0]);
        if let Ok(bytes) = std::fs::read(&path) {
            hasher.write(&bytes);
        }
        hasher.write(&[0xff]);
    }
    ProjectConfig { skills, files, fingerprint: format!("{:016x}", hasher.finish()) }
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 6 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = std::fs::symlink_metadata(&path) else { continue };
        if metadata.file_type().is_symlink() {
            continue; // a link out of the project is never loaded, so it isn't fingerprinted either
        }
        if metadata.is_dir() {
            collect(&path, out, depth + 1);
        } else if metadata.len() <= 4 * 1024 * 1024 {
            out.push(path);
        }
    }
}

fn key(project: &Path) -> String {
    format!("{KEY_PREFIX}{}", canonical_key(project))
}

/// The project's identity in the database: its canonical path.
pub(crate) fn canonical_key(project: &Path) -> String {
    std::fs::canonicalize(project).unwrap_or_else(|_| project.to_path_buf()).display().to_string()
}

/// Whether this folder's current configuration is trusted.
#[must_use]
pub fn is_trusted(store: Option<&Store>, project: &Path, config: &ProjectConfig) -> bool {
    let Some(store) = store else { return false };
    store.setting(&key(project)).ok().flatten().is_some_and(|stored| stored == config.fingerprint)
}

/// Remembers that the user trusts this folder's configuration as it is now.
pub fn trust(store: &Store, project: &Path, config: &ProjectConfig) -> Result<(), String> {
    store.set_setting(&key(project), &config.fingerprint).map_err(|error| error.to_string())
}

/// Forgets the trust.
#[cfg_attr(not(test), allow(dead_code, reason = "wired to /untrust once there is a place to show it"))]
pub fn revoke(store: &Store, project: &Path) -> Result<(), String> {
    store.delete_setting(&key(project)).map_err(|error| error.to_string())
}

/// A short, stable name for a project path, for files kept per project outside it.
pub(crate) fn short_hash(text: &str) -> String {
    let mut hasher = Fnv::new();
    hasher.write(text.as_bytes());
    format!("{:016x}", hasher.finish())
}

/// FNV-1a, 64-bit: enough to notice a change, with no dependency. Not a security boundary (the
/// user's own disk is the input).
struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
        }
    }
    fn finish(&self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fingerprint_follows_the_content_and_trust_is_pinned_to_it() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path();
        let empty = project_config(project);
        assert!(empty.skills.is_empty() && empty.files.is_empty());
        std::fs::create_dir_all(project.join(".re-code/skills/review")).unwrap();
        std::fs::write(project.join(".re-code/skills/review/SKILL.md"), "---\nname: review\ndescription: d\n---\nx")
            .unwrap();
        let first = project_config(project);
        assert_eq!(first.skills, ["review"]);
        assert_ne!(first.fingerprint, empty.fingerprint);
        // Root instruction files count: adding one changes the fingerprint and is listed.
        std::fs::write(project.join("AGENTS.md"), "Run the tests.\n").unwrap();
        let with_agents = project_config(project);
        assert_ne!(with_agents.fingerprint, first.fingerprint);
        assert!(with_agents.files.iter().any(|f| f == "./AGENTS.md"), "{:?}", with_agents.files);
        std::fs::remove_file(project.join("AGENTS.md")).unwrap();

        let store = Store::open(&dir.path().join("db.sqlite")).unwrap();
        assert!(!is_trusted(Some(&store), project, &first));
        trust(&store, project, &first).unwrap();
        assert!(is_trusted(Some(&store), project, &first));
        // A change to the skill asks again.
        std::fs::write(
            project.join(".re-code/skills/review/SKILL.md"),
            "---\nname: review\ndescription: d\n---\nchanged",
        )
        .unwrap();
        let second = project_config(project);
        assert!(!is_trusted(Some(&store), project, &second));
        revoke(&store, project).unwrap();
        assert!(!is_trusted(Some(&store), project, &first));
        assert!(!is_trusted(None, project, &first), "without a database nothing is trusted");
    }
}
