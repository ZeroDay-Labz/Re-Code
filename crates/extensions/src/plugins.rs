//! Plugins: a folder bringing skills, MCP servers and things re-code can't run, installed from the
//! Hermes Agent plugin catalog, a Claude Code marketplace, GitHub or a local folder. The files are
//! fetched the same way skills are (`fetch`: no git, nothing executed), examined in staging, shown
//! with Hermes' wording (`name [tier] pinned @ sha · …`), and moved under re-code's data directory.
//! Skills and MCP servers from an enabled plugin join the others; Python tools, desktop plugins,
//! agents, LSP servers and hooks are listed as not run here (hooks are translated into re-code's
//! format next to the plugin, for the user to copy if wanted, never loaded by themselves).

use crate::fetch::{Caps, Fetcher, Folder, check_relative, utc_stamp, write_file};
use crate::skillhub::{SOURCE_FILE, read_source};
use crate::{frontmatter, paste, scan, skills};
use recode_core::extensions::{
    ClaudePluginInfo, ExternalSource, InstalledSource, Marketplace, McpServer, Plugin, PluginListing, PluginOutcome,
    PluginPart, PluginPreview, Replacing, Skill, SkillSource, validate_name,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// Hooks translated from a Claude Code plugin, written beside it and never loaded on their own.
pub const HOOKS_CONVERTED: &str = "hooks.converted.toml";
const CATALOG_MAX_AGE: Duration = Duration::from_secs(6 * 60 * 60);
const CATALOG_LIMIT: u64 = 16 * 1024 * 1024;
const MANIFEST_LIMIT: u64 = 8 * 1024 * 1024;
/// A plugin may hold more than a skill: twice the skill limits.
const PLUGIN_CAPS: Caps = Caps { max_total_bytes: 10 * 1024 * 1024, max_files: 800, max_file_bytes: 2 * 1024 * 1024 };
/// The Hermes catalog's name as a marketplace.
pub const HERMES_CATALOG: &str = "hermes";

// ---------------------------------------------------------------------------------------------
// Reading installed plugins

/// Every plugin folder under `plugins_dir`, by name.
#[must_use]
pub fn discover(plugins_dir: &Path, disabled: &BTreeSet<String>) -> Vec<Plugin> {
    let Ok(entries) = std::fs::read_dir(plugins_dir) else { return Vec::new() };
    let mut plugins: Vec<Plugin> = entries
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| read_plugin(&entry.path(), disabled))
        .collect();
    plugins.sort_by(|a, b| a.name.cmp(&b.name));
    plugins
}

/// Reads one plugin folder. `None` when it isn't one (no manifest and nothing a plugin holds).
#[must_use]
pub fn read_plugin(dir: &Path, disabled: &BTreeSet<String>) -> Option<Plugin> {
    let folder = dir.file_name()?.to_string_lossy().into_owned();
    let manifest = read_manifest(dir);
    let parts = parts_of(dir);
    if manifest.is_none() && parts.is_empty() && read_source(dir).is_none() {
        return None;
    }
    let manifest = manifest.unwrap_or_default();
    let source = read_source(dir).unwrap_or_default();
    let label = if source.label.is_empty() { "local".to_owned() } else { source.label.clone() };
    Some(Plugin {
        name: folder.clone(),
        description: manifest.description.unwrap_or_default(),
        version: manifest.version,
        maintainer: manifest.author,
        label,
        source,
        directory: dir.display().to_string(),
        enabled: !disabled.contains(&folder),
        parts,
    })
}

/// The skills an enabled plugin brings, for the skill index.
#[must_use]
pub fn skills_of(dir: &Path, plugin_name: &str, enabled: bool, disabled_skills: &BTreeSet<String>) -> Vec<Skill> {
    let mut found = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir.join("skills")) {
        for entry in entries.flatten() {
            if let Some(mut skill) = skills::read_skill(&entry.path(), SkillSource::Plugin, disabled_skills, true) {
                skill.plugin = Some(plugin_name.to_owned());
                skill.enabled = skill.enabled && enabled;
                found.push(skill);
            }
        }
    }
    if let Ok(entries) = std::fs::read_dir(dir.join("commands")) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("md")
                && let Some(mut skill) = read_command(&path, disabled_skills)
            {
                skill.plugin = Some(plugin_name.to_owned());
                skill.enabled = skill.enabled && enabled;
                found.push(skill);
            }
        }
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

/// A Claude Code command file (`commands/<name>.md`) as a skill: its front matter's description, and
/// the body as the instructions.
fn read_command(path: &Path, disabled: &BTreeSet<String>) -> Option<Skill> {
    let text = std::fs::read_to_string(path).ok()?;
    let stem = path.file_stem()?.to_string_lossy().to_lowercase().replace(['_', ' ', '.'], "-");
    let (front, body) = frontmatter::split(&text);
    let front = front.map(frontmatter::parse).unwrap_or_default();
    let description = front.description.unwrap_or_else(|| {
        body.lines().map(str::trim).find(|line| !line.is_empty() && !line.starts_with('#')).unwrap_or("").to_owned()
    });
    let findings = scan::scan_text(path.file_name()?.to_str()?, &text);
    let verdict = scan::verdict(findings);
    let problem = validate_name("skill", &stem).err();
    let enabled = problem.is_none() && !verdict.blocks() && !disabled.contains(&stem);
    Some(Skill {
        name: stem,
        description,
        path: path.display().to_string(),
        directory: path.parent()?.display().to_string(),
        source: SkillSource::Plugin,
        enabled,
        estimated_tokens: u64::try_from(text.chars().count()).unwrap_or(u64::MAX) / 3,
        problem,
        scan: verdict,
        trusted: true,
        ..Skill::default()
    })
}

/// The MCP servers a plugin brings (`mcp.json` or `.mcp.json`), with `${CLAUDE_PLUGIN_ROOT}` pointing at
/// the plugin's folder. They are marked external with the plugin as their label.
#[must_use]
pub fn servers_of(dir: &Path, plugin_name: &str) -> Vec<McpServer> {
    let mut servers = Vec::new();
    for file in ["mcp.json", ".mcp.json"] {
        let Ok(text) = std::fs::read_to_string(dir.join(file)) else { continue };
        let text = text
            .replace("${CLAUDE_PLUGIN_ROOT}", &dir.display().to_string())
            .replace("${CLAUDE_PLUGIN_DATA}", &dir.join(".data").display().to_string());
        let Ok(parsed) = paste::parse(None, &text) else { continue };
        for mut server in parsed {
            server.external = Some(ExternalSource { label: format!("plugin {plugin_name}"), unsupported: None });
            servers.push(server);
        }
    }
    servers
}

#[derive(Debug, Default, Clone, Deserialize)]
struct Manifest {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    author: Option<serde_json::Value>,
}

#[derive(Debug, Default, Clone)]
struct ManifestInfo {
    name: Option<String>,
    version: Option<String>,
    description: Option<String>,
    author: Option<String>,
}

/// `.claude-plugin/plugin.json`, `plugin.json` or Hermes' `plugin.yaml`, whichever is there.
fn read_manifest(dir: &Path) -> Option<ManifestInfo> {
    for file in [".claude-plugin/plugin.json", "plugin.json"] {
        let Ok(text) = std::fs::read_to_string(dir.join(file)) else { continue };
        let Ok(manifest) = serde_json::from_str::<Manifest>(&text) else { continue };
        let author = manifest.author.and_then(|value| match value {
            serde_json::Value::String(name) => Some(name),
            serde_json::Value::Object(map) => map.get("name").and_then(|n| n.as_str()).map(str::to_owned),
            _ => None,
        });
        return Some(ManifestInfo {
            name: manifest.name,
            version: manifest.version,
            description: manifest.description,
            author,
        });
    }
    for file in ["plugin.yaml", "plugin.yml"] {
        let Ok(text) = std::fs::read_to_string(dir.join(file)) else { continue };
        let value = |key: &str| -> Option<String> {
            text.lines().find_map(|line| {
                let (k, v) = line.split_once(':')?;
                (k.trim() == key).then(|| v.trim().trim_matches(['"', '\'']).to_owned()).filter(|v| !v.is_empty())
            })
        };
        return Some(ManifestInfo {
            name: value("name"),
            version: value("version"),
            description: value("description"),
            author: value("author"),
        });
    }
    None
}

/// What a plugin folder holds, part by part.
#[must_use]
pub fn parts_of(dir: &Path) -> Vec<PluginPart> {
    let mut parts = Vec::new();
    for skill in skills_of(dir, "", true, &BTreeSet::new()) {
        let command = skill.path.ends_with(".md") && !skill.path.ends_with("SKILL.md");
        parts.push(PluginPart::Skill { name: skill.name, path: skill.path, scan: skill.scan, command });
    }
    for server in servers_of(dir, "") {
        parts.push(PluginPart::Mcp { name: server.name.clone(), connection: server.connection_text() });
    }
    if let Some(hooks) = read_claude_hooks(dir) {
        for hook in hooks {
            parts.push(PluginPart::Hook { event: hook.event, command: hook.command, converted: hook.converted });
        }
    }
    let has = |relative: &str| dir.join(relative).exists();
    let dir_has_ext = |relative: &str, ext: &str| {
        std::fs::read_dir(dir.join(relative))
            .map(|entries| {
                entries.flatten().any(|entry| entry.path().extension().and_then(|e| e.to_str()) == Some(ext))
            })
            .unwrap_or(false)
    };
    let python_files: Vec<String> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| entry.path().extension().and_then(|e| e.to_str()) == Some("py"))
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|name| name != "__init__.py")
                .collect()
        })
        .unwrap_or_default();
    if !python_files.is_empty() || has("tools") && dir_has_ext("tools", "py") {
        parts.push(PluginPart::NotRunHere {
            what: "Python plugin code (tools, hooks or middleware for Hermes Agent)".to_owned(),
            detail: python_files.join(", "),
        });
    }
    if has("desktop") {
        parts.push(PluginPart::NotRunHere {
            what: "desktop plugin (Hermes app interface)".to_owned(),
            detail: String::new(),
        });
    }
    if has("agents") && dir_has_ext("agents", "md") {
        parts.push(PluginPart::NotRunHere { what: "agents".to_owned(), detail: "subagent definitions".to_owned() });
    }
    if has(".lsp.json") {
        parts.push(PluginPart::NotRunHere {
            what: "LSP server".to_owned(),
            detail: "code intelligence for Claude Code".to_owned(),
        });
    }
    if has("bin") {
        parts.push(PluginPart::NotRunHere { what: "programs in bin/".to_owned(), detail: String::new() });
    }
    if has("output-styles") {
        parts.push(PluginPart::NotRunHere { what: "output styles".to_owned(), detail: String::new() });
    }
    if has("settings.json") {
        parts.push(PluginPart::NotRunHere {
            what: "settings.json".to_owned(),
            detail: "Claude Code settings".to_owned(),
        });
    }
    if has("openclaw.plugin.json") {
        parts.push(PluginPart::NotRunHere { what: "OpenClaw plugin manifest".to_owned(), detail: String::new() });
    }
    parts
}

// ---------------------------------------------------------------------------------------------
// Claude Code hooks → re-code hooks (written, never loaded by themselves)

struct ConvertedHook {
    event: String,
    command: String,
    converted: bool,
    tools: Vec<String>,
    timeout_secs: Option<u64>,
}

#[derive(Serialize)]
struct HookToml {
    event: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<String>,
    command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    timeout_secs: Option<u64>,
}

#[derive(Serialize)]
struct HooksToml {
    hook: Vec<HookToml>,
}

/// `hooks/hooks.json` as Claude Code writes it: `{"hooks": {"PreToolUse": [{"matcher": "Edit|Write",
/// "hooks": [{"type": "command", "command": "…"}]}]}}`.
fn read_claude_hooks(dir: &Path) -> Option<Vec<ConvertedHook>> {
    let text = std::fs::read_to_string(dir.join("hooks/hooks.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let events = value.get("hooks")?.as_object()?;
    let mut out = Vec::new();
    for (event, groups) in events {
        let Some(groups) = groups.as_array() else { continue };
        let mapped = match event.as_str() {
            "PreToolUse" => Some("pre_tool_use"),
            "PostToolUse" => Some("post_tool_use"),
            "Stop" => Some("stop"),
            "SessionStart" => Some("session_start"),
            _ => None,
        };
        for group in groups {
            let tools: Vec<String> = group
                .get("matcher")
                .and_then(|m| m.as_str())
                .map(|matcher| matcher.split('|').map(map_tool_name).filter(|t| !t.is_empty()).collect())
                .unwrap_or_default();
            let Some(hooks) = group.get("hooks").and_then(|h| h.as_array()) else { continue };
            for hook in hooks {
                let Some(command) = hook.get("command").and_then(|c| c.as_str()) else { continue };
                let timeout_secs = hook.get("timeout").and_then(serde_json::Value::as_u64);
                out.push(ConvertedHook {
                    event: mapped.map(str::to_owned).unwrap_or_else(|| event.clone()),
                    command: command.to_owned(),
                    converted: mapped.is_some(),
                    tools: tools.clone(),
                    timeout_secs,
                });
            }
        }
    }
    Some(out)
}

/// Claude Code's tool names in a matcher → re-code's.
fn map_tool_name(name: &str) -> String {
    match name.trim() {
        "Edit" | "MultiEdit" | "NotebookEdit" => "edit".to_owned(),
        "Write" => "write".to_owned(),
        "Bash" => "shell".to_owned(),
        "Read" => "read".to_owned(),
        "Glob" | "LS" => "list".to_owned(),
        "Grep" => "grep".to_owned(),
        other if other.starts_with('^') || other.contains(':') => String::new(),
        other => other.to_lowercase(),
    }
}

/// Writes `hooks.converted.toml` for the hooks that have an equivalent, with `${CLAUDE_PLUGIN_ROOT}`
/// resolved to where the plugin will live. Returns how many were written.
fn write_converted_hooks(staged: &Path, final_location: &Path) -> Result<usize, String> {
    let Some(hooks) = read_claude_hooks(staged) else { return Ok(0) };
    let converted: Vec<HookToml> = hooks
        .into_iter()
        .filter(|hook| hook.converted)
        .map(|hook| HookToml {
            event: hook.event,
            tools: hook.tools,
            command: hook.command.replace("${CLAUDE_PLUGIN_ROOT}", &final_location.display().to_string()),
            timeout_secs: hook.timeout_secs,
        })
        .collect();
    if converted.is_empty() {
        return Ok(0);
    }
    let count = converted.len();
    let body = toml::to_string(&HooksToml { hook: converted }).map_err(|error| error.to_string())?;
    let text = format!(
        "# Hooks translated from this plugin's hooks/hooks.json. re-code does NOT load this file: read each\n\
         # command, then copy the entries you want into ~/.config/re-code/hooks.toml (run as your own commands)\n\
         # or a trusted project's .re-code/hooks.toml (sandboxed).\n{body}"
    );
    std::fs::write(staged.join(HOOKS_CONVERTED), text).map_err(|error| error.to_string())?;
    Ok(count)
}

// ---------------------------------------------------------------------------------------------
// Where plugins come from

/// What the user asked for, parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginTap {
    /// A bare name: the Hermes catalog, then the followed marketplaces.
    Name {
        name: String,
    },
    /// `name@marketplace` (`@hermes` for the catalog).
    FromMarketplace {
        name: String,
        marketplace: String,
    },
    GitHub {
        owner: String,
        repo: String,
        subdir: String,
        reference: Option<String>,
    },
    Local {
        path: PathBuf,
    },
}

impl PluginTap {
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("Enter a plugin to install: a name from the Hermes catalog, name@marketplace, owner/repo[/folder][@ref], a GitHub link, or a folder.".to_owned());
        }
        if text.starts_with('/') || text.starts_with('~') || text.starts_with("./") || text.starts_with("../") {
            return Ok(Self::Local { path: expand_home(text) });
        }
        if Path::new(text).is_dir()
            && (Path::new(text).join("plugin.json").is_file() || Path::new(text).join(".claude-plugin").is_dir())
        {
            return Ok(Self::Local { path: PathBuf::from(text) });
        }
        let lower = text.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("https://").or_else(|| lower.strip_prefix("http://")) {
            let rest = rest.strip_prefix("www.").unwrap_or(rest);
            let prefix_len = text.len() - rest.len();
            let original = text[prefix_len..].trim_end_matches('/');
            if let Some(after) = original.strip_prefix("github.com/").or_else(|| original.strip_prefix("GitHub.com/")) {
                let parts: Vec<&str> = after.split('/').filter(|s| !s.is_empty()).collect();
                if parts.len() < 2 {
                    return Err("A GitHub link needs an owner and a repository.".to_owned());
                }
                let (reference, subdir) = match parts.get(2) {
                    Some(&"tree" | &"blob") if parts.len() >= 4 => (Some(parts[3].to_owned()), parts[4..].join("/")),
                    _ => (None, String::new()),
                };
                return Ok(Self::GitHub {
                    owner: parts[0].to_owned(),
                    repo: parts[1].trim_end_matches(".git").to_owned(),
                    subdir,
                    reference,
                });
            }
            return Err("re-code installs plugins from the Hermes catalog, Claude Code marketplaces, GitHub, or a folder on this computer.".to_owned());
        }
        if let Some((name, marketplace)) = text.rsplit_once('@')
            && !name.contains('/')
            && !name.is_empty()
            && !marketplace.is_empty()
            && !marketplace.contains('/')
        {
            return Ok(Self::FromMarketplace { name: name.to_owned(), marketplace: marketplace.to_owned() });
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
                return Err(format!("{text} doesn't look like owner/repo[/folder][@ref]."));
            }
            return Ok(Self::GitHub {
                owner: owner.to_owned(),
                repo: parts[1].trim_end_matches(".git").to_owned(),
                subdir: parts[2..].join("/"),
                reference,
            });
        }
        Ok(Self::Name { name: text.to_owned() })
    }
}

fn expand_home(text: &str) -> PathBuf {
    if let Some(rest) = text.strip_prefix('~')
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join(rest.trim_start_matches('/'));
    }
    PathBuf::from(text)
}

/// One entry of the Hermes plugin catalog (`plugin-catalog.json`, shape read live 2026-11-08).
#[derive(Debug, Clone, Default, Deserialize)]
struct CatalogEntry {
    #[serde(default)]
    name: String,
    #[serde(default)]
    repo: String,
    #[serde(default)]
    sha: String,
    #[serde(default)]
    subdir: Option<String>,
    #[serde(default)]
    description: String,
    #[serde(default)]
    maintainer: Option<String>,
    #[serde(default)]
    tier: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    category: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct Catalog {
    #[serde(default)]
    entries: Vec<CatalogEntry>,
    #[serde(default)]
    removed: Vec<Removed>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct Removed {
    #[serde(default)]
    name: String,
    #[serde(default)]
    reason: String,
}

/// A Claude Code marketplace manifest (`.claude-plugin/marketplace.json`).
#[derive(Debug, Clone, Default, Deserialize)]
struct MarketplaceManifest {
    #[serde(default)]
    name: String,
    #[serde(default)]
    plugins: Vec<MarketplacePlugin>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct MarketplacePlugin {
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    source: serde_json::Value,
}

/// Where a marketplace plugin's files are, or why they can't be fetched.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ResolvedSource {
    GitHub { owner: String, repo: String, subdir: String, reference: Option<String> },
    Local(PathBuf),
    Unsupported(String),
}

/// Reads a marketplace entry's `source`: `"./path"`, or `{source: url|git-subdir|github|archive|npm|command}`.
fn resolve_marketplace_source(
    market: &Marketplace,
    local_root: Option<&Path>,
    source: &serde_json::Value,
) -> ResolvedSource {
    if let Some(relative) = source.as_str() {
        let relative = relative.trim_start_matches("./").trim_matches('/');
        if let Some(root) = local_root {
            return ResolvedSource::Local(root.join(relative));
        }
        return match split_repo(&market.repo) {
            Some((owner, repo)) => ResolvedSource::GitHub {
                owner,
                repo,
                subdir: relative.to_owned(),
                reference: (!market.sha.is_empty()).then(|| market.sha.clone()),
            },
            None => ResolvedSource::Unsupported("the marketplace isn't on GitHub".to_owned()),
        };
    }
    let kind = source.get("source").and_then(|s| s.as_str()).unwrap_or_default();
    let text = |key: &str| source.get(key).and_then(|v| v.as_str()).map(str::to_owned);
    let sha = text("sha").filter(|sha| sha.len() == 40);
    match kind {
        "url" | "git" => match text("url").as_deref().and_then(split_repo) {
            Some((owner, repo)) => ResolvedSource::GitHub { owner, repo, subdir: String::new(), reference: sha },
            None => ResolvedSource::Unsupported(
                "its git address isn't on GitHub (other hosts need git, which installs never run)".to_owned(),
            ),
        },
        "git-subdir" => match text("url").as_deref().and_then(split_repo) {
            Some((owner, repo)) => ResolvedSource::GitHub {
                owner,
                repo,
                subdir: text("path").unwrap_or_default().trim_matches('/').to_owned(),
                reference: sha,
            },
            None => ResolvedSource::Unsupported("its git address isn't on GitHub".to_owned()),
        },
        "github" => match text("repo").as_deref().and_then(split_repo) {
            Some((owner, repo)) => {
                ResolvedSource::GitHub { owner, repo, subdir: text("path").unwrap_or_default(), reference: sha }
            }
            None => ResolvedSource::Unsupported("its repo field isn't owner/repo".to_owned()),
        },
        "archive" => ResolvedSource::Unsupported("zip archives aren't supported yet".to_owned()),
        "npm" | "command" => {
            ResolvedSource::Unsupported("this source runs code on install, which re-code never does".to_owned())
        }
        other => ResolvedSource::Unsupported(format!("unknown source type {other}")),
    }
}

fn split_repo(text: &str) -> Option<(String, String)> {
    let rest = text.trim().trim_end_matches('/');
    let rest = rest
        .strip_prefix("https://github.com/")
        .or_else(|| rest.strip_prefix("http://github.com/"))
        .or_else(|| rest.strip_prefix("github.com/"))
        .or_else(|| rest.strip_prefix("git@github.com:"))
        .or_else(|| (!rest.contains("://") && !rest.contains('@')).then_some(rest))?;
    let mut parts = rest.split('/');
    let owner = parts.next().filter(|s| !s.is_empty())?;
    let repo = parts.next().filter(|s| !s.is_empty())?.trim_end_matches(".git");
    if !owner.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return None;
    }
    Some((owner.to_owned(), repo.to_owned()))
}

/// A plugin moved into place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Committed {
    pub name: String,
    pub enabled: bool,
    pub location: String,
    pub note: Option<String>,
}

/// Fetches, stages, installs and updates plugins; follows marketplaces.
#[derive(Debug)]
pub struct PluginHub {
    fetcher: Fetcher,
    staging_root: PathBuf,
    plugins_dir: PathBuf,
    cache_dir: PathBuf,
    catalog: tokio::sync::Mutex<Option<Arc<Catalog>>>,
}

impl PluginHub {
    #[must_use]
    pub fn new(fetcher: Fetcher, cache_dir: PathBuf, plugins_dir: PathBuf) -> Self {
        Self {
            fetcher,
            staging_root: cache_dir.join("hub").join("staging"),
            plugins_dir,
            cache_dir: cache_dir.join("hub"),
            catalog: tokio::sync::Mutex::new(None),
        }
    }

    /// Where a marketplace's manifest is kept once read.
    fn manifest_cache(&self, name: &str) -> PathBuf {
        self.cache_dir.join("marketplaces").join(format!("{}.json", safe_name(name)))
    }

    /// Fetches what `text` names into staging and examines it. Nothing is installed.
    pub async fn stage(&self, text: &str, marketplaces: &[Marketplace]) -> PluginOutcome {
        let tap = text.trim().to_owned();
        match self.stage_inner(&tap, marketplaces, None).await {
            Ok(outcome) => outcome,
            Err(error) => PluginOutcome::Failed { tap, error },
        }
    }

    /// Stages the newest version of an installed plugin.
    pub async fn update(&self, name: &str, marketplaces: &[Marketplace]) -> PluginOutcome {
        let tap = format!("update {name}");
        let Some(source) = read_source(&self.plugins_dir.join(name)) else {
            let error = if self.plugins_dir.join(name).is_dir() {
                format!("{name} wasn't installed from a hub, so there's nothing to update it from.")
            } else {
                format!("There's no plugin called {name}.")
            };
            return PluginOutcome::Failed { tap, error };
        };
        match self.stage_inner(&source.tap, marketplaces, Some((name, &source))).await {
            Ok(outcome) => outcome,
            Err(error) => PluginOutcome::Failed { tap: source.tap, error },
        }
    }

    async fn stage_inner(
        &self,
        tap_text: &str,
        marketplaces: &[Marketplace],
        updating: Option<(&str, &InstalledSource)>,
    ) -> Result<PluginOutcome, String> {
        match PluginTap::parse(tap_text)? {
            PluginTap::Name { name } => {
                // The Hermes catalog first, then every followed marketplace; a name in several asks.
                let mut found: Vec<(String, ResolvedSource, String, Option<String>)> = Vec::new();
                let mut problems = Vec::new();
                match self.catalog_entry(&name).await {
                    Ok(Some(entry)) => {
                        let (owner, repo) = split_repo(&entry.repo).ok_or_else(|| {
                            format!("The catalog gives {name} a repository re-code can't read: {}", entry.repo)
                        })?;
                        found.push((
                            HERMES_CATALOG.to_owned(),
                            ResolvedSource::GitHub {
                                owner,
                                repo,
                                subdir: entry.subdir.clone().unwrap_or_default(),
                                reference: Some(entry.sha.clone()),
                            },
                            entry.tier.clone().unwrap_or_else(|| "community".to_owned()),
                            entry.maintainer.clone(),
                        ));
                    }
                    Ok(None) => {}
                    // A catalog that can't be read shouldn't stop a marketplace from answering.
                    Err(error) => problems.push(error),
                }
                for market in marketplaces {
                    match self.marketplace_plugin(market, &name) {
                        Ok(Some((resolved, _))) => {
                            found.push((market.name.clone(), resolved, market.name.clone(), None))
                        }
                        Ok(None) => {}
                        Err(error) => problems.push(error),
                    }
                }
                match found.len() {
                    0 if !problems.is_empty() => Err(problems.join(" ")),
                    0 => Err(format!(
                        "Nothing called {name} in the Hermes catalog{}. Try /plugin search <words>, or name@marketplace.",
                        if marketplaces.is_empty() { "" } else { " or your marketplaces" }
                    )),
                    1 => {
                        let (_, resolved, label, maintainer) = found.remove(0);
                        self.stage_resolved(resolved, &label, maintainer, tap_text, updating).await
                    }
                    _ => Err(format!(
                        "{name} is offered by {}: say which with name@marketplace.",
                        found.iter().map(|f| f.0.as_str()).collect::<Vec<_>>().join(" and ")
                    )),
                }
            }
            PluginTap::FromMarketplace { name, marketplace } => {
                if marketplace == HERMES_CATALOG {
                    let entry = self
                        .catalog_entry(&name)
                        .await?
                        .ok_or_else(|| format!("The Hermes catalog has nothing called {name}."))?;
                    let (owner, repo) = split_repo(&entry.repo).ok_or_else(|| {
                        format!("The catalog gives {name} a repository re-code can't read: {}", entry.repo)
                    })?;
                    let resolved = ResolvedSource::GitHub {
                        owner,
                        repo,
                        subdir: entry.subdir.clone().unwrap_or_default(),
                        reference: Some(entry.sha.clone()),
                    };
                    let label = entry.tier.clone().unwrap_or_else(|| "community".to_owned());
                    return self.stage_resolved(resolved, &label, entry.maintainer.clone(), tap_text, updating).await;
                }
                let market = marketplaces
                    .iter()
                    .find(|m| m.name == marketplace)
                    .ok_or_else(|| format!("You don't follow a marketplace called {marketplace}. Add it with /plugin marketplace add <owner/repo>."))?;
                let (resolved, _) = self
                    .marketplace_plugin(market, &name)?
                    .ok_or_else(|| format!("{marketplace} has no plugin called {name}."))?;
                self.stage_resolved(resolved, &market.name, None, tap_text, updating).await
            }
            PluginTap::GitHub { owner, repo, subdir, reference } => {
                let resolved = ResolvedSource::GitHub { owner, repo, subdir, reference };
                self.stage_resolved(resolved, "github", None, tap_text, updating).await
            }
            PluginTap::Local { path } => {
                self.stage_resolved(ResolvedSource::Local(path), "local", None, tap_text, updating).await
            }
        }
    }

    async fn stage_resolved(
        &self,
        resolved: ResolvedSource,
        label: &str,
        maintainer: Option<String>,
        tap_text: &str,
        updating: Option<(&str, &InstalledSource)>,
    ) -> Result<PluginOutcome, String> {
        match resolved {
            ResolvedSource::Unsupported(why) => Err(format!("{tap_text} can't be installed: {why}.")),
            ResolvedSource::Local(path) => {
                if !path.is_dir() {
                    return Err(format!("{} isn't a folder.", path.display()));
                }
                let (id, dest) = self.fresh_staging()?;
                copy_tree(&path, &dest, PLUGIN_CAPS).inspect_err(|_| {
                    let _ = std::fs::remove_dir_all(&id);
                })?;
                let source = InstalledSource {
                    tap: tap_text.to_owned(),
                    repo: path.display().to_string(),
                    path: String::new(),
                    sha: String::new(),
                    label: label.to_owned(),
                    installed_at: String::new(),
                };
                self.examine(&id, dest, source, maintainer, None, updating)
            }
            ResolvedSource::GitHub { owner, repo, subdir, reference } => {
                let sha = self.fetcher.resolve_ref(&owner, &repo, reference.as_deref()).await?;
                if let Some((name, current)) = updating
                    && current.sha == sha
                {
                    return Ok(PluginOutcome::UpToDate { name: name.to_owned(), sha });
                }
                let tree = self.fetcher.tree(&owner, &repo, &sha, &subdir).await?;
                let (id, dest) = self.fresh_staging()?;
                let folder = Folder { owner: &owner, repo: &repo, sha: &sha, subdir: &subdir };
                let downloaded = match self.fetcher.download_dir(folder, &tree, &dest, PLUGIN_CAPS).await {
                    Ok(downloaded) => downloaded,
                    Err(error) => {
                        let _ = std::fs::remove_dir_all(&id);
                        return Err(error);
                    }
                };
                let note = (!downloaded.skipped.is_empty()).then(|| {
                    format!(
                        "Not taken: {}.",
                        downloaded.skipped.iter().map(|(p, why)| format!("{p} ({why})")).collect::<Vec<_>>().join(", ")
                    )
                });
                let source = InstalledSource {
                    tap: strip_pin(tap_text),
                    repo: format!("{owner}/{repo}"),
                    path: subdir,
                    sha,
                    label: label.to_owned(),
                    installed_at: String::new(),
                };
                self.examine(&id, dest, source, maintainer, note, updating)
            }
        }
    }

    fn fresh_staging(&self) -> Result<(PathBuf, PathBuf), String> {
        let nanos = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let id = self.staging_root.join(format!("p{nanos:x}-{:x}", std::process::id()));
        let dest = id.join("plugin");
        std::fs::create_dir_all(&dest).map_err(|error| format!("couldn't create {}: {error}", dest.display()))?;
        Ok((id, dest))
    }

    /// Reads a staged folder as a plugin, renames it to its name and builds the preview.
    fn examine(
        &self,
        id: &Path,
        dest: PathBuf,
        mut source: InstalledSource,
        maintainer: Option<String>,
        note: Option<String>,
        updating: Option<(&str, &InstalledSource)>,
    ) -> Result<PluginOutcome, String> {
        let fail = |error: String| {
            let _ = std::fs::remove_dir_all(id);
            error
        };
        let manifest = read_manifest(&dest);
        let parts = parts_of(&dest);
        if manifest.is_none() && parts.is_empty() {
            return Err(fail(
                "That folder has no plugin manifest and nothing a plugin brings (skills/, commands/, mcp.json)."
                    .to_owned(),
            ));
        }
        let raw_name = updating
            .map(|(name, _)| name.to_owned())
            .or_else(|| manifest.as_ref().and_then(|m| m.name.clone()))
            .or_else(|| source.tap.rsplit(['/', '@']).next().map(str::to_owned))
            .unwrap_or_else(|| "plugin".to_owned());
        let name = safe_name(&raw_name);
        validate_name("plugin", &name).map_err(fail)?;
        let staged = id.join(&name);
        std::fs::rename(&dest, &staged).map_err(|error| fail(format!("couldn't rename the staged folder: {error}")))?;

        let target = self.plugins_dir.join(&name);
        let replaces = if target.exists() {
            match read_source(&target) {
                Some(current) => Some(Replacing { sha: current.sha, changed: changed_files(&target, &staged) }),
                None => {
                    return Err(fail(format!(
                        "You already have a plugin folder called {name} that wasn't installed from a hub. Remove it first."
                    )));
                }
            }
        } else {
            None
        };
        let files = list_files(&staged);
        let bytes = files.iter().filter_map(|file| std::fs::metadata(staged.join(file)).ok()).map(|m| m.len()).sum();
        let converted = write_converted_hooks(&staged, &target).map_err(fail)?;
        source.installed_at = utc_stamp(SystemTime::now());
        source.label = if source.label.is_empty() { "local".to_owned() } else { source.label };
        write_source(&staged, &source).map_err(fail)?;
        let mut plugin =
            read_plugin(&staged, &BTreeSet::new()).ok_or_else(|| fail("The staged plugin vanished.".to_owned()))?;
        plugin.name = name;
        plugin.directory = staged.display().to_string();
        if plugin.maintainer.is_none() {
            plugin.maintainer = maintainer;
        }
        let mut note = note;
        if converted > 0 {
            let extra = format!(
                "{converted} hook(s) were translated into {HOOKS_CONVERTED}; re-code doesn't load them until you copy them into your hooks.toml."
            );
            note = Some(match note {
                Some(existing) => format!("{existing} {extra}"),
                None => extra,
            });
        }
        Ok(PluginOutcome::Preview(Box::new(PluginPreview {
            staging: staged.display().to_string(),
            plugin,
            files: files.len(),
            bytes,
            location: target.display().to_string(),
            note,
            replaces,
        })))
    }

    /// Moves a staged plugin into the plugins folder. Nothing is enabled beyond its skills and servers.
    pub fn commit(&self, staging: &str, enable: bool) -> Result<Committed, String> {
        let staged = self.staged_path(staging)?;
        let name = staged.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_owned();
        validate_name("plugin", &name)?;
        let parts = parts_of(&staged);
        let dangerous = parts.iter().any(|part| matches!(part, PluginPart::Skill { scan, .. } if scan.blocks()));
        let (enabled, note) = if enable && dangerous {
            (false, Some("A skill in it was flagged as dangerous, so the plugin stays off (the flagged skill can never be enabled).".to_owned()))
        } else {
            (enable, None)
        };
        let target = self.plugins_dir.join(&name);
        if target.exists() {
            if read_source(&target).is_none() {
                return Err(format!("{name} exists and wasn't installed from a hub; remove it first."));
            }
            std::fs::remove_dir_all(&target).map_err(|error| format!("couldn't remove the old {name}: {error}"))?;
        }
        std::fs::create_dir_all(&self.plugins_dir)
            .map_err(|error| format!("couldn't create {}: {error}", self.plugins_dir.display()))?;
        if std::fs::rename(&staged, &target).is_err() {
            copy_tree(
                &staged,
                &target,
                Caps { max_total_bytes: u64::MAX, max_files: usize::MAX, max_file_bytes: u64::MAX },
            )
            .map_err(|error| format!("couldn't move the plugin into place: {error}"))?;
            let _ = std::fs::remove_dir_all(&staged);
        }
        if let Some(parent) = staged.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
        Ok(Committed { name, enabled, location: target.display().to_string(), note })
    }

    /// Removes a staged plugin.
    pub fn discard(&self, staging: &str) -> Result<(), String> {
        let staged = self.staged_path(staging)?;
        let parent = staged.parent().unwrap_or(&staged);
        std::fs::remove_dir_all(parent).map_err(|error| format!("couldn't remove the staged plugin: {error}"))
    }

    fn staged_path(&self, staging: &str) -> Result<PathBuf, String> {
        let canonical = Path::new(staging).canonicalize().map_err(|_| "That staged plugin is gone.".to_owned())?;
        let root = self.staging_root.canonicalize().map_err(|_| "That staged plugin is gone.".to_owned())?;
        if !canonical.starts_with(&root) || canonical == root || canonical.parent() == Some(root.as_path()) {
            return Err("That isn't a staged plugin.".to_owned());
        }
        if !canonical.join(SOURCE_FILE).is_file() {
            return Err("The staged folder has no source record.".to_owned());
        }
        Ok(canonical)
    }

    /// Removes an installed plugin's folder.
    pub fn remove(&self, name: &str) -> Result<(), String> {
        validate_name("plugin", name)?;
        let target = self.plugins_dir.join(name);
        if !target.is_dir() {
            return Err(format!("There's no plugin called {name}."));
        }
        std::fs::remove_dir_all(&target).map_err(|error| format!("couldn't remove {name}: {error}"))
    }

    // -----------------------------------------------------------------------------------------
    // The Hermes catalog

    async fn catalog(&self) -> Result<Arc<Catalog>, String> {
        let mut slot = self.catalog.lock().await;
        if let Some(catalog) = slot.as_ref() {
            return Ok(catalog.clone());
        }
        let url = self.fetcher.bases().hermes_catalog.clone();
        let cached = self
            .fetcher
            .cached_json(&url, &self.cache_dir.join("hermes-plugin-catalog.json"), CATALOG_MAX_AGE, CATALOG_LIMIT)
            .await
            .map_err(|error| format!("The Hermes plugin catalog couldn't be fetched: {error}"))?;
        let catalog: Catalog = serde_json::from_str(&cached.text)
            .map_err(|error| format!("The Hermes plugin catalog isn't in the shape re-code expects: {error}"))?;
        let catalog = Arc::new(catalog);
        *slot = Some(catalog.clone());
        Ok(catalog)
    }

    async fn catalog_entry(&self, name: &str) -> Result<Option<CatalogEntry>, String> {
        let catalog = self.catalog().await?;
        if let Some(entry) = catalog.entries.iter().find(|entry| entry.name.eq_ignore_ascii_case(name)) {
            if entry.sha.len() != 40 {
                return Err(format!(
                    "The catalog entry for {name} has no commit pin, so it can't be installed safely."
                ));
            }
            return Ok(Some(entry.clone()));
        }
        if let Some(removed) = catalog.removed.iter().find(|removed| removed.name.eq_ignore_ascii_case(name)) {
            return Err(format!("{name} was removed from the Hermes catalog: {}", removed.reason));
        }
        Ok(None)
    }

    // -----------------------------------------------------------------------------------------
    // Marketplaces

    /// Follows a marketplace: reads its manifest at a pinned commit (or from a local folder) and
    /// returns the record to store.
    pub async fn add_marketplace(&self, text: &str) -> Result<Marketplace, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("Name the marketplace: owner/repo[@ref], a GitHub link, or a folder.".to_owned());
        }
        let (manifest_text, repo, sha) = match PluginTap::parse(text)? {
            PluginTap::Local { path } => {
                let file = path.join(".claude-plugin/marketplace.json");
                let text = std::fs::read_to_string(&file)
                    .map_err(|_| format!("{} has no .claude-plugin/marketplace.json.", path.display()))?;
                (text, path.display().to_string(), String::new())
            }
            PluginTap::GitHub { owner, repo, subdir, reference } => {
                let sha = self.fetcher.resolve_ref(&owner, &repo, reference.as_deref()).await?;
                let file = if subdir.is_empty() {
                    ".claude-plugin/marketplace.json".to_owned()
                } else {
                    format!("{subdir}/.claude-plugin/marketplace.json")
                };
                let bytes = self
                    .fetcher
                    .raw(&owner, &repo, &sha, &file, MANIFEST_LIMIT)
                    .await
                    .map_err(|_| format!("{owner}/{repo} has no .claude-plugin/marketplace.json at its root."))?;
                let text = String::from_utf8(bytes).map_err(|_| "The marketplace manifest isn't text.".to_owned())?;
                (text, format!("{owner}/{repo}"), sha)
            }
            PluginTap::Name { .. } | PluginTap::FromMarketplace { .. } => {
                return Err("A marketplace is a repository: owner/repo[@ref], a GitHub link, or a folder.".to_owned());
            }
        };
        let manifest: MarketplaceManifest = serde_json::from_str(&paste::without_comments(&manifest_text))
            .map_err(|error| format!("The marketplace manifest couldn't be read: {error}"))?;
        let name = if manifest.name.is_empty() {
            repo.rsplit('/').next().unwrap_or("marketplace").to_owned()
        } else {
            manifest.name.clone()
        };
        let name = safe_name(&name);
        if name == HERMES_CATALOG {
            return Err("hermes is the Hermes catalog's own name; the marketplace can't use it.".to_owned());
        }
        let cache = self.manifest_cache(&name);
        if let Some(parent) = cache.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        std::fs::write(&cache, &manifest_text).map_err(|error| format!("couldn't keep the manifest: {error}"))?;
        Ok(Marketplace {
            name,
            repo,
            sha,
            added_at: utc_stamp(SystemTime::now()),
            plugin_count: manifest.plugins.len(),
        })
    }

    /// Forgets a marketplace's cached manifest.
    pub fn forget_marketplace(&self, name: &str) {
        let _ = std::fs::remove_file(self.manifest_cache(name));
    }

    fn manifest_of(&self, market: &Marketplace) -> Result<(MarketplaceManifest, Option<PathBuf>), String> {
        let local_root =
            (market.sha.is_empty() && Path::new(&market.repo).is_dir()).then(|| PathBuf::from(&market.repo));
        let text = match &local_root {
            Some(root) => std::fs::read_to_string(root.join(".claude-plugin/marketplace.json"))
                .map_err(|_| format!("{} no longer has a marketplace.json.", root.display()))?,
            None => std::fs::read_to_string(self.manifest_cache(&market.name)).map_err(|_| {
                format!("The manifest of {} isn't cached any more; remove and add it again.", market.name)
            })?,
        };
        let manifest: MarketplaceManifest = serde_json::from_str(&paste::without_comments(&text))
            .map_err(|error| format!("The manifest of {} couldn't be read: {error}", market.name))?;
        Ok((manifest, local_root))
    }

    fn marketplace_plugin(
        &self,
        market: &Marketplace,
        name: &str,
    ) -> Result<Option<(ResolvedSource, MarketplacePlugin)>, String> {
        let (manifest, local_root) = self.manifest_of(market)?;
        Ok(manifest
            .plugins
            .iter()
            .find(|plugin| plugin.name.eq_ignore_ascii_case(name))
            .map(|plugin| (resolve_marketplace_source(market, local_root.as_deref(), &plugin.source), plugin.clone())))
    }

    /// Everything a marketplace offers (`hermes` for the catalog), with what can't be installed marked.
    pub async fn browse(&self, name: &str, marketplaces: &[Marketplace]) -> Result<Vec<PluginListing>, String> {
        if name == HERMES_CATALOG {
            let catalog = self.catalog().await?;
            return Ok(catalog.entries.iter().map(catalog_listing).collect());
        }
        let market = marketplaces
            .iter()
            .find(|m| m.name == name)
            .ok_or_else(|| format!("You don't follow a marketplace called {name}."))?;
        let (manifest, local_root) = self.manifest_of(market)?;
        Ok(manifest
            .plugins
            .iter()
            .map(|plugin| {
                let resolved = resolve_marketplace_source(market, local_root.as_deref(), &plugin.source);
                PluginListing {
                    tap: format!("{}@{}", plugin.name, market.name),
                    name: plugin.name.clone(),
                    description: plugin.description.clone(),
                    label: plugin.category.clone().unwrap_or_else(|| market.name.clone()),
                    version: plugin.version.clone(),
                    unsupported: match resolved {
                        ResolvedSource::Unsupported(why) => Some(why),
                        _ => None,
                    },
                }
            })
            .collect())
    }

    /// Searches the Hermes catalog and every followed marketplace by name and description.
    pub async fn search(
        &self,
        query: &str,
        marketplaces: &[Marketplace],
        limit: usize,
    ) -> Result<Vec<PluginListing>, String> {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return Err("Type something to search for.".to_owned());
        }
        let words: Vec<&str> = query.split_whitespace().collect();
        let mut all: Vec<PluginListing> = Vec::new();
        let mut problems = Vec::new();
        match self.browse(HERMES_CATALOG, marketplaces).await {
            Ok(listings) => all.extend(listings),
            Err(error) => problems.push(error),
        }
        for market in marketplaces {
            match self.browse(&market.name, marketplaces).await {
                Ok(listings) => all.extend(listings),
                Err(error) => problems.push(error),
            }
        }
        if all.is_empty() && !problems.is_empty() {
            return Err(problems.join(" "));
        }
        let mut scored: Vec<(u8, PluginListing)> = all
            .into_iter()
            .filter_map(|listing| {
                let name = listing.name.to_lowercase();
                let description = listing.description.to_lowercase();
                let score = if name == query {
                    0
                } else if name.starts_with(&query) {
                    1
                } else if name.contains(&query) {
                    2
                } else if words.iter().all(|word| description.contains(word) || name.contains(word)) {
                    3
                } else {
                    return None;
                };
                Some((score, listing))
            })
            .collect();
        scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.name.cmp(&b.1.name)));
        Ok(scored.into_iter().take(limit.max(1)).map(|(_, listing)| listing).collect())
    }
}

fn catalog_listing(entry: &CatalogEntry) -> PluginListing {
    PluginListing {
        tap: format!("{}@{HERMES_CATALOG}", entry.name),
        name: entry.name.clone(),
        description: entry.description.clone(),
        label: entry.tier.clone().or_else(|| entry.category.clone()).unwrap_or_else(|| "community".to_owned()),
        version: entry.version.clone(),
        unsupported: (entry.sha.len() != 40).then(|| "no commit pin in the catalog".to_owned()),
    }
}

/// What Claude Code has installed, from `~/.claude/plugins/installed_plugins.json` (lenient: the file's
/// nesting is described, not specified).
#[must_use]
pub fn claude_installed(claude_plugins_dir: &Path) -> Vec<ClaudePluginInfo> {
    let Ok(text) = std::fs::read_to_string(claude_plugins_dir.join("installed_plugins.json")) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else { return Vec::new() };
    let Some(plugins) = value.get("plugins").and_then(|p| p.as_object()) else { return Vec::new() };
    let mut out = Vec::new();
    for (key, entry) in plugins {
        let records: Vec<&serde_json::Value> = match entry {
            serde_json::Value::Array(items) => items.iter().collect(),
            other => vec![other],
        };
        for record in records {
            let Some(path) = record.get("installPath").and_then(|p| p.as_str()) else { continue };
            out.push(ClaudePluginInfo {
                key: key.clone(),
                version: record.get("version").and_then(|v| v.as_str()).map(str::to_owned),
                install_path: path.to_owned(),
            });
        }
    }
    out.sort_by(|a, b| a.key.cmp(&b.key));
    out
}

// ---------------------------------------------------------------------------------------------
// Helpers

/// A folder-safe name: lowercase letters, digits, `-` and `_`.
#[must_use]
pub fn safe_name(raw: &str) -> String {
    let mut name: String = raw
        .trim()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c.to_ascii_lowercase() } else { '-' })
        .collect();
    while name.contains("--") {
        name = name.replace("--", "-");
    }
    let name = name.trim_matches('-').to_owned();
    if name.is_empty() { "plugin".to_owned() } else { name.chars().take(64).collect() }
}

fn strip_pin(tap: &str) -> String {
    match tap.rsplit_once('@') {
        Some((spec, pin)) if pin.len() == 40 && pin.chars().all(|c| c.is_ascii_hexdigit()) => spec.to_owned(),
        _ => tap.to_owned(),
    }
}

fn write_source(directory: &Path, source: &InstalledSource) -> Result<(), String> {
    let body = toml::to_string(source).map_err(|error| error.to_string())?;
    let text = format!("# Written by re-code when this plugin was installed. It's what `update` reads.\n{body}");
    std::fs::write(directory.join(SOURCE_FILE), text).map_err(|error| error.to_string())
}

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
                if relative != SOURCE_FILE && relative != HOOKS_CONVERTED {
                    files.push(relative);
                }
            }
        }
    }
    files.sort();
    files
}

fn changed_files(installed: &Path, staged: &Path) -> Vec<String> {
    let old: BTreeSet<String> = list_files(installed).into_iter().collect();
    let new: BTreeSet<String> = list_files(staged).into_iter().collect();
    let mut changed = Vec::new();
    for path in old.union(&new) {
        match (old.contains(path), new.contains(path)) {
            (true, false) => changed.push(format!("- {path}")),
            (false, true) => changed.push(format!("+ {path}")),
            _ => {
                if std::fs::read(installed.join(path)).ok() != std::fs::read(staged.join(path)).ok() {
                    changed.push(format!("~ {path}"));
                }
            }
        }
    }
    changed
}

/// Copies regular files and folders, never following links.
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
            check_relative(&rel_text)?;
            let Ok(metadata) = std::fs::symlink_metadata(&path) else { continue };
            if metadata.file_type().is_symlink() {
                skipped.push((rel_text, "is a symbolic link".to_owned()));
            } else if metadata.is_dir() {
                if entry.file_name() == ".git" {
                    continue;
                }
                stack.push(rel);
            } else if metadata.is_file() {
                count += 1;
                total += metadata.len();
                if count > caps.max_files || total > caps.max_total_bytes || metadata.len() > caps.max_file_bytes {
                    return Err("That folder is larger than the install limits.".to_owned());
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

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, relative: &str, text: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn plugin_taps_are_recognised() {
        assert_eq!(
            PluginTap::parse("better-session-appearance").unwrap(),
            PluginTap::Name { name: "better-session-appearance".into() }
        );
        assert_eq!(
            PluginTap::parse("context7@claude-plugins-official").unwrap(),
            PluginTap::FromMarketplace { name: "context7".into(), marketplace: "claude-plugins-official".into() }
        );
        assert_eq!(
            PluginTap::parse("0bull/agent-plugin").unwrap(),
            PluginTap::GitHub {
                owner: "0bull".into(),
                repo: "agent-plugin".into(),
                subdir: String::new(),
                reference: None
            }
        );
        assert_eq!(
            PluginTap::parse("https://github.com/apoapostolov/hermes-agent-awesome-plugins/tree/main/public/better-session-appearance").unwrap(),
            PluginTap::GitHub {
                owner: "apoapostolov".into(),
                repo: "hermes-agent-awesome-plugins".into(),
                subdir: "public/better-session-appearance".into(),
                reference: Some("main".into())
            }
        );
        assert_eq!(
            PluginTap::parse("~/plugins/mine").unwrap(),
            PluginTap::Local { path: expand_home("~/plugins/mine") }
        );
        assert!(PluginTap::parse("").is_err());
        assert!(PluginTap::parse("https://gitlab.com/a/b").is_err());
        assert_eq!(safe_name("WordPress.com Plugin"), "wordpress-com-plugin");
        assert_eq!(strip_pin("a/b@2c5958ecac7cee2b583967a2c4981d0f4a75c579"), "a/b");
    }

    #[test]
    fn a_claude_code_plugin_folder_is_read_part_by_part() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("frontend-design");
        write(
            &root,
            ".claude-plugin/plugin.json",
            r#"{"name":"frontend-design","description":"UI skill","version":"1.1.1","author":{"name":"Anthropic"}}"#,
        );
        write(
            &root,
            "skills/frontend-design/SKILL.md",
            "---\nname: frontend-design\ndescription: Build interfaces.\n---\nbody",
        );
        write(&root, "commands/review-ui.md", "---\ndescription: Review the UI.\n---\nLook at it.");
        write(
            &root,
            ".mcp.json",
            r#"{"mcpServers":{"ctx":{"type":"http","url":"https://mcp.example/mcp"},"local":{"command":"node","args":["${CLAUDE_PLUGIN_ROOT}/server.js"]}}}"#,
        );
        write(
            &root,
            "hooks/hooks.json",
            r#"{"hooks":{"PreToolUse":[{"matcher":"Edit|Write|MultiEdit","hooks":[{"type":"command","command":"bash \"${CLAUDE_PLUGIN_ROOT}/check.sh\"","timeout":20}]}],"SessionStart":[{"hooks":[{"type":"command","command":"echo hi"}]}],"UserPromptSubmit":[{"hooks":[{"type":"command","command":"x"}]}]}}"#,
        );
        write(&root, "agents/helper.md", "# helper");
        write(&root, ".lsp.json", "{}");
        let plugin = read_plugin(&root, &BTreeSet::new()).unwrap();
        assert_eq!(plugin.name, "frontend-design");
        assert_eq!(plugin.version.as_deref(), Some("1.1.1"));
        assert_eq!(plugin.maintainer.as_deref(), Some("Anthropic"));
        assert_eq!(plugin.label, "local");
        let described: Vec<String> = plugin.parts.iter().map(PluginPart::describe).collect();
        assert!(described.contains(&"skill frontend-design".to_owned()), "{described:?}");
        assert!(described.contains(&"command /review-ui (as a skill)".to_owned()), "{described:?}");
        assert!(described.iter().any(|d| d.starts_with("MCP server ctx · https://mcp.example/mcp")), "{described:?}");
        assert!(
            described
                .iter()
                .any(|d| d.starts_with("MCP server local · node ") && d.contains(&root.display().to_string())),
            "{described:?}"
        );
        assert!(described.contains(&"hook on pre_tool_use · converted, not loaded".to_owned()), "{described:?}");
        assert!(described.contains(&"hook on UserPromptSubmit · no equivalent here".to_owned()), "{described:?}");
        assert!(described.contains(&"agents · not run here (subagent definitions)".to_owned()), "{described:?}");
        assert!(described.iter().any(|d| d.starts_with("LSP server")), "{described:?}");
        assert!(
            plugin.capability_sentence().starts_with("brings 2 skills, 2 MCP servers, 3 hooks (not loaded), 2 parts"),
            "{}",
            plugin.capability_sentence()
        );
        assert_eq!(plugin.header(), "frontend-design [local] · maintained by Anthropic");

        let skills = skills_of(&root, "frontend-design", true, &BTreeSet::new());
        assert_eq!(skills.len(), 2);
        assert!(
            skills.iter().all(|s| s.source == SkillSource::Plugin
                && s.plugin.as_deref() == Some("frontend-design")
                && s.enabled)
        );
        let off = skills_of(&root, "frontend-design", false, &BTreeSet::new());
        assert!(off.iter().all(|s| !s.enabled));
        let servers = servers_of(&root, "frontend-design");
        assert_eq!(servers.len(), 2);
        assert_eq!(servers[0].external.as_ref().unwrap().label, "plugin frontend-design");

        let written = write_converted_hooks(&root, Path::new("/data/plugins/frontend-design")).unwrap();
        assert_eq!(written, 2);
        let text = std::fs::read_to_string(root.join(HOOKS_CONVERTED)).unwrap();
        assert!(
            text.contains("event = \"pre_tool_use\"") && text.contains("tools = [\"edit\", \"write\", \"edit\"]"),
            "{text}"
        );
        assert!(
            text.contains("/data/plugins/frontend-design/check.sh") && text.contains("timeout_secs = 20"),
            "{text}"
        );
        assert!(!text.contains("UserPromptSubmit"));
    }

    #[test]
    fn a_hermes_python_plugin_has_nothing_that_runs_here() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("better-session-appearance");
        write(
            &root,
            "plugin.yaml",
            "name: better-session-appearance\nversion: 1.2.4\ndescription: \"Colour each session.\"\nauthor: Apostol Apostolov\n",
        );
        write(&root, "__init__.py", "");
        write(&root, "desktop/plugin.js", "x");
        let plugin = read_plugin(&root, &BTreeSet::new()).unwrap();
        assert_eq!(plugin.version.as_deref(), Some("1.2.4"));
        assert_eq!(plugin.description, "Colour each session.");
        assert!(plugin.parts.iter().all(|part| !part.runs_here()));
        assert!(
            plugin.capability_sentence().ends_with("nothing here runs in re-code"),
            "{}",
            plugin.capability_sentence()
        );
        let empty = dir.path().join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        assert!(read_plugin(&empty, &BTreeSet::new()).is_none());
    }

    #[test]
    fn marketplace_sources_resolve_or_are_refused() {
        let market = Marketplace {
            name: "m".into(),
            repo: "anthropics/claude-plugins-official".into(),
            sha: "a".repeat(40),
            ..Default::default()
        };
        let resolve = |json: &str| resolve_marketplace_source(&market, None, &serde_json::from_str(json).unwrap());
        assert_eq!(
            resolve(r#""./plugins/frontend-design""#),
            ResolvedSource::GitHub {
                owner: "anthropics".into(),
                repo: "claude-plugins-official".into(),
                subdir: "plugins/frontend-design".into(),
                reference: Some("a".repeat(40))
            }
        );
        assert_eq!(
            resolve(&format!(
                r#"{{"source":"url","url":"https://github.com/Sales/adlc.git","sha":"{}"}}"#,
                "b".repeat(40)
            )),
            ResolvedSource::GitHub {
                owner: "Sales".into(),
                repo: "adlc".into(),
                subdir: String::new(),
                reference: Some("b".repeat(40))
            }
        );
        assert_eq!(
            resolve(r#"{"source":"git-subdir","url":"https://github.com/o/r","path":"plugins/x"}"#),
            ResolvedSource::GitHub { owner: "o".into(), repo: "r".into(), subdir: "plugins/x".into(), reference: None }
        );
        assert!(
            matches!(resolve(r#"{"source":"npm","package":"x"}"#), ResolvedSource::Unsupported(why) if why.contains("runs code"))
        );
        assert!(
            matches!(resolve(r#"{"source":"archive","url":"https://x/y.zip"}"#), ResolvedSource::Unsupported(why) if why.contains("zip"))
        );
        assert!(matches!(
            resolve(r#"{"source":"url","url":"https://gitlab.com/a/b"}"#),
            ResolvedSource::Unsupported(_)
        ));
        let local = Marketplace { name: "l".into(), repo: "/tmp/market".into(), ..Default::default() };
        assert_eq!(
            resolve_marketplace_source(&local, Some(Path::new("/tmp/market")), &serde_json::json!("./plugins/a")),
            ResolvedSource::Local(PathBuf::from("/tmp/market/plugins/a"))
        );
    }

    #[test]
    fn claude_codes_installed_plugins_are_listed_leniently() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "installed_plugins.json",
            r#"{"version":2,"plugins":{"rust-analyzer-lsp@claude-plugins-official":[{"scope":"user","installPath":"/home/x/.claude/plugins/cache/o/rust-analyzer-lsp/1.0.0","version":"1.0.0"}],"other@m":{"installPath":"/p"}}}"#,
        );
        let listed = claude_installed(dir.path());
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].key, "other@m");
        assert_eq!(listed[1].version.as_deref(), Some("1.0.0"));
        assert!(claude_installed(&dir.path().join("nothing")).is_empty());
    }
}
