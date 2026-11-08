//! Fetching skill files without git and without running anything. GitHub is read through its REST API
//! (one call to pin a commit, one to list the folder) and the raw file endpoint; skills.sh through its
//! API when it answers; the Hermes index as a cached JSON file. Every base address can be overridden,
//! so tests run against a local mock.
//!
//! A tarball was considered and rejected: GitHub serves whole repositories only, and the Hermes
//! repository's tarball was 90 MB for a seven-file skill when measured on 2026-11-07.

use serde::Deserialize;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::Semaphore;

const TIMEOUT: Duration = Duration::from_secs(30);
/// GitHub refuses requests without one.
const USER_AGENT: &str = "re-code (https://github.com/ZeroDay-Labz/Re-Code)";
/// How many raw files to fetch at once.
const PARALLEL: usize = 6;

/// Size and count limits for one skill. Hitting one is a plain error, not a partial install.
#[derive(Debug, Clone, Copy)]
pub struct Caps {
    pub max_total_bytes: u64,
    pub max_files: usize,
    pub max_file_bytes: u64,
}

impl Default for Caps {
    fn default() -> Self {
        Self { max_total_bytes: 5 * 1024 * 1024, max_files: 400, max_file_bytes: 2 * 1024 * 1024 }
    }
}

/// Where each service is reached.
#[derive(Debug, Clone)]
pub struct Bases {
    pub github_api: String,
    pub github_raw: String,
    pub skills_sh: String,
    pub hermes_index: String,
    pub hermes_catalog: String,
}

impl Bases {
    /// The real services.
    #[must_use]
    pub fn live() -> Self {
        Self {
            github_api: "https://api.github.com".to_owned(),
            github_raw: "https://raw.githubusercontent.com".to_owned(),
            skills_sh: "https://skills.sh".to_owned(),
            hermes_index: "https://nousresearch.github.io/hermes-agent/docs/api/skills-index.json".to_owned(),
            hermes_catalog: "https://nousresearch.github.io/hermes-agent/docs/api/plugin-catalog.json".to_owned(),
        }
    }

    /// The real services unless `RE_CODE_HUB_GITHUB_API`, `RE_CODE_HUB_GITHUB_RAW`, `RE_CODE_HUB_SKILLS_SH`,
    /// `RE_CODE_HUB_HERMES_INDEX` or `RE_CODE_HUB_HERMES_CATALOG` say otherwise (for tests).
    #[must_use]
    pub fn from_env() -> Self {
        let mut bases = Self::live();
        let read = |name: &str, slot: &mut String| {
            if let Some(value) = std::env::var_os(name).and_then(|v| v.into_string().ok())
                && !value.trim().is_empty()
            {
                *slot = value.trim().trim_end_matches('/').to_owned();
            }
        };
        read("RE_CODE_HUB_GITHUB_API", &mut bases.github_api);
        read("RE_CODE_HUB_GITHUB_RAW", &mut bases.github_raw);
        read("RE_CODE_HUB_SKILLS_SH", &mut bases.skills_sh);
        read("RE_CODE_HUB_HERMES_INDEX", &mut bases.hermes_index);
        read("RE_CODE_HUB_HERMES_CATALOG", &mut bases.hermes_catalog);
        bases
    }
}

/// One entry of a repository folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    /// Relative to the folder that was listed.
    pub path: String,
    pub kind: EntryKind,
    pub size: u64,
    /// The file is marked executable in git.
    pub executable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
    Submodule,
}

/// A listed folder.
#[derive(Debug, Clone, Default)]
pub struct Tree {
    pub entries: Vec<TreeEntry>,
    /// GitHub stops listing very large trees; the rest is unknown.
    pub truncated: bool,
}

impl Tree {
    /// Folders holding a `SKILL.md`, as relative paths ("" for the root), nearest first.
    #[must_use]
    pub fn skill_dirs(&self) -> Vec<String> {
        let mut dirs: Vec<String> = self
            .entries
            .iter()
            .filter(|entry| entry.kind == EntryKind::File)
            .filter_map(|entry| entry.path.strip_suffix("SKILL.md").map(|dir| dir.trim_end_matches('/').to_owned()))
            .collect();
        dirs.sort_by_key(|dir| (dir.matches('/').count(), dir.clone()));
        dirs
    }
}

/// A folder of a repository at a commit.
#[derive(Debug, Clone, Copy)]
pub struct Folder<'a> {
    pub owner: &'a str,
    pub repo: &'a str,
    pub sha: &'a str,
    /// Empty for the root.
    pub subdir: &'a str,
}

/// Files written to disk by a download.
#[derive(Debug, Clone, Default)]
pub struct Downloaded {
    /// Relative paths, sorted.
    pub files: Vec<String>,
    pub bytes: u64,
    /// Entries that weren't taken, and why: links and submodules.
    pub skipped: Vec<(String, String)>,
}

/// The client. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Fetcher {
    client: reqwest::Client,
    bases: Bases,
    /// `GITHUB_TOKEN` from the environment, sent to the GitHub API only. Never stored.
    github_token: Option<String>,
}

impl Fetcher {
    pub fn new(bases: Bases) -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .user_agent(USER_AGENT)
            .build()
            .map_err(|error| format!("couldn't set up HTTP: {error}"))?;
        let github_token = std::env::var("GITHUB_TOKEN").ok().map(|t| t.trim().to_owned()).filter(|t| !t.is_empty());
        Ok(Self { client, bases, github_token })
    }

    #[must_use]
    pub fn bases(&self) -> &Bases {
        &self.bases
    }

    /// Pins a reference (branch, tag, commit, or `HEAD` for the default branch) to its commit.
    pub async fn resolve_ref(&self, owner: &str, repo: &str, reference: Option<&str>) -> Result<String, String> {
        let reference = reference.map(str::trim).filter(|r| !r.is_empty()).unwrap_or("HEAD");
        if reference.len() == 40 && reference.chars().all(|c| c.is_ascii_hexdigit()) {
            // Already a commit: nothing to ask.
            return Ok(reference.to_ascii_lowercase());
        }
        let url = format!("{}/repos/{owner}/{repo}/commits/{reference}", self.bases.github_api);
        let text = self.github(&url).await.map_err(|error| match error {
            GitHubError::NotFound => format!("GitHub has no {owner}/{repo} at {reference}, or it's private."),
            other => other.to_string(),
        })?;
        #[derive(Deserialize)]
        struct Commit {
            sha: String,
        }
        let commit: Commit =
            serde_json::from_str(&text).map_err(|error| format!("GitHub's commit answer was unexpected: {error}"))?;
        if commit.sha.len() != 40 || !commit.sha.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err("GitHub's commit answer had no usable sha.".to_owned());
        }
        Ok(commit.sha)
    }

    /// Lists a folder of a repository at a commit, recursively. `subdir` empty means the root.
    pub async fn tree(&self, owner: &str, repo: &str, sha: &str, subdir: &str) -> Result<Tree, String> {
        let subdir = subdir.trim_matches('/');
        let spec = if subdir.is_empty() { sha.to_owned() } else { format!("{sha}:{subdir}") };
        let url = format!("{}/repos/{owner}/{repo}/git/trees/{}?recursive=1", self.bases.github_api, urlpath(&spec));
        let text = self.github(&url).await.map_err(|error| match error {
            GitHubError::NotFound if subdir.is_empty() => format!("GitHub has no {owner}/{repo}, or it's private."),
            GitHubError::NotFound => format!("{owner}/{repo} has no folder {subdir} at that commit."),
            other => other.to_string(),
        })?;
        #[derive(Deserialize)]
        struct Listing {
            #[serde(default)]
            tree: Vec<Item>,
            #[serde(default)]
            truncated: bool,
        }
        #[derive(Deserialize)]
        struct Item {
            path: String,
            #[serde(rename = "type")]
            kind: String,
            #[serde(default)]
            mode: String,
            #[serde(default)]
            size: u64,
        }
        let listing: Listing =
            serde_json::from_str(&text).map_err(|error| format!("GitHub's tree answer was unexpected: {error}"))?;
        let entries = listing
            .tree
            .into_iter()
            .map(|item| {
                let kind = match (item.kind.as_str(), item.mode.as_str()) {
                    (_, "120000") => EntryKind::Symlink,
                    ("commit", _) | (_, "160000") => EntryKind::Submodule,
                    ("tree", _) => EntryKind::Directory,
                    _ => EntryKind::File,
                };
                TreeEntry { path: item.path, kind, size: item.size, executable: item.mode == "100755" }
            })
            .collect();
        Ok(Tree { entries, truncated: listing.truncated })
    }

    /// Downloads a repository folder's files into `dest`, which must exist and be empty. Only regular
    /// files and folders are written; links and submodules are reported in `skipped`. Paths that leave
    /// the folder are refused outright.
    pub async fn download_dir(
        &self,
        folder: Folder<'_>,
        tree: &Tree,
        dest: &Path,
        caps: Caps,
    ) -> Result<Downloaded, String> {
        let Folder { owner, repo, sha, subdir } = folder;
        let mut files: Vec<&TreeEntry> = Vec::new();
        let mut skipped = Vec::new();
        for entry in &tree.entries {
            check_relative(&entry.path)?;
            match entry.kind {
                EntryKind::File => files.push(entry),
                EntryKind::Directory => {}
                EntryKind::Symlink => skipped.push((entry.path.clone(), "is a symbolic link".to_owned())),
                EntryKind::Submodule => skipped.push((entry.path.clone(), "is a git submodule".to_owned())),
            }
        }
        if tree.truncated {
            return Err(
                "GitHub listed only part of that folder (it's very large), so it can't be installed whole.".to_owned()
            );
        }
        if files.len() > caps.max_files {
            return Err(format!("That skill has {} files; the limit is {}.", files.len(), caps.max_files));
        }
        let total: u64 = files.iter().map(|f| f.size).sum();
        if total > caps.max_total_bytes {
            return Err(format!("That skill is {}; the limit is {}.", human(total), human(caps.max_total_bytes)));
        }
        if let Some(big) = files.iter().find(|f| f.size > caps.max_file_bytes) {
            return Err(format!(
                "{} is {}; the limit per file is {}.",
                big.path,
                human(big.size),
                human(caps.max_file_bytes)
            ));
        }
        let subdir = subdir.trim_matches('/');
        let gate = std::sync::Arc::new(Semaphore::new(PARALLEL));
        let mut tasks = tokio::task::JoinSet::new();
        for entry in &files {
            let repo_path = if subdir.is_empty() { entry.path.clone() } else { format!("{subdir}/{}", entry.path) };
            let url = format!("{}/{owner}/{repo}/{sha}/{}", self.bases.github_raw, urlpath(&repo_path));
            let client = self.client.clone();
            let gate = gate.clone();
            let relative = entry.path.clone();
            let executable = entry.executable;
            let target = dest.join(&relative);
            let limit = caps.max_file_bytes;
            tasks.spawn(async move {
                let _permit = gate.acquire_owned().await.map_err(|_| "cancelled".to_owned())?;
                let bytes = fetch_bytes(&client, &url, limit).await.map_err(|error| format!("{relative}: {error}"))?;
                write_file(&target, &bytes, executable).map_err(|error| format!("{relative}: {error}"))?;
                Ok::<(String, u64), String>((relative, bytes.len() as u64))
            });
        }
        let mut downloaded = Downloaded { skipped, ..Downloaded::default() };
        while let Some(result) = tasks.join_next().await {
            let (path, size) = result.map_err(|error| format!("a download task failed: {error}"))??;
            downloaded.files.push(path);
            downloaded.bytes += size;
        }
        downloaded.files.sort();
        Ok(downloaded)
    }

    /// One file from a repository at a commit.
    pub async fn raw(&self, owner: &str, repo: &str, sha: &str, path: &str, limit: u64) -> Result<Vec<u8>, String> {
        let url = format!("{}/{owner}/{repo}/{sha}/{}", self.bases.github_raw, urlpath(path.trim_matches('/')));
        fetch_bytes(&self.client, &url, limit).await
    }

    /// Any text at a URL, with a size limit.
    pub async fn text(&self, url: &str, limit: u64) -> Result<String, String> {
        let bytes = fetch_bytes(&self.client, url, limit).await?;
        String::from_utf8(bytes).map_err(|_| format!("{url} isn't text."))
    }

    /// A skill from skills.sh's API: its files as (path, contents). `Ok(None)` when the API wants a
    /// login or doesn't know the skill, so the caller can fall back to the repository.
    pub async fn skills_sh(&self, source: &str, slug: &str) -> Result<Option<Vec<(String, String)>>, String> {
        let url = format!("{}/api/v1/skills/{}/{}", self.bases.skills_sh, urlpath(source), urlencode(slug));
        let response = self.client.get(&url).send().await.map_err(|error| format!("can't reach skills.sh: {error}"))?;
        let status = response.status();
        if matches!(status.as_u16(), 401 | 403 | 404) {
            return Ok(None);
        }
        if !status.is_success() {
            return Err(format!("skills.sh answered HTTP {}.", status.as_u16()));
        }
        #[derive(Deserialize)]
        struct Answer {
            #[serde(default)]
            files: Option<Vec<File>>,
        }
        #[derive(Deserialize)]
        struct File {
            path: String,
            #[serde(default)]
            contents: Option<String>,
            #[serde(default)]
            content: Option<String>,
        }
        let answer: Answer =
            response.json().await.map_err(|error| format!("skills.sh's answer was unexpected: {error}"))?;
        let Some(files) = answer.files else { return Ok(None) };
        let files = files
            .into_iter()
            .filter_map(|file| file.contents.or(file.content).map(|text| (file.path, text)))
            .collect::<Vec<_>>();
        Ok(if files.is_empty() { None } else { Some(files) })
    }

    /// A JSON document kept in `cache_file` for `max_age`. Returns the text and whether it's a stale copy
    /// kept because the download failed (the error is then in the second value's note).
    pub async fn cached_json(
        &self,
        url: &str,
        cache_file: &Path,
        max_age: Duration,
        limit: u64,
    ) -> Result<Cached, String> {
        let age = std::fs::metadata(cache_file).and_then(|m| m.modified()).ok().and_then(|m| m.elapsed().ok());
        if let Some(age) = age
            && age < max_age
            && let Ok(text) = std::fs::read_to_string(cache_file)
        {
            return Ok(Cached { text, stale_because: None, age });
        }
        match fetch_bytes(&self.client, url, limit).await {
            Ok(bytes) => {
                let text = String::from_utf8(bytes).map_err(|_| format!("{url} isn't text."))?;
                if let Some(parent) = cache_file.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let temp = cache_file.with_extension("part");
                std::fs::write(&temp, &text)
                    .and_then(|()| std::fs::rename(&temp, cache_file))
                    .map_err(|error| format!("couldn't keep {}: {error}", cache_file.display()))?;
                Ok(Cached { text, stale_because: None, age: Duration::ZERO })
            }
            Err(error) => match (age, std::fs::read_to_string(cache_file)) {
                (Some(age), Ok(text)) => Ok(Cached { text, stale_because: Some(error), age }),
                _ => Err(error),
            },
        }
    }

    /// A GitHub API call, with the token when there is one and the rate limit explained.
    async fn github(&self, url: &str) -> Result<String, GitHubError> {
        let mut request = self
            .client
            .get(url)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(token) = &self.github_token {
            request = request.bearer_auth(token);
        }
        let response =
            request.send().await.map_err(|error| GitHubError::Other(format!("can't reach GitHub: {error}")))?;
        let status = response.status();
        if status.is_success() {
            return response.text().await.map_err(|error| GitHubError::Other(error.to_string()));
        }
        let remaining = header(&response, "x-ratelimit-remaining");
        let reset = header(&response, "x-ratelimit-reset").and_then(|v| v.parse::<u64>().ok());
        if matches!(status.as_u16(), 403 | 429) && remaining.as_deref() == Some("0") {
            let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            let wait = reset.map(|reset| reset.saturating_sub(now)).unwrap_or(0);
            let when = if wait == 0 {
                "shortly".to_owned()
            } else if wait < 120 {
                format!("in {wait} seconds")
            } else {
                format!("in {} minutes", wait.div_ceil(60))
            };
            let how = if self.github_token.is_some() {
                "Your GITHUB_TOKEN's limit"
            } else {
                "GitHub's limit for anonymous requests (60 an hour)"
            };
            return Err(GitHubError::RateLimited(format!(
                "{how} is used up; it resets {when}. Set GITHUB_TOKEN in the environment to raise it (it's only sent to GitHub, never stored)."
            )));
        }
        if status.as_u16() == 404 {
            return Err(GitHubError::NotFound);
        }
        let body = response.text().await.unwrap_or_default();
        let message = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(str::to_owned))
            .unwrap_or_default();
        Err(GitHubError::Other(if message.is_empty() {
            format!("GitHub answered HTTP {}.", status.as_u16())
        } else {
            format!("GitHub answered HTTP {}: {message}", status.as_u16())
        }))
    }
}

/// A cached document.
#[derive(Debug, Clone)]
pub struct Cached {
    pub text: String,
    /// Set when the download failed and an older copy is being used.
    pub stale_because: Option<String>,
    pub age: Duration,
}

#[derive(Debug)]
enum GitHubError {
    NotFound,
    RateLimited(String),
    Other(String),
}

impl std::fmt::Display for GitHubError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => f.write_str("GitHub has nothing at that address."),
            Self::RateLimited(text) | Self::Other(text) => f.write_str(text),
        }
    }
}

fn header(response: &reqwest::Response, name: &str) -> Option<String> {
    response.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_owned)
}

/// Downloads a body, stopping at `limit` bytes.
async fn fetch_bytes(client: &reqwest::Client, url: &str, limit: u64) -> Result<Vec<u8>, String> {
    use futures_util::StreamExt;
    let response = client.get(url).send().await.map_err(|error| format!("can't reach {}: {error}", host_of(url)))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("{} answered HTTP {}.", host_of(url), status.as_u16()));
    }
    if let Some(length) = response.content_length()
        && length > limit
    {
        return Err(format!("{} is {}; the limit is {}.", url, human(length), human(limit)));
    }
    let mut stream = response.bytes_stream();
    let mut out: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| format!("download interrupted: {error}"))?;
        if out.len() as u64 + chunk.len() as u64 > limit {
            return Err(format!("{url} is larger than the limit of {}.", human(limit)));
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

/// Writes a file, creating its folders, with mode 0644 (0755 when git marks it executable).
pub(crate) fn write_file(target: &Path, bytes: &[u8], executable: bool) -> std::io::Result<()> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(target, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if executable { 0o755 } else { 0o644 };
        std::fs::set_permissions(target, std::fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

/// Refuses a path that isn't a plain relative path inside the folder.
pub(crate) fn check_relative(path: &str) -> Result<(), String> {
    if path.is_empty() || path.starts_with('/') || path.contains('\\') || path.contains('\0') {
        return Err(format!("refused a file path that isn't relative: {path}"));
    }
    if path.split('/').any(|part| part.is_empty() || part == "." || part == "..") {
        return Err(format!("refused a file path that leaves the folder: {path}"));
    }
    Ok(())
}

fn host_of(url: &str) -> &str {
    url.split("://").nth(1).and_then(|rest| rest.split('/').next()).unwrap_or(url)
}

/// "1.2 MB" and the like, for limits and sizes.
#[must_use]
pub fn human(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{} kB", bytes.div_ceil(1024))
    } else {
        format!("{bytes} bytes")
    }
}

/// Percent-encodes one path segment.
fn urlencode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b':' => out.push(byte as char),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Percent-encodes a path, keeping its `/` separators.
fn urlpath(path: &str) -> String {
    path.split('/').map(urlencode).collect::<Vec<_>>().join("/")
}

/// UTC civil time like `2026-11-07T19:00:00Z` for a moment, without a date library.
#[must_use]
pub fn utc_stamp(at: SystemTime) -> String {
    let secs = at.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rem = secs % 86_400;
    // Howard Hinnant's days-to-civil algorithm.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths_are_checked() {
        assert!(check_relative("a/b.md").is_ok());
        for bad in ["/etc/passwd", "../x", "a/../b", "a//b", "", "a/./b", "a\\b"] {
            assert!(check_relative(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn stamps_and_sizes_read_well() {
        assert_eq!(utc_stamp(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        assert_eq!(utc_stamp(UNIX_EPOCH + Duration::from_secs(1_794_109_964)), "2026-11-08T03:52:44Z");
        assert_eq!(utc_stamp(UNIX_EPOCH + Duration::from_secs(951_782_400)), "2000-02-29T00:00:00Z");
        assert_eq!(human(512), "512 bytes");
        assert_eq!(human(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(human(1500), "2 kB");
    }

    #[test]
    fn tree_lists_skill_folders_nearest_first() {
        let entry = |path: &str| TreeEntry { path: path.to_owned(), kind: EntryKind::File, size: 1, executable: false };
        let tree = Tree {
            entries: vec![
                entry("skills/b/SKILL.md"),
                entry("deep/er/c/SKILL.md"),
                entry("skills/a/SKILL.md"),
                entry("README.md"),
            ],
            truncated: false,
        };
        assert_eq!(tree.skill_dirs(), ["skills/a", "skills/b", "deep/er/c"]);
        let root = Tree { entries: vec![entry("SKILL.md")], truncated: false };
        assert_eq!(root.skill_dirs(), [""]);
    }

    #[test]
    fn urls_are_encoded_but_slashes_kept_in_paths() {
        assert_eq!(urlpath("a b/c#d"), "a%20b/c%23d");
        assert_eq!(urlpath("abc:skills/pdf"), "abc:skills/pdf", "GitHub takes a sha:path tree spec with its slashes");
    }
}
