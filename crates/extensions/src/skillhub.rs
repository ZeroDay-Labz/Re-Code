//! Installing skills from hubs the way Hermes Agent does, without running anything that was fetched:
//! `official/security/godmode`, a bare name looked up in the Hermes index, a skills.sh link, a GitHub
//! repository (with a folder and a ref), a direct `SKILL.md` link, or a local folder. Everything goes
//! into a staging folder first, is read and scanned, and is shown to the user; only then is it moved
//! into re-code's skills folder, with a `.re-code-source.toml` recording where it came from and which
//! commit, so it can be updated.

use crate::fetch::{Caps, EntryKind, Fetcher, Folder, check_relative, human, utc_stamp, write_file};
use crate::{frontmatter, scan, skills};
use recode_core::extensions::{
    Declares, Finding, InstallOutcome, InstallPreview, InstalledSource, Replacing, Severity, SkillCandidate,
    SkillSearchHit, SkillSource, validate_name,
};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// The file in an installed skill's folder that says where it came from.
pub const SOURCE_FILE: &str = ".re-code-source.toml";
/// Hermes Agent's own skills live here.
const HERMES_OWNER: &str = "NousResearch";
const HERMES_REPO: &str = "hermes-agent";
const HERMES_SKILLS_DIR: &str = "optional-skills";
/// The Hermes index is about 42 MB (measured 2026-11-07) and lists 100k skills; it's kept for a day.
const HERMES_INDEX_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
const HERMES_INDEX_LIMIT: u64 = 96 * 1024 * 1024;
/// How many skill folders to offer when a repository holds several.
const MAX_CANDIDATES: usize = 40;
const SKILL_TEXT_LIMIT: usize = 64 * 1024;
const STAGING_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// What the user asked for, parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tap {
    /// `official/<category>/<skill>`: Hermes Agent's own skills.
    Official { path: String },
    /// A bare name or `hermes/<identifier>`, found in the Hermes index.
    HermesLookup { query: String },
    /// `skills.sh/<owner>/<repo>/<slug>`.
    SkillsSh { owner: String, repo: String, slug: String },
    /// `owner/repo[/folder][@ref]`, or a github.com link.
    GitHub { owner: String, repo: String, subdir: String, reference: Option<String> },
    /// A direct link to one `SKILL.md`.
    SkillFile { url: String },
    /// A folder on this computer.
    Local { path: PathBuf },
}

impl Tap {
    /// Reads any of the accepted forms. The error says what is accepted.
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("Enter a skill to install: official/security/godmode, a name, owner/repo/folder, a GitHub or skills.sh link, a SKILL.md link, or a folder.".to_owned());
        }
        if text.starts_with('/') || text.starts_with('~') || text.starts_with("./") || text.starts_with("../") {
            return Ok(Self::Local { path: expand_home(text) });
        }
        if Path::new(text).join("SKILL.md").is_file() {
            return Ok(Self::Local { path: PathBuf::from(text) });
        }
        let lower = text.to_ascii_lowercase();
        let without_scheme = lower
            .strip_prefix("https://")
            .or_else(|| lower.strip_prefix("http://"))
            .map(|rest| rest.strip_prefix("www.").unwrap_or(rest));
        let is_link = without_scheme.is_some();
        // ASCII lowercasing keeps byte offsets, so the original (case-preserved) text lines up.
        let prefix_len = lower.len() - without_scheme.map_or(lower.len(), str::len);
        let rest = without_scheme.unwrap_or(lower.as_str()).trim_end_matches('/');
        let original_rest = &text[prefix_len..prefix_len + rest.len()];
        if is_link && lower.ends_with("/skill.md") {
            if let Some(after) = rest.strip_prefix("raw.githubusercontent.com/") {
                let parts: Vec<&str> = original_segments(original_rest, after).collect();
                if parts.len() >= 4 {
                    let subdir = parts[3..parts.len() - 1].join("/");
                    return Ok(Self::GitHub {
                        owner: parts[0].to_owned(),
                        repo: parts[1].to_owned(),
                        subdir,
                        reference: Some(parts[2].to_owned()),
                    });
                }
            }
            if !rest.starts_with("github.com/") {
                return Ok(Self::SkillFile { url: text.to_owned() });
            }
        }
        if let Some(after) = rest.strip_prefix("skills.sh/") {
            let parts: Vec<&str> = original_segments(original_rest, after).collect();
            return match parts.as_slice() {
                [owner, repo, slug] if !owner.is_empty() && !repo.is_empty() && !slug.is_empty() => {
                    Ok(Self::SkillsSh {
                        owner: (*owner).to_owned(),
                        repo: (*repo).to_owned(),
                        slug: (*slug).to_owned(),
                    })
                }
                _ => Err("A skills.sh link looks like skills.sh/<owner>/<repo>/<skill>.".to_owned()),
            };
        }
        if let Some(after) = rest.strip_prefix("github.com/") {
            let parts: Vec<&str> = original_segments(original_rest, after).collect();
            if parts.len() < 2 || parts[0].is_empty() || parts[1].is_empty() {
                return Err("A GitHub link needs an owner and a repository: github.com/<owner>/<repo>.".to_owned());
            }
            let owner = parts[0].to_owned();
            let repo = parts[1].trim_end_matches(".git").to_owned();
            let (reference, subdir) = match parts.get(2) {
                Some(&"tree" | &"blob") if parts.len() >= 4 => {
                    let mut path: Vec<&str> = parts[4..].to_vec();
                    if path.last().is_some_and(|last| last.eq_ignore_ascii_case("SKILL.md")) {
                        path.pop();
                    }
                    (Some(parts[3].to_owned()), path.join("/"))
                }
                _ => (None, String::new()),
            };
            return Ok(Self::GitHub { owner, repo, subdir, reference });
        }
        if is_link || lower.ends_with(".git") {
            if ["gitlab.", "codeberg.", "bitbucket.", "gitea.", "sr.ht"].iter().any(|host| rest.contains(host))
                || lower.ends_with(".git")
            {
                return Err("Only GitHub is supported for now. Other git hosts come with the sandboxed-install work, since they need git to run.".to_owned());
            }
            return Err("re-code can install from GitHub, skills.sh, the Hermes index (official/… or a name), a direct SKILL.md link, or a folder on this computer.".to_owned());
        }
        if let Some(path) = text.strip_prefix("official/") {
            let path = path.trim_matches('/');
            if path.is_empty() {
                return Err(
                    "Name the skill: official/<category>/<skill>, such as official/security/godmode.".to_owned()
                );
            }
            return Ok(Self::Official { path: path.to_owned() });
        }
        if let Some(query) = text.strip_prefix("hermes/") {
            return Ok(Self::HermesLookup { query: query.trim_matches('/').to_owned() });
        }
        if text.contains('/') {
            let (spec, reference) = match text.rsplit_once('@') {
                Some((spec, reference)) if !reference.contains('/') && !reference.is_empty() => {
                    (spec, Some(reference.to_owned()))
                }
                _ => (text, None),
            };
            let parts: Vec<&str> = spec.trim_matches('/').split('/').collect();
            let owner = parts[0];
            let valid_owner = !owner.is_empty() && owner.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
            if !valid_owner || parts.len() < 2 || parts[1].is_empty() {
                return Err(format!("{text} doesn't look like owner/repo[/folder][@ref] or a Hermes identifier."));
            }
            return Ok(Self::GitHub {
                owner: owner.to_owned(),
                repo: parts[1].trim_end_matches(".git").to_owned(),
                subdir: parts[2..].join("/"),
                reference,
            });
        }
        Ok(Self::HermesLookup { query: text.to_owned() })
    }

    /// The label shown next to the skill: where the trust comes from.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Official { .. } => "official",
            Self::HermesLookup { .. } => "hermes",
            Self::SkillsSh { .. } => "skills.sh",
            Self::GitHub { .. } => "github",
            Self::SkillFile { .. } => "file",
            Self::Local { .. } => "local",
        }
    }
}

/// Segments of the original (case-preserved) text that correspond to `after` in the lowercased text.
fn original_segments<'a>(original_rest: &'a str, after_lower: &str) -> impl Iterator<Item = &'a str> {
    let start = original_rest.len() - after_lower.len();
    original_rest[start..].split('/').filter(|segment| !segment.is_empty())
}

fn expand_home(text: &str) -> PathBuf {
    if let Some(rest) = text.strip_prefix('~')
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join(rest.trim_start_matches('/'));
    }
    PathBuf::from(text)
}

/// A skill moved into place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Committed {
    pub name: String,
    pub enabled: bool,
    pub location: String,
    /// Why it's off although enabling was asked, when that happened.
    pub note: Option<String>,
}

/// A hub search's answer.
#[derive(Debug, Clone, Default)]
pub struct SkillSearch {
    pub hits: Vec<SkillSearchHit>,
    /// "The index is 3 days old; today's couldn't be fetched", and the like.
    pub note: Option<String>,
}

#[derive(Debug, Deserialize, Clone, Default)]
struct HermesIndex {
    #[serde(default)]
    skills: Vec<HermesSkill>,
}

#[derive(Debug, Deserialize, Clone, Default)]
struct HermesSkill {
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    source: String,
    #[serde(default)]
    identifier: String,
    #[serde(default)]
    trust_level: String,
    #[serde(default)]
    repo: String,
    #[serde(default)]
    path: String,
}

impl HermesSkill {
    fn label(&self) -> String {
        if !self.source.is_empty() {
            self.source.clone()
        } else if !self.trust_level.is_empty() {
            self.trust_level.clone()
        } else {
            "hermes".to_owned()
        }
    }

    fn hit(&self) -> SkillSearchHit {
        SkillSearchHit {
            tap: format!("hermes/{}", self.identifier),
            name: self.name.clone(),
            description: self.description.clone(),
            label: self.label(),
            repo: self.repo.clone(),
            path: self.path.clone(),
        }
    }
}

/// Fetches, stages, installs and updates skills. Share it with `Arc`.
#[derive(Debug)]
pub struct Hub {
    fetcher: Fetcher,
    staging_root: PathBuf,
    skills_dir: PathBuf,
    cache_dir: PathBuf,
    caps: Caps,
    hermes: tokio::sync::Mutex<Option<Arc<Vec<HermesSkill>>>>,
}

impl Hub {
    /// `cache_dir` holds the staging area and cached indexes; `skills_dir` is where skills end up.
    /// Staged skills older than a day (left by a crash) are removed.
    #[must_use]
    pub fn new(fetcher: Fetcher, cache_dir: PathBuf, skills_dir: PathBuf) -> Self {
        let hub = Self {
            fetcher,
            staging_root: cache_dir.join("hub").join("staging"),
            skills_dir,
            cache_dir: cache_dir.join("hub"),
            caps: Caps::default(),
            hermes: tokio::sync::Mutex::new(None),
        };
        hub.clean_staging();
        hub
    }

    /// Tighter limits, for tests.
    #[must_use]
    pub fn with_caps(mut self, caps: Caps) -> Self {
        self.caps = caps;
        self
    }

    fn clean_staging(&self) {
        let Ok(entries) = std::fs::read_dir(&self.staging_root) else { return };
        for entry in entries.flatten() {
            let old = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|m| m.elapsed().ok())
                .is_some_and(|age| age > STAGING_MAX_AGE);
            if old {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }

    /// Fetches what `text` names into staging and examines it. Nothing is installed.
    pub async fn stage(&self, text: &str) -> InstallOutcome {
        let tap_text = text.trim().to_owned();
        match self.stage_inner(&tap_text, None).await {
            Ok(outcome) => outcome,
            Err(error) => InstallOutcome::Failed { tap: tap_text, error },
        }
    }

    /// Stages a skill from pasted `SKILL.md` text (front matter and body). Nothing else comes with it.
    pub fn stage_text(&self, text: &str) -> InstallOutcome {
        let tap = "pasted".to_owned();
        let staged = (|| -> Result<InstallOutcome, String> {
            if !Self::looks_like_skill_text(text) {
                return Err(
                    "That isn't a SKILL.md: it needs front matter (between --- lines) with a name and a description."
                        .to_owned(),
                );
            }
            let (id, dest) = self.fresh_staging()?;
            write_file(&dest.join("SKILL.md"), text.as_bytes(), false).map_err(|error| {
                let _ = std::fs::remove_dir_all(&id);
                format!("couldn't write the pasted SKILL.md: {error}")
            })?;
            let source = InstalledSource {
                tap: tap.clone(),
                repo: "pasted text".to_owned(),
                path: String::new(),
                sha: String::new(),
                label: "pasted".to_owned(),
                installed_at: String::new(),
            };
            let note = Some("Pasted as text: only the SKILL.md came with it, nothing it refers to.".to_owned());
            self.examine(&id, dest, source, Vec::new(), note, None)
        })();
        match staged {
            Ok(outcome) => outcome,
            Err(error) => InstallOutcome::Failed { tap, error },
        }
    }

    /// Whether pasted text looks like a skill file rather than a link or a name.
    #[must_use]
    pub fn looks_like_skill_text(text: &str) -> bool {
        let trimmed = text.trim_start();
        trimmed.starts_with("---") && trimmed.contains("\nname:") && trimmed.contains("description:")
    }

    /// Stages the newest version of an installed skill, with what would change.
    pub async fn update(&self, name: &str) -> InstallOutcome {
        let tap = format!("update {name}");
        if read_source(&self.skills_dir.join(name)).is_some_and(|source| source.label == "pasted") {
            return InstallOutcome::Failed {
                tap,
                error: format!("{name} was pasted as text, so there's nowhere to fetch an update from."),
            };
        }
        let Some(source) = read_source(&self.skills_dir.join(name)) else {
            let error = if self.skills_dir.join(name).join("SKILL.md").is_file() {
                format!("{name} wasn't installed from a hub, so there's nothing to update it from.")
            } else {
                format!("There's no skill called {name} in {}.", self.skills_dir.display())
            };
            return InstallOutcome::Failed { tap, error };
        };
        if source.tap.is_empty() {
            return InstallOutcome::Failed {
                tap,
                error: format!("{name}'s source file doesn't say where it came from."),
            };
        }
        match self.stage_inner(&source.tap, Some(&source)).await {
            Ok(outcome) => outcome,
            Err(error) => InstallOutcome::Failed { tap: source.tap, error },
        }
    }

    async fn stage_inner(&self, tap_text: &str, updating: Option<&InstalledSource>) -> Result<InstallOutcome, String> {
        let tap = Tap::parse(tap_text)?;
        match tap {
            Tap::Official { path } => {
                let subdir = format!("{HERMES_SKILLS_DIR}/{path}");
                self.stage_github(HERMES_OWNER, HERMES_REPO, &subdir, None, "official", tap_text, updating, None).await
            }
            Tap::HermesLookup { query } => {
                let (skill, note) = match self.hermes_lookup(&query).await? {
                    Lookup::One(skill, note) => (skill, note),
                    Lookup::Many(candidates, note) => {
                        return Ok(InstallOutcome::Choose { tap: tap_text.to_owned(), candidates, note });
                    }
                };
                let (owner, repo) = split_repo(&skill.repo).ok_or_else(|| {
                    format!("The Hermes index gives {} a repository re-code can't read: {}", skill.name, skill.repo)
                })?;
                let label = skill.label();
                self.stage_github(&owner, &repo, &skill.path, None, &label, tap_text, updating, note).await
            }
            Tap::SkillsSh { owner, repo, slug } => {
                let source_id = format!("{owner}/{repo}");
                match self.fetcher.skills_sh(&source_id, &slug).await? {
                    Some(files) => {
                        let (id, dest) = self.fresh_staging()?;
                        let mut bytes = 0u64;
                        let mut written = Vec::new();
                        for (path, contents) in &files {
                            check_relative(path).inspect_err(|_| {
                                let _ = std::fs::remove_dir_all(&id);
                            })?;
                            if written.len() >= self.caps.max_files
                                || bytes + contents.len() as u64 > self.caps.max_total_bytes
                            {
                                let _ = std::fs::remove_dir_all(&id);
                                return Err("That skill is larger than the install limits.".to_owned());
                            }
                            write_file(&dest.join(path), contents.as_bytes(), false).map_err(|error| {
                                let _ = std::fs::remove_dir_all(&id);
                                format!("{path}: {error}")
                            })?;
                            bytes += contents.len() as u64;
                            written.push(path.clone());
                        }
                        let source = InstalledSource {
                            tap: tap_text.to_owned(),
                            repo: source_id,
                            path: slug,
                            sha: String::new(),
                            label: "skills.sh".to_owned(),
                            installed_at: String::new(),
                        };
                        self.examine(&id, dest, source, Vec::new(), None, updating)
                    }
                    None => {
                        let note = Some(
                            "skills.sh's API needs a login, so the files came from the GitHub repository it points at."
                                .to_owned(),
                        );
                        let sha = self.fetcher.resolve_ref(&owner, &repo, None).await?;
                        let tree = self.fetcher.tree(&owner, &repo, &sha, "").await?;
                        let dirs = tree.skill_dirs();
                        let wanted = [format!("skills/{slug}"), format!(".claude/skills/{slug}"), slug.clone()];
                        let found = wanted
                            .iter()
                            .find(|candidate| dirs.contains(candidate))
                            .cloned()
                            .or_else(|| dirs.iter().find(|dir| dir.rsplit('/').next() == Some(slug.as_str())).cloned())
                            .ok_or_else(|| format!("{owner}/{repo} has no folder called {slug} with a SKILL.md."))?;
                        self.stage_github(&owner, &repo, &found, Some(&sha), "skills.sh", tap_text, updating, note)
                            .await
                    }
                }
            }
            Tap::GitHub { owner, repo, subdir, reference } => {
                self.stage_github(&owner, &repo, &subdir, reference.as_deref(), "github", tap_text, updating, None)
                    .await
            }
            Tap::SkillFile { url } => {
                let text = self.fetcher.text(&url, self.caps.max_file_bytes).await?;
                let (id, dest) = self.fresh_staging()?;
                write_file(&dest.join("SKILL.md"), text.as_bytes(), false).map_err(|error| {
                    let _ = std::fs::remove_dir_all(&id);
                    error.to_string()
                })?;
                let host = url.split("://").nth(1).and_then(|r| r.split('/').next()).unwrap_or("").to_owned();
                let source = InstalledSource {
                    tap: tap_text.to_owned(),
                    repo: host,
                    path: url.clone(),
                    sha: String::new(),
                    label: "file".to_owned(),
                    installed_at: String::new(),
                };
                let note =
                    Some("A single file: whatever it refers to (scripts, references) isn't included.".to_owned());
                self.examine(&id, dest, source, Vec::new(), note, updating)
            }
            Tap::Local { path } => {
                if !path.join("SKILL.md").is_file() {
                    return Err(format!("{} has no SKILL.md.", path.display()));
                }
                let (id, dest) = self.fresh_staging()?;
                let skipped = copy_tree(&path, &dest, self.caps).inspect_err(|_| {
                    let _ = std::fs::remove_dir_all(&id);
                })?;
                let source = InstalledSource {
                    tap: tap_text.to_owned(),
                    repo: path.display().to_string(),
                    path: String::new(),
                    sha: String::new(),
                    label: "local".to_owned(),
                    installed_at: String::new(),
                };
                self.examine(&id, dest, source, skipped, None, updating)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn stage_github(
        &self,
        owner: &str,
        repo: &str,
        subdir: &str,
        reference: Option<&str>,
        label: &str,
        tap_text: &str,
        updating: Option<&InstalledSource>,
        note: Option<String>,
    ) -> Result<InstallOutcome, String> {
        let subdir = subdir.trim_matches('/').to_owned();
        let sha = self.fetcher.resolve_ref(owner, repo, reference).await?;
        if let Some(current) = updating
            && current.sha == sha
        {
            return Ok(InstallOutcome::UpToDate { name: skill_folder_name(&self.skills_dir, current), sha });
        }
        let tree = self.fetcher.tree(owner, repo, &sha, &subdir).await?;
        let mut dirs = tree.skill_dirs();
        let place = if subdir.is_empty() { format!("{owner}/{repo}") } else { format!("{owner}/{repo}/{subdir}") };
        let root_is_skill = dirs.first().map(String::as_str) == Some("");
        // A root SKILL.md over a whole repository (a monorepo that also ships its skills in folders, or
        // one with evals, packaging and tooling around it) is offered folder by folder instead.
        let root_too_big = root_is_skill && {
            let files = tree.entries.iter().filter(|entry| entry.kind == EntryKind::File);
            let (count, bytes) = files.fold((0usize, 0u64), |(n, b), entry| (n + 1, b + entry.size));
            count > self.caps.max_files || bytes > self.caps.max_total_bytes
        };
        let mut note = note;
        if root_is_skill && (dirs.len() > 1 || root_too_big) {
            if root_too_big {
                dirs.remove(0);
                if dirs.is_empty() {
                    return Err(format!(
                        "{place} is one skill of {} files; the limit is {}. Install a folder of it instead.",
                        tree.entries.iter().filter(|entry| entry.kind == EntryKind::File).count(),
                        self.caps.max_files
                    ));
                }
                note = Some(format!(
                    "The whole of {place} is too large to take as one skill ({} files; the limit is {}), so its skill folders are offered instead.",
                    tree.entries.iter().filter(|entry| entry.kind == EntryKind::File).count(),
                    self.caps.max_files
                ));
            } else {
                note = Some(format!("{place} is itself a skill (the first row) and also holds skills in folders."));
            }
        }
        if !root_is_skill || dirs.len() > 1 || root_too_big {
            if dirs.is_empty() {
                let more = if tree.truncated { " (GitHub listed only part of it)" } else { "" };
                return Err(format!("{place} has no SKILL.md anywhere{more}."));
            }
            let candidates = self.candidates(owner, repo, &sha, &subdir, &dirs).await;
            if tree.truncated {
                note =
                    Some("GitHub listed only part of this repository; there may be more skills than shown.".to_owned());
            }
            return Ok(InstallOutcome::Choose { tap: tap_text.to_owned(), candidates, note });
        }
        let (id, dest) = self.fresh_staging()?;
        let folder = Folder { owner, repo, sha: &sha, subdir: &subdir };
        let downloaded = match self.fetcher.download_dir(folder, &tree, &dest, self.caps).await {
            Ok(downloaded) => downloaded,
            Err(error) => {
                let _ = std::fs::remove_dir_all(&id);
                return Err(error);
            }
        };
        let source = InstalledSource {
            tap: strip_pin(tap_text),
            repo: format!("{owner}/{repo}"),
            path: subdir,
            sha,
            label: label.to_owned(),
            installed_at: String::new(),
        };
        self.examine(&id, dest, source, downloaded.skipped, note, updating)
    }

    /// Skill folders to choose from, with each one's description when it can be read.
    async fn candidates(
        &self,
        owner: &str,
        repo: &str,
        sha: &str,
        subdir: &str,
        dirs: &[String],
    ) -> Vec<SkillCandidate> {
        let mut tasks = tokio::task::JoinSet::new();
        for (index, dir) in dirs.iter().take(MAX_CANDIDATES).enumerate() {
            let full = match (subdir.is_empty(), dir.is_empty()) {
                (true, true) => String::new(),
                (true, false) => dir.clone(),
                (false, true) => subdir.to_owned(),
                (false, false) => format!("{subdir}/{dir}"),
            };
            let fetcher = self.fetcher.clone();
            let (owner, repo, sha) = (owner.to_owned(), repo.to_owned(), sha.to_owned());
            tasks.spawn(async move {
                let file = if full.is_empty() { "SKILL.md".to_owned() } else { format!("{full}/SKILL.md") };
                let text = fetcher.raw(&owner, &repo, &sha, &file, 256 * 1024).await.ok();
                let front = text
                    .as_deref()
                    .and_then(|bytes| std::str::from_utf8(bytes).ok())
                    .map(|text| frontmatter::split(text).0.map(frontmatter::parse).unwrap_or_default())
                    .unwrap_or_default();
                let fallback =
                    if full.is_empty() { repo.clone() } else { full.rsplit('/').next().unwrap_or(&full).to_owned() };
                let name = front.name.unwrap_or(fallback);
                let tap = if full.is_empty() {
                    format!("{owner}/{repo}@{sha}")
                } else {
                    format!("{owner}/{repo}/{full}@{sha}")
                };
                (
                    index,
                    SkillCandidate {
                        tap,
                        path: if full.is_empty() { "(the whole repository)".to_owned() } else { full },
                        name,
                        description: front.description.unwrap_or_default(),
                    },
                )
            });
        }
        let mut found: Vec<(usize, SkillCandidate)> = Vec::new();
        while let Some(result) = tasks.join_next().await {
            if let Ok(item) = result {
                found.push(item);
            }
        }
        found.sort_by_key(|(index, _)| *index);
        found.into_iter().map(|(_, candidate)| candidate).collect()
    }

    /// A new `staging/<id>/skill` folder. Returns the id folder (to remove on failure) and the skill folder.
    fn fresh_staging(&self) -> Result<(PathBuf, PathBuf), String> {
        let nanos = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let id = self.staging_root.join(format!("{nanos:x}-{:x}", std::process::id()));
        let dest = id.join("skill");
        std::fs::create_dir_all(&dest).map_err(|error| format!("couldn't create {}: {error}", dest.display()))?;
        Ok((id, dest))
    }

    /// Reads and scans a staged folder, renames it to the skill's name, and builds the preview.
    fn examine(
        &self,
        id: &Path,
        dest: PathBuf,
        mut source: InstalledSource,
        skipped: Vec<(String, String)>,
        note: Option<String>,
        updating: Option<&InstalledSource>,
    ) -> Result<InstallOutcome, String> {
        let fail = |error: String| {
            let _ = std::fs::remove_dir_all(id);
            error
        };
        let first = skills::read_skill(&dest, SkillSource::User, &BTreeSet::new(), false)
            .ok_or_else(|| fail("The files have no SKILL.md at the skill's root.".to_owned()))?;
        validate_name("skill", &first.name)
            .map_err(|error| fail(format!("Its name can't be a folder name: {error}")))?;
        if let Some(current) = updating
            && skill_folder_name(&self.skills_dir, current) != first.name
        {
            return Err(fail(format!(
                "The update calls itself {} but the installed skill is {}; install it as a new skill instead.",
                first.name,
                skill_folder_name(&self.skills_dir, current)
            )));
        }
        let staged = id.join(&first.name);
        std::fs::rename(&dest, &staged).map_err(|error| fail(format!("couldn't rename the staged folder: {error}")))?;
        let skill = skills::read_skill(&staged, SkillSource::User, &BTreeSet::new(), false)
            .ok_or_else(|| fail("The staged skill vanished.".to_owned()))?;

        let target = self.skills_dir.join(&skill.name);
        let replaces = if target.exists() {
            match read_source(&target) {
                Some(current) => Some(Replacing { sha: current.sha, changed: changed_files(&target, &staged) }),
                None => {
                    return Err(fail(format!(
                        "You already have a skill called {}, and it wasn't installed from a hub. Delete it first (d on the Skills tab) to replace it.",
                        skill.name
                    )));
                }
            }
        } else {
            None
        };

        let files = list_files(&staged);
        let bytes = files.iter().filter_map(|file| std::fs::metadata(staged.join(file)).ok()).map(|m| m.len()).sum();
        let declares = declares(&staged, &files, &skill.allowed_tools);
        let mut findings = skill.scan.findings().to_vec();
        for (path, why) in skipped {
            findings.push(Finding {
                file: path,
                line: 0,
                reason: format!("{why}, so it wasn't taken; the skill may expect it"),
                excerpt: String::new(),
                severity: Severity::Caution,
            });
        }
        let scan = scan::verdict(findings);
        let skill_text = std::fs::read_to_string(staged.join("SKILL.md"))
            .map(|text| text.chars().take(SKILL_TEXT_LIMIT).collect())
            .unwrap_or_default();
        source.installed_at = utc_stamp(SystemTime::now());
        write_source(&staged, &source).map_err(fail)?;

        Ok(InstallOutcome::Preview(Box::new(InstallPreview {
            staging: staged.display().to_string(),
            name: skill.name.clone(),
            description: skill.description.clone(),
            source,
            files,
            bytes,
            estimated_tokens: skill.estimated_tokens,
            declares,
            scan,
            location: target.display().to_string(),
            skill_text,
            problem: skill.problem.clone(),
            note,
            replaces,
        })))
    }

    /// Moves a staged skill into the skills folder. A dangerous verdict keeps it off whatever was asked.
    pub fn commit(&self, staging: &str, enable: bool) -> Result<Committed, String> {
        let staged = self.staged_path(staging)?;
        let name = staged.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_owned();
        validate_name("skill", &name)?;
        let scan = scan::scan_dir(&staged);
        let (enabled, note) = if enable && scan.blocks() {
            (false, Some("The scan found something dangerous, so it stays off.".to_owned()))
        } else {
            (enable, None)
        };
        let target = self.skills_dir.join(&name);
        if target.exists() {
            if read_source(&target).is_none() {
                return Err(format!("{name} exists and wasn't installed from a hub; delete it first."));
            }
            std::fs::remove_dir_all(&target).map_err(|error| format!("couldn't remove the old {name}: {error}"))?;
        }
        std::fs::create_dir_all(&self.skills_dir)
            .map_err(|error| format!("couldn't create {}: {error}", self.skills_dir.display()))?;
        if std::fs::rename(&staged, &target).is_err() {
            // The cache and config folders may be on different filesystems.
            copy_tree(
                &staged,
                &target,
                Caps { max_total_bytes: u64::MAX, max_files: usize::MAX, max_file_bytes: u64::MAX },
            )
            .map_err(|error| format!("couldn't move the skill into place: {error}"))?;
            let _ = std::fs::remove_dir_all(&staged);
        }
        if let Some(parent) = staged.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
        Ok(Committed { name, enabled, location: target.display().to_string(), note })
    }

    /// Removes a staged skill.
    pub fn discard(&self, staging: &str) -> Result<(), String> {
        let staged = self.staged_path(staging)?;
        let parent = staged.parent().unwrap_or(&staged);
        std::fs::remove_dir_all(parent).map_err(|error| format!("couldn't remove the staged skill: {error}"))
    }

    /// Only paths inside the staging area are ever moved or removed.
    fn staged_path(&self, staging: &str) -> Result<PathBuf, String> {
        let path = Path::new(staging);
        let canonical = path.canonicalize().map_err(|_| "That staged skill is gone.".to_owned())?;
        let root = self.staging_root.canonicalize().map_err(|_| "That staged skill is gone.".to_owned())?;
        if !canonical.starts_with(&root) || canonical == root || canonical.parent() == Some(root.as_path()) {
            return Err("That isn't a staged skill.".to_owned());
        }
        if !canonical.join("SKILL.md").is_file() {
            return Err("The staged folder has no SKILL.md.".to_owned());
        }
        Ok(canonical)
    }

    /// Searches the Hermes index by name, identifier and description.
    pub async fn search(&self, query: &str, limit: usize) -> Result<SkillSearch, String> {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return Err("Type something to search for.".to_owned());
        }
        let (index, note) = self.hermes_index().await?;
        let words: Vec<&str> = query.split_whitespace().collect();
        let mut scored: Vec<(u8, &HermesSkill)> = index
            .iter()
            .filter_map(|skill| {
                let name = skill.name.to_lowercase();
                let identifier = skill.identifier.to_lowercase();
                let description = skill.description.to_lowercase();
                let score = if name == query {
                    0
                } else if name.starts_with(&query) {
                    1
                } else if name.contains(&query) || identifier.contains(&query) {
                    2
                } else if words.iter().all(|word| description.contains(word) || name.contains(word)) {
                    3
                } else {
                    return None;
                };
                Some((score, skill))
            })
            .collect();
        scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.name.cmp(&b.1.name)));
        let hits = scored.into_iter().take(limit.max(1)).map(|(_, skill)| skill.hit()).collect();
        Ok(SkillSearch { hits, note })
    }

    async fn hermes_lookup(&self, query: &str) -> Result<Lookup, String> {
        let query = query.trim();
        if query.is_empty() {
            return Err("Name the skill to look up.".to_owned());
        }
        let (index, note) = self.hermes_index().await?;
        if let Some(skill) = index.iter().find(|skill| skill.identifier.eq_ignore_ascii_case(query)) {
            return Ok(Lookup::One(skill.clone(), note));
        }
        let by_name: Vec<&HermesSkill> = index.iter().filter(|skill| skill.name.eq_ignore_ascii_case(query)).collect();
        match by_name.as_slice() {
            [one] => Ok(Lookup::One((*one).clone(), note)),
            [] => {
                let lower = query.to_lowercase();
                let similar: Vec<SkillCandidate> = index
                    .iter()
                    .filter(|skill| skill.name.to_lowercase().contains(&lower))
                    .take(MAX_CANDIDATES)
                    .map(candidate_of)
                    .collect();
                if similar.is_empty() {
                    Err(format!("Nothing in the Hermes index is called {query}. Try /skill search <words>."))
                } else {
                    let note = Some(format!("Nothing is called exactly {query}; these names contain it."));
                    Ok(Lookup::Many(similar, note))
                }
            }
            many => {
                let note = Some(format!("{} skills are called {query}; pick one by its identifier.", many.len()));
                Ok(Lookup::Many(many.iter().map(|skill| candidate_of(skill)).collect(), note))
            }
        }
    }

    /// The Hermes index, parsed once per hub and refreshed daily on disk.
    async fn hermes_index(&self) -> Result<(Arc<Vec<HermesSkill>>, Option<String>), String> {
        let mut slot = self.hermes.lock().await;
        if let Some(index) = slot.as_ref() {
            return Ok((index.clone(), None));
        }
        let cache_file = self.cache_dir.join("hermes-skills-index.json");
        let cached = self
            .fetcher
            .cached_json(
                &self.fetcher.bases().hermes_index.clone(),
                &cache_file,
                HERMES_INDEX_MAX_AGE,
                HERMES_INDEX_LIMIT,
            )
            .await
            .map_err(|error| format!("The Hermes skills index couldn't be fetched: {error}"))?;
        let note = cached.stale_because.as_ref().map(|why| {
            format!("Using a Hermes index from {} ago; today's couldn't be fetched ({why}).", age_text(cached.age))
        });
        let text = cached.text;
        let index: HermesIndex = tokio::task::spawn_blocking(move || serde_json::from_str(&text))
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| format!("The Hermes skills index isn't in the shape re-code expects: {error}"))?;
        let skills = Arc::new(index.skills);
        *slot = Some(skills.clone());
        Ok((skills, note))
    }
}

enum Lookup {
    One(HermesSkill, Option<String>),
    Many(Vec<SkillCandidate>, Option<String>),
}

fn candidate_of(skill: &HermesSkill) -> SkillCandidate {
    SkillCandidate {
        tap: format!("hermes/{}", skill.identifier),
        path: format!("{} · {}", skill.repo, skill.path),
        name: skill.name.clone(),
        description: skill.description.clone(),
    }
}

/// `owner/repo` from the forms the Hermes index uses (`owner/repo`, a github.com URL).
fn split_repo(text: &str) -> Option<(String, String)> {
    let rest = text.trim().trim_end_matches('/');
    let rest = rest
        .strip_prefix("https://github.com/")
        .or_else(|| rest.strip_prefix("http://github.com/"))
        .or_else(|| rest.strip_prefix("github.com/"))
        .unwrap_or(rest);
    let mut parts = rest.split('/');
    let owner = parts.next().filter(|s| !s.is_empty())?;
    let repo = parts.next().filter(|s| !s.is_empty())?.trim_end_matches(".git");
    Some((owner.to_owned(), repo.to_owned()))
}

/// `owner/repo/path@<sha>` → `owner/repo/path`, so an update follows the branch, not the pinned commit.
fn strip_pin(tap: &str) -> String {
    match tap.rsplit_once('@') {
        Some((spec, pin)) if pin.len() == 40 && pin.chars().all(|c| c.is_ascii_hexdigit()) => spec.to_owned(),
        _ => tap.to_owned(),
    }
}

fn skill_folder_name(skills_dir: &Path, source: &InstalledSource) -> String {
    // The installed folder is found by its source file; the name is the folder.
    let Ok(entries) = std::fs::read_dir(skills_dir) else { return String::new() };
    for entry in entries.flatten() {
        if read_source(&entry.path()).is_some_and(|s| s.tap == source.tap && s.sha == source.sha) {
            return entry.file_name().to_string_lossy().into_owned();
        }
    }
    String::new()
}

fn age_text(age: Duration) -> String {
    let hours = age.as_secs() / 3600;
    if hours < 1 {
        "under an hour".to_owned()
    } else if hours < 48 {
        format!("{hours} hours")
    } else {
        format!("{} days", hours / 24)
    }
}

/// Reads `.re-code-source.toml` from a skill folder.
#[must_use]
pub fn read_source(directory: &Path) -> Option<InstalledSource> {
    let text = std::fs::read_to_string(directory.join(SOURCE_FILE)).ok()?;
    toml::from_str(&text).ok()
}

fn write_source(directory: &Path, source: &InstalledSource) -> Result<(), String> {
    let body = toml::to_string(source).map_err(|error| error.to_string())?;
    let text = format!("# Written by re-code when this skill was installed. It's what `update` reads.\n{body}");
    std::fs::write(directory.join(SOURCE_FILE), text).map_err(|error| error.to_string())
}

/// Every regular file under a folder, relative, sorted, without the source file.
fn list_files(root: &Path) -> Vec<String> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(metadata) = std::fs::symlink_metadata(&path) else { continue };
            if metadata.is_dir() {
                stack.push(path);
            } else if metadata.is_file() {
                let relative = path.strip_prefix(root).unwrap_or(&path).display().to_string();
                if relative != SOURCE_FILE {
                    files.push(relative);
                }
            }
        }
    }
    files.sort();
    files
}

/// `+ path`, `- path` and `~ path` between the installed and staged folders.
fn changed_files(installed: &Path, staged: &Path) -> Vec<String> {
    let old: BTreeSet<String> = list_files(installed).into_iter().collect();
    let new: BTreeSet<String> = list_files(staged).into_iter().collect();
    let mut changed = Vec::new();
    for path in old.union(&new) {
        match (old.contains(path), new.contains(path)) {
            (true, false) => changed.push(format!("- {path}")),
            (false, true) => changed.push(format!("+ {path}")),
            _ => {
                let same = std::fs::read(installed.join(path)).ok() == std::fs::read(staged.join(path)).ok();
                if !same {
                    changed.push(format!("~ {path}"));
                }
            }
        }
    }
    changed
}

/// Copies regular files and folders, never following links. Returns what was skipped and why.
fn copy_tree(from: &Path, to: &Path, caps: Caps) -> Result<Vec<(String, String)>, String> {
    let mut skipped = Vec::new();
    let mut count = 0usize;
    let mut total = 0u64;
    let mut stack = vec![PathBuf::new()];
    while let Some(relative) = stack.pop() {
        let dir = from.join(&relative);
        let entries = std::fs::read_dir(&dir).map_err(|error| format!("can't read {}: {error}", dir.display()))?;
        for entry in entries.flatten() {
            let path = entry.path();
            let rel = relative.join(entry.file_name());
            let rel_text = rel.display().to_string();
            let Ok(metadata) = std::fs::symlink_metadata(&path) else { continue };
            if metadata.file_type().is_symlink() {
                skipped.push((rel_text, "is a symbolic link".to_owned()));
            } else if metadata.is_dir() {
                stack.push(rel);
            } else if metadata.is_file() {
                count += 1;
                total += metadata.len();
                if count > caps.max_files {
                    return Err(format!("That folder has more than {} files.", caps.max_files));
                }
                if metadata.len() > caps.max_file_bytes {
                    return Err(format!(
                        "{rel_text} is {}; the limit per file is {}.",
                        human(metadata.len()),
                        human(caps.max_file_bytes)
                    ));
                }
                if total > caps.max_total_bytes {
                    return Err(format!("That folder is larger than {}.", human(caps.max_total_bytes)));
                }
                let bytes = std::fs::read(&path).map_err(|error| format!("{rel_text}: {error}"))?;
                #[cfg(unix)]
                let executable = {
                    use std::os::unix::fs::PermissionsExt;
                    metadata.permissions().mode() & 0o111 != 0
                };
                #[cfg(not(unix))]
                let executable = false;
                write_file(&to.join(&rel), &bytes, executable).map_err(|error| format!("{rel_text}: {error}"))?;
            }
        }
    }
    Ok(skipped)
}

/// What the files declare: tools asked for, scripts shipped, environment variables referred to.
fn declares(root: &Path, files: &[String], allowed_tools: &[String]) -> Declares {
    const SCRIPT_EXTENSIONS: &[&str] = &["sh", "bash", "zsh", "py", "js", "mjs", "ts", "rb", "pl", "ps1"];
    const COMMON: &[&str] = &[
        "HOME", "PATH", "PWD", "OLDPWD", "USER", "SHELL", "EDITOR", "TMPDIR", "LANG", "TERM", "RANDOM", "UID",
        "HOSTNAME", "DISPLAY", "LC_ALL", "PAGER", "VISUAL",
    ];
    let scripts: Vec<String> = files
        .iter()
        .filter(|file| {
            file.starts_with("scripts/")
                || Path::new(file)
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| SCRIPT_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        })
        .cloned()
        .collect();
    let mut env_vars = BTreeSet::new();
    let mut texts = vec!["SKILL.md".to_owned()];
    texts.extend(scripts.iter().cloned());
    for file in texts {
        let Ok(text) = std::fs::read_to_string(root.join(&file)) else { continue };
        for name in env_names(&text) {
            if !COMMON.contains(&name.as_str()) && !name.starts_with("XDG_") {
                env_vars.insert(name);
            }
        }
    }
    Declares { allowed_tools: allowed_tools.to_vec(), scripts, env_vars: env_vars.into_iter().take(12).collect() }
}

/// `$NAME`, `${NAME}`, `getenv("NAME")`, `environ["NAME"]`, `environ.get("NAME")`, `process.env.NAME`.
fn env_names(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    // Markers are ASCII, so a byte offset just past one is always a character boundary.
    let read_name = |start: usize| -> Option<String> {
        let rest = text.get(start..)?;
        let end =
            rest.find(|c: char| !(c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')).unwrap_or(rest.len());
        let name = &rest[..end];
        (name.len() >= 2 && name.chars().next().is_some_and(|c| c.is_ascii_uppercase())).then(|| name.to_owned())
    };
    for (index, c) in text.char_indices() {
        let found = if c == '$' {
            let after = if text[index + 1..].starts_with('{') { index + 2 } else { index + 1 };
            read_name(after)
        } else if c.is_ascii() {
            ["getenv(\"", "getenv('", "environ[\"", "environ['", "environ.get(\"", "environ.get('", "process.env."]
                .iter()
                .find(|marker| text[index..].starts_with(*marker))
                .and_then(|marker| read_name(index + marker.len()))
        } else {
            None
        };
        if let Some(name) = found {
            names.push(name);
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taps_are_recognised() {
        assert_eq!(Tap::parse("official/security/godmode").unwrap(), Tap::Official { path: "security/godmode".into() });
        assert_eq!(Tap::parse("godmode").unwrap(), Tap::HermesLookup { query: "godmode".into() });
        assert_eq!(
            Tap::parse("hermes/official/security/godmode").unwrap(),
            Tap::HermesLookup { query: "official/security/godmode".into() }
        );
        assert_eq!(
            Tap::parse("anthropics/skills/skills/pdf").unwrap(),
            Tap::GitHub {
                owner: "anthropics".into(),
                repo: "skills".into(),
                subdir: "skills/pdf".into(),
                reference: None
            }
        );
        assert_eq!(
            Tap::parse("anthropics/skills@v1.2").unwrap(),
            Tap::GitHub {
                owner: "anthropics".into(),
                repo: "skills".into(),
                subdir: String::new(),
                reference: Some("v1.2".into())
            }
        );
        assert_eq!(
            Tap::parse("https://github.com/anthropics/skills/tree/main/skills/pdf").unwrap(),
            Tap::GitHub {
                owner: "anthropics".into(),
                repo: "skills".into(),
                subdir: "skills/pdf".into(),
                reference: Some("main".into())
            }
        );
        assert_eq!(
            Tap::parse("https://github.com/anthropics/skills/blob/main/skills/pdf/SKILL.md").unwrap(),
            Tap::GitHub {
                owner: "anthropics".into(),
                repo: "skills".into(),
                subdir: "skills/pdf".into(),
                reference: Some("main".into())
            }
        );
        assert_eq!(
            Tap::parse("https://github.com/Anthropics/Skills.git").unwrap(),
            Tap::GitHub { owner: "Anthropics".into(), repo: "Skills".into(), subdir: String::new(), reference: None }
        );
        assert_eq!(
            Tap::parse("https://raw.githubusercontent.com/o/r/main/skills/pdf/SKILL.md").unwrap(),
            Tap::GitHub {
                owner: "o".into(),
                repo: "r".into(),
                subdir: "skills/pdf".into(),
                reference: Some("main".into())
            }
        );
        assert_eq!(
            Tap::parse("https://skills.sh/anthropics/skills/pdf").unwrap(),
            Tap::SkillsSh { owner: "anthropics".into(), repo: "skills".into(), slug: "pdf".into() }
        );
        assert_eq!(
            Tap::parse("skills.sh/vercel-labs/agent-skills/web-design-guidelines/").unwrap(),
            Tap::SkillsSh {
                owner: "vercel-labs".into(),
                repo: "agent-skills".into(),
                slug: "web-design-guidelines".into()
            }
        );
        assert_eq!(
            Tap::parse("https://example.com/things/SKILL.md").unwrap(),
            Tap::SkillFile { url: "https://example.com/things/SKILL.md".into() }
        );
        assert_eq!(Tap::parse("~/skills/mine").unwrap(), Tap::Local { path: expand_home("~/skills/mine") });
        assert!(Tap::parse("").is_err());
        assert!(Tap::parse("https://gitlab.com/a/b").unwrap_err().contains("Only GitHub"));
        assert!(Tap::parse("git@github.com:a/b.git").unwrap_err().contains("Only GitHub"));
        assert!(Tap::parse("https://example.com/page").unwrap_err().contains("install from"));
        assert!(Tap::parse("io.github.x/y").unwrap_err().contains("doesn't look like"));
        assert!(Tap::parse("skills.sh/only-two/parts").is_err());
        assert_eq!(Tap::parse("official/x").unwrap().label(), "official");
    }

    #[test]
    fn pins_are_stripped_and_repos_split() {
        assert_eq!(strip_pin("a/b/c@2c5958ecac7cee2b583967a2c4981d0f4a75c579"), "a/b/c");
        assert_eq!(strip_pin("a/b@v1"), "a/b@v1");
        assert_eq!(
            split_repo("https://github.com/NousResearch/hermes-agent.git"),
            Some(("NousResearch".into(), "hermes-agent".into()))
        );
        assert_eq!(split_repo("owner/repo"), Some(("owner".into(), "repo".into())));
        assert_eq!(split_repo("nothing"), None);
    }

    #[test]
    fn env_names_are_found_in_prose_and_code() {
        let names = env_names("Set $OPENAI_API_KEY and ${HOME}/x; os.environ[\"TOKEN\"]; process.env.PORT; $1 $a");
        assert_eq!(names, ["OPENAI_API_KEY", "HOME", "TOKEN", "PORT"]);
        assert_eq!(env_names("x₀ — $A_B$"), ["A_B"], "multi-byte text before a reference");
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("SKILL.md"), "needs $API_KEY and $HOME").unwrap();
        std::fs::create_dir(dir.path().join("scripts")).unwrap();
        std::fs::write(dir.path().join("scripts/run.py"), "os.getenv('OTHER')").unwrap();
        let files = list_files(dir.path());
        let declares = declares(dir.path(), &files, &["Bash".to_owned()]);
        assert_eq!(declares.scripts, ["scripts/run.py"]);
        assert_eq!(declares.env_vars, ["API_KEY", "OTHER"]);
        assert!(declares.summary().contains("asks for tools: Bash"));
        assert_eq!(Declares::default().summary(), "declares no tools, hooks, middleware, or env vars");
    }

    #[test]
    fn source_files_round_trip_and_changes_are_listed() {
        let dir = tempfile::tempdir().unwrap();
        let installed = dir.path().join("installed");
        let staged = dir.path().join("staged");
        std::fs::create_dir_all(&installed).unwrap();
        std::fs::create_dir_all(staged.join("scripts")).unwrap();
        std::fs::write(installed.join("SKILL.md"), "old").unwrap();
        std::fs::write(installed.join("gone.md"), "x").unwrap();
        std::fs::write(staged.join("SKILL.md"), "new").unwrap();
        std::fs::write(staged.join("scripts/a.py"), "x").unwrap();
        let source = InstalledSource {
            tap: "official/x/y".into(),
            repo: "NousResearch/hermes-agent".into(),
            path: "optional-skills/x/y".into(),
            sha: "abc".into(),
            label: "official".into(),
            installed_at: "2026-11-07T00:00:00Z".into(),
        };
        write_source(&installed, &source).unwrap();
        assert_eq!(read_source(&installed), Some(source));
        assert_eq!(changed_files(&installed, &staged), ["~ SKILL.md", "- gone.md", "+ scripts/a.py"]);
        assert_eq!(list_files(&installed), ["SKILL.md", "gone.md"], "the source file isn't listed");
    }
}
