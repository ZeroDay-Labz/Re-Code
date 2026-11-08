//! MCP servers, skills and custom tools: their definitions and the rules for them. No I/O happens here.

use crate::providers::CloudProvider;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Names reserved for the built-in tools.
pub const RESERVED_TOOL_NAMES: &[&str] = &["read", "list", "grep", "edit", "write", "delete", "shell", "git", "todo"];

/// A one-click MCP server the user can add.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpPreset {
    pub name: &'static str,
    /// The command or address placed into the add form.
    pub connection: &'static str,
    /// Environment variables or headers placed into the add form (`KEY=value, OTHER=${VAR}`), to fill in.
    pub settings: &'static str,
    pub note: &'static str,
    /// What the user has to have or get first, in a few words; empty when nothing.
    pub needs: &'static str,
}

/// Built-in MCP servers offered as one-click presets. `@latest` keeps them up to date.
pub const MCP_PRESETS: &[McpPreset] = &[
    McpPreset {
        name: "rea",
        connection: "npx -y rea-agents@latest mcp",
        settings: "",
        note: "Reverse-engineer binaries, apps and firmware (github.com/morluto/rea).",
        needs: "Node.js",
    },
    McpPreset {
        name: "context7",
        connection: "npx -y @upstash/context7-mcp@latest",
        settings: "",
        note: "Up-to-date library documentation for the model.",
        needs: "Node.js",
    },
];

/// Web search and page reading for the model, as MCP servers: `/web` offers these. Package names and
/// variables checked against npm and PyPI on 2026-11-08. The address or key is the user's own and goes
/// into their `extensions.toml`, never into re-code.
pub const WEB_SEARCH_PRESETS: &[McpPreset] = &[
    McpPreset {
        name: "searxng",
        connection: "npx -y mcp-searxng@latest",
        settings: "SEARXNG_URL=http://127.0.0.1:8080",
        note: "Your own SearXNG instance: private, no key, any engines you've enabled there.",
        needs: "A running SearXNG (put its address in SEARXNG_URL) and Node.js",
    },
    McpPreset {
        name: "duckduckgo",
        connection: "uvx duckduckgo-mcp-server",
        settings: "",
        note: "DuckDuckGo results without an account.",
        needs: "uv (Python); no key",
    },
    McpPreset {
        name: "fetch",
        connection: "uvx mcp-server-fetch",
        settings: "",
        note: "Reads a web page as text for the model, with a start index for long pages. Pairs with any search.",
        needs: "uv (Python); no key",
    },
    McpPreset {
        name: "firecrawl",
        connection: "npx -y firecrawl-mcp@latest",
        settings: "FIRECRAWL_API_URL=http://127.0.0.1:3002, FIRECRAWL_API_KEY=${FIRECRAWL_API_KEY}",
        note: "Search, scrape, crawl and map the web. Your own Firecrawl (its API address) or Firecrawl cloud with a key.",
        needs: "A self-hosted Firecrawl (FIRECRAWL_API_URL; leave the key empty) or a key from firecrawl.dev, and Node.js",
    },
    McpPreset {
        name: "brave-search",
        connection: "npx -y @brave/brave-search-mcp-server@latest",
        settings: "BRAVE_API_KEY=${BRAVE_API_KEY}",
        note: "Brave's index: web, news, images; a free tier exists.",
        needs: "A key from brave.com/search/api (free tier) and Node.js",
    },
    McpPreset {
        name: "tavily",
        connection: "npx -y tavily-mcp@latest",
        settings: "TAVILY_API_KEY=${TAVILY_API_KEY}",
        note: "Search tuned for agents, with extracted page content.",
        needs: "A key from tavily.com (free tier) and Node.js",
    },
    McpPreset {
        name: "exa",
        connection: "npx -y exa-mcp-server@latest",
        settings: "EXA_API_KEY=${EXA_API_KEY}",
        note: "Neural search and research over the web.",
        needs: "A key from exa.ai and Node.js",
    },
];

/// How to reach an MCP server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum McpTransport {
    /// A local program, spoken to over stdin and stdout.
    Stdio {
        command: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        args: Vec<String>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        env: BTreeMap<String, String>,
    },
    /// A server reached over streamable HTTP.
    Http {
        url: String,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        headers: BTreeMap<String, String>,
    },
}

/// A configured MCP server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServer {
    pub name: String,
    #[serde(flatten)]
    pub transport: McpTransport,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Send this server's tool schemas to the model directly (vs discover-then-call). Best for small servers.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub expose_to_agent: bool,
    /// Set for a server read live from another tool's configuration; never saved in re-code's file.
    #[serde(skip)]
    pub external: Option<ExternalSource>,
    /// Which of the server's tools the model may see and call. `None` means all of them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolFilter>,
    /// How a stdio server's program is run: inside bubblewrap (the default) or on the host.
    #[serde(default, skip_serializing_if = "McpSandbox::is_default")]
    pub sandbox: McpSandbox,
}

/// Where a stdio MCP server's program runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum McpSandbox {
    /// Under bubblewrap with network: the project writable, the home folder emptied except toolchains and
    /// package caches, secrets hidden. Servers that need more can opt out.
    #[default]
    Bubblewrap,
    /// On the host, as the user, with the user's whole environment visible to it.
    None,
}

impl McpSandbox {
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::Bubblewrap
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Bubblewrap => "bubblewrap",
            Self::None => "none (host)",
        }
    }
}

/// A choice of tools from a server, by name or glob (`*` and `?`). `include` wins when both are set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ToolFilter {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
}

impl ToolFilter {
    /// Whether a tool passes the filter.
    #[must_use]
    pub fn allows(&self, tool: &str) -> bool {
        if !self.include.is_empty() {
            return self.include.iter().any(|pattern| glob_matches(pattern, tool));
        }
        !self.exclude.iter().any(|pattern| glob_matches(pattern, tool))
    }
}

/// `*` matches any run of characters, `?` one character; everything else is literal and case-sensitive.
#[must_use]
pub fn glob_matches(pattern: &str, text: &str) -> bool {
    fn go(p: &[char], t: &[char]) -> bool {
        match (p.first(), t.first()) {
            (None, None) => true,
            (Some('*'), _) => go(&p[1..], t) || (!t.is_empty() && go(p, &t[1..])),
            (Some('?'), Some(_)) => go(&p[1..], &t[1..]),
            (Some(a), Some(b)) if a == b => go(&p[1..], &t[1..]),
            _ => false,
        }
    }
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    go(&p, &t)
}

impl McpServer {
    /// Whether the model may see and call this tool of the server.
    #[must_use]
    pub fn allows_tool(&self, tool: &str) -> bool {
        self.tools.as_ref().is_none_or(|filter| filter.allows(tool))
    }
}

/// One result of searching the MCP registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpSearchHit {
    /// The registry's name, such as `io.github.owner/server`.
    pub name: String,
    pub description: String,
    pub version: Option<String>,
    pub repository: Option<String>,
    /// How re-code would run it, when a supported package or remote exists.
    pub server: Option<McpServer>,
    /// Why it can't be added, when it can't.
    pub unsupported: Option<String>,
}

/// Where an external server's definition lives.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExternalSource {
    /// For example "Claude Code" or "Claude Code · this project".
    pub label: String,
    /// Why re-code can't use it, if it can't (for example an SSE server).
    pub unsupported: Option<String>,
}

fn default_true() -> bool {
    true
}

impl McpServer {
    /// The text a person would type to define this server's connection.
    #[must_use]
    pub fn connection_text(&self) -> String {
        match &self.transport {
            McpTransport::Stdio { command, args, .. } => {
                std::iter::once(command).chain(args).map(|word| quote_word(word)).collect::<Vec<_>>().join(" ")
            }
            McpTransport::Http { url, .. } => url.clone(),
        }
    }

    /// Environment variables or headers as `KEY=value, KEY=value`, with real values.
    #[must_use]
    pub fn settings_text(&self) -> String {
        let map = match &self.transport {
            McpTransport::Stdio { env, .. } => env,
            McpTransport::Http { headers, .. } => headers,
        };
        map.iter().map(|(key, value)| format!("{key}={value}")).collect::<Vec<_>>().join(", ")
    }

    /// Environment variables or headers, safe to display: only `${VAR}` references show as written.
    #[must_use]
    pub fn masked_settings(&self) -> Vec<String> {
        let map = match &self.transport {
            McpTransport::Stdio { env, .. } => env,
            McpTransport::Http { headers, .. } => headers,
        };
        map.iter()
            .map(|(key, value)| if is_reference(value) { format!("{key}={value}") } else { format!("{key}=••••••") })
            .collect()
    }
}

/// True for a value like `${API_KEY}`, which is resolved from the environment when the server starts.
#[must_use]
pub fn is_reference(value: &str) -> bool {
    value
        .strip_prefix("${")
        .and_then(|rest| rest.strip_suffix('}'))
        .is_some_and(|name| !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
}

fn quote_word(word: &str) -> String {
    if !word.is_empty() && !word.contains(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == '\\') {
        return word.to_owned();
    }
    format!("\"{}\"", word.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Splits a command line into words, honouring quotes and backslashes. Nothing is ever given to a shell.
pub fn split_command(text: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut started = false;
    let mut quote = None;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"') | None, '\\') => match chars.next() {
                Some(next) => {
                    word.push(next);
                    started = true;
                }
                None => return Err("The command ends with a stray backslash.".to_owned()),
            },
            (Some(_), c) => word.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                started = true;
            }
            (None, c) if c.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            (None, c) => {
                word.push(c);
                started = true;
            }
        }
    }
    if quote.is_some() {
        return Err("The command has an unclosed quote.".to_owned());
    }
    if started {
        words.push(word);
    }
    Ok(words)
}

fn parse_settings(text: &str) -> Result<BTreeMap<String, String>, String> {
    let mut map = BTreeMap::new();
    for part in text.split(',').map(str::trim).filter(|part| !part.is_empty()) {
        let Some((key, value)) = part.split_once('=') else {
            return Err(format!("\"{part}\" needs to look like KEY=value."));
        };
        let key = key.trim();
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
            return Err(format!("\"{key}\" isn't a valid name. Use letters, numbers, - and _."));
        }
        map.insert(key.to_owned(), value.trim().to_owned());
    }
    Ok(map)
}

/// Checks a name for a server, tool or skill.
pub fn validate_name(kind: &str, name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err(format!("The {kind} needs a name."));
    }
    if name.len() > 64 {
        return Err(format!("The {kind} name is too long. The limit is 64 characters."));
    }
    let valid = name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') && !name.starts_with('-');
    if !valid {
        return Err(format!("The {kind} name can only use letters, numbers, - and _."));
    }
    Ok(())
}

/// Builds a server from what was typed: a URL means HTTP, and anything else is a command to run.
pub fn parse_mcp_form(name: &str, connection: &str, settings: &str, enabled: bool) -> Result<McpServer, String> {
    let name = name.trim();
    validate_name("server", name)?;
    let connection = connection.trim();
    if connection.is_empty() {
        return Err("Enter a command to run, or an http:// or https:// address.".to_owned());
    }
    let settings = parse_settings(settings)?;
    let transport = if connection.starts_with("http://") || connection.starts_with("https://") {
        McpTransport::Http { url: connection.to_owned(), headers: settings }
    } else {
        let mut words = split_command(connection)?.into_iter();
        let command = words.next().ok_or("Enter a command to run, or an http:// or https:// address.")?;
        McpTransport::Stdio { command, args: words.collect(), env: settings }
    };
    Ok(McpServer {
        name: name.to_owned(),
        transport,
        enabled,
        expose_to_agent: false,
        external: None,
        tools: None,
        sandbox: McpSandbox::default(),
    })
}

/// A command the agent will be able to run as a tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomTool {
    pub name: String,
    pub description: String,
    /// What to run. `{name}` is replaced by the parameter of that name, as a single argument.
    pub command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parameters: Vec<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Whether it may use the network when run in the sandbox. Off unless set in the config.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub network: bool,
}

/// Builds a tool from what was typed.
pub fn parse_tool_form(
    name: &str,
    description: &str,
    command: &str,
    parameters: &str,
    enabled: bool,
) -> Result<CustomTool, String> {
    let name = name.trim();
    validate_name("tool", name)?;
    if RESERVED_TOOL_NAMES.contains(&name) {
        return Err(format!("\"{name}\" is the name of a built-in tool."));
    }
    let description = description.trim();
    if description.is_empty() {
        return Err("Describe what the tool does. The model reads this to decide when to use it.".to_owned());
    }
    let command = command.trim();
    let words = split_command(command)?;
    if words.is_empty() {
        return Err("Enter the command this tool runs.".to_owned());
    }
    let parameters: Vec<String> =
        parameters.split(',').map(str::trim).filter(|part| !part.is_empty()).map(str::to_owned).collect();
    for parameter in &parameters {
        validate_name("parameter", parameter)?;
    }
    for parameter in &parameters {
        let placeholder = format!("{{{parameter}}}");
        if !words.iter().any(|word| word.contains(&placeholder)) {
            return Err(format!("The command never uses {placeholder}."));
        }
    }
    Ok(CustomTool {
        name: name.to_owned(),
        description: description.to_owned(),
        command: command.to_owned(),
        parameters,
        enabled,
        network: false,
    })
}

/// Where a skill came from, which decides what can be done with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SkillSource {
    /// In re-code's own directory, and editable here.
    #[default]
    User,
    /// From the current project. It stays off until project trust exists, as a repository could carry anything.
    Project,
    /// Found in Claude Code's directory. Read-only here.
    ClaudeCode,
    /// Part of an installed plugin (`Skill.plugin` names it). Removed with the plugin.
    Plugin,
}

/// What a static scan of a skill found (`recode_extensions::scan`). It can't prove a skill safe; it
/// catches patterns with no honest place in a coding assistant's instructions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case", tag = "verdict")]
pub enum Verdict {
    #[default]
    Clean,
    /// Worth reading before enabling; can be enabled.
    Caution { findings: Vec<Finding> },
    /// Never enabled.
    Dangerous { findings: Vec<Finding> },
}

impl Verdict {
    #[must_use]
    pub fn blocks(&self) -> bool {
        matches!(self, Self::Dangerous { .. })
    }

    #[must_use]
    pub fn findings(&self) -> &[Finding] {
        match self {
            Self::Clean => &[],
            Self::Caution { findings } | Self::Dangerous { findings } => findings,
        }
    }

    /// A word for a list row.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Caution { .. } => "caution",
            Self::Dangerous { .. } => "dangerous",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// The file within the skill, relative to its directory.
    pub file: String,
    /// 1-based; 0 when the finding is about the file itself.
    pub line: usize,
    /// What the pattern means, in plain words.
    pub reason: String,
    /// The matching text, trimmed.
    pub excerpt: String,
    pub severity: Severity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Caution,
    Dangerous,
}

/// A discovered skill, in the Agent Skills format (`SKILL.md` with front matter).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Skill {
    pub name: String,
    pub description: String,
    /// The `SKILL.md` file.
    pub path: String,
    /// The skill's directory, which its reference files are relative to.
    #[serde(default)]
    pub directory: String,
    pub source: SkillSource,
    pub enabled: bool,
    /// Size of the skill's instructions, estimated.
    pub estimated_tokens: u64,
    /// A problem with the file, if any.
    pub problem: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub compatibility: Option<String>,
    /// `allowed-tools` from the front matter (experimental in the specification; shown, not enforced yet).
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    pub scan: Verdict,
    /// For project skills: whether the folder's configuration has been trusted.
    #[serde(default)]
    pub trusted: bool,
    /// For skills installed from a hub: where from and which commit, so they can be updated.
    #[serde(default)]
    pub installed_from: Option<InstalledSource>,
    /// The plugin this skill belongs to, for `SkillSource::Plugin`.
    #[serde(default)]
    pub plugin: Option<String>,
}

/// One thing a plugin brings. Skills and MCP servers run here; the rest is listed so the user knows
/// what the plugin would have done elsewhere.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum PluginPart {
    /// A skill folder (`skills/<name>/SKILL.md`) or a Claude Code command file (`commands/<name>.md`).
    Skill { name: String, path: String, scan: Verdict, command: bool },
    /// An MCP server from `mcp.json` or `.mcp.json`.
    Mcp { name: String, connection: String },
    /// A hook that was translated into re-code's format but is not loaded until the user copies it.
    Hook { event: String, command: String, converted: bool },
    /// Something re-code has no way to run: Python tools, desktop plugins, agents, LSP servers…
    NotRunHere { what: String, detail: String },
}

impl PluginPart {
    /// One line for a list.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Skill { name, command: false, scan, .. } => match scan {
                Verdict::Clean => format!("skill {name}"),
                other => format!("skill {name} · {}", other.label()),
            },
            Self::Skill { name, command: true, .. } => format!("command /{name} (as a skill)"),
            Self::Mcp { name, connection } => format!("MCP server {name} · {connection}"),
            Self::Hook { event, converted: true, .. } => format!("hook on {event} · converted, not loaded"),
            Self::Hook { event, converted: false, .. } => format!("hook on {event} · no equivalent here"),
            Self::NotRunHere { what, detail } if detail.is_empty() => format!("{what} · not run here"),
            Self::NotRunHere { what, detail } => format!("{what} · not run here ({detail})"),
        }
    }

    /// Whether re-code puts this part to use.
    #[must_use]
    pub fn runs_here(&self) -> bool {
        matches!(self, Self::Skill { .. } | Self::Mcp { .. })
    }
}

/// An installed plugin: a folder under re-code's data directory with a source record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Plugin {
    pub name: String,
    pub description: String,
    pub version: Option<String>,
    pub maintainer: Option<String>,
    /// `official`, `verified`, `community` (Hermes tiers), a marketplace name, `github` or `local`.
    pub label: String,
    pub source: InstalledSource,
    pub directory: String,
    pub enabled: bool,
    pub parts: Vec<PluginPart>,
}

impl Plugin {
    /// Hermes' header line: `name [tier] pinned @ sha8 · maintained by …`.
    #[must_use]
    pub fn header(&self) -> String {
        let mut text = format!("{} [{}]", self.name, self.label);
        if self.source.sha.len() >= 8 {
            text.push_str(&format!(" pinned @ {}", &self.source.sha[..8]));
        }
        if let Some(maintainer) = &self.maintainer {
            text.push_str(&format!(" · maintained by {maintainer}"));
        }
        text
    }

    /// Hermes' capability sentence, from what the files hold.
    #[must_use]
    pub fn capability_sentence(&self) -> String {
        let count = |pred: &dyn Fn(&PluginPart) -> bool| self.parts.iter().filter(|part| pred(part)).count();
        let skills = count(&|part| matches!(part, PluginPart::Skill { .. }));
        let servers = count(&|part| matches!(part, PluginPart::Mcp { .. }));
        let hooks = count(&|part| matches!(part, PluginPart::Hook { .. }));
        let other = count(&|part| matches!(part, PluginPart::NotRunHere { .. }));
        if skills + servers + hooks + other == 0 {
            return "declares no tools, hooks, middleware, or env vars; nothing here runs in re-code".to_owned();
        }
        let mut parts = Vec::new();
        if skills > 0 {
            parts.push(if skills == 1 { "1 skill".to_owned() } else { format!("{skills} skills") });
        }
        if servers > 0 {
            parts.push(if servers == 1 { "1 MCP server".to_owned() } else { format!("{servers} MCP servers") });
        }
        if hooks > 0 {
            parts.push(if hooks == 1 {
                "1 hook (not loaded)".to_owned()
            } else {
                format!("{hooks} hooks (not loaded)")
            });
        }
        if other > 0 {
            parts.push(if other == 1 {
                "1 part that doesn't run here".to_owned()
            } else {
                format!("{other} parts that don't run here")
            });
        }
        if skills + servers == 0 {
            format!("{}; nothing here runs in re-code", parts.join(", "))
        } else {
            format!("brings {}", parts.join(", "))
        }
    }
}

/// A plugin fetched into staging and examined, waiting for the decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PluginPreview {
    pub staging: String,
    pub plugin: Plugin,
    pub files: usize,
    pub bytes: u64,
    /// Where it will live.
    pub location: String,
    pub note: Option<String>,
    /// Set when a plugin of this name is installed: an update, with the files that change.
    pub replaces: Option<Replacing>,
}

/// What staging a plugin came to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PluginOutcome {
    Preview(Box<PluginPreview>),
    UpToDate { name: String, sha: String },
    Failed { tap: String, error: String },
}

/// One plugin offered by a marketplace or catalog, and how re-code would install it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PluginListing {
    /// What to stage to install it.
    pub tap: String,
    pub name: String,
    pub description: String,
    /// Hermes tier, or the marketplace's category.
    pub label: String,
    pub version: Option<String>,
    /// Why it can't be installed, when it can't (an `npm` or `command` source runs code on install).
    pub unsupported: Option<String>,
    /// The catalog's category (Hermes: tools, desktop, models, memory, general, automation, web, platform, voice).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
}

/// A Claude Code marketplace re-code follows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Marketplace {
    pub name: String,
    /// `owner/repo` on GitHub, or a local folder.
    pub repo: String,
    /// The commit the manifest was read at (empty for a local folder).
    #[serde(default)]
    pub sha: String,
    #[serde(default)]
    pub added_at: String,
    /// How many plugins its manifest lists, when it has been read.
    #[serde(default)]
    pub plugin_count: usize,
}

/// A plugin Claude Code itself has installed, listed read-only so it can be brought over.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ClaudePluginInfo {
    /// `name@marketplace` as Claude Code keys it.
    pub key: String,
    pub version: Option<String>,
    /// The folder with the files, which `install` can take as a local tap.
    pub install_path: String,
}

/// Where an installed skill came from, as recorded in `.re-code-source.toml` inside its folder.
/// It lets re-code check for and install an update, and show the pinned commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct InstalledSource {
    /// What the user gave, such as `official/security/godmode` or `anthropics/skills/skills/pdf`.
    pub tap: String,
    /// `owner/repo` on GitHub, or the host for a single-file install.
    pub repo: String,
    /// The folder inside the repository.
    #[serde(default)]
    pub path: String,
    /// The commit the files were taken from (40 hex characters); empty for a local copy.
    #[serde(default)]
    pub sha: String,
    /// `official`, `community`, `skills.sh`, `github`, `file` or `local`.
    #[serde(default)]
    pub label: String,
    /// When it was installed, UTC, like `2026-11-07T19:00:00Z`.
    #[serde(default)]
    pub installed_at: String,
}

impl InstalledSource {
    /// `official @ 4134cbef`, or `local` when nothing is pinned.
    #[must_use]
    pub fn pin_text(&self) -> String {
        if self.sha.len() >= 8 { format!("{} @ {}", self.label, &self.sha[..8]) } else { self.label.clone() }
    }
}

/// What a skill's files declare they need or do, read before installing. The wording follows Hermes
/// Agent's install summary so people who know it recognise it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Declares {
    /// `allowed-tools` from the front matter.
    pub allowed_tools: Vec<String>,
    /// Scripts shipped with the skill (anything runnable, or under `scripts/`).
    pub scripts: Vec<String>,
    /// Environment variables the instructions or scripts refer to.
    pub env_vars: Vec<String>,
}

impl Declares {
    /// One sentence: "declares no tools, hooks, middleware, or env vars", or what it does declare.
    #[must_use]
    pub fn summary(&self) -> String {
        if self.allowed_tools.is_empty() && self.scripts.is_empty() && self.env_vars.is_empty() {
            return "declares no tools, hooks, middleware, or env vars".to_owned();
        }
        let mut parts = Vec::new();
        if !self.allowed_tools.is_empty() {
            parts.push(format!("asks for tools: {}", self.allowed_tools.join(" ")));
        }
        if !self.scripts.is_empty() {
            parts.push(match self.scripts.len() {
                1 => format!("ships a script ({})", self.scripts[0]),
                n => format!("ships {n} scripts"),
            });
        }
        if !self.env_vars.is_empty() {
            parts.push(format!("uses env vars: {}", self.env_vars.join(", ")));
        }
        format!("declares no hooks or middleware; {}", parts.join("; "))
    }
}

/// What installing an update would change, file by file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Replacing {
    /// The commit installed now.
    pub sha: String,
    /// Files added, removed or changed (`+ path`, `- path`, `~ path`).
    pub changed: Vec<String>,
}

/// A skill fetched into a staging folder and examined, waiting for the user's decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct InstallPreview {
    /// The staged skill folder (under re-code's cache). Committing moves it into place; discarding removes it.
    pub staging: String,
    pub name: String,
    pub description: String,
    pub source: InstalledSource,
    /// Every file, relative to the skill folder.
    pub files: Vec<String>,
    pub bytes: u64,
    /// What `SKILL.md` costs when the model loads it, estimated.
    pub estimated_tokens: u64,
    pub declares: Declares,
    pub scan: Verdict,
    /// Where it will live once installed.
    pub location: String,
    /// `SKILL.md` as fetched, for reading before deciding (cut at 64k characters).
    pub skill_text: String,
    /// A specification problem that would keep it off (no description, a bad name).
    pub problem: Option<String>,
    /// Something to know, such as "skills.sh's API needs a login, so the files came from GitHub".
    pub note: Option<String>,
    /// Set when a skill of this name is installed already: this is an update.
    pub replaces: Option<Replacing>,
}

/// One skill folder found where several could be meant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SkillCandidate {
    /// What to stage to get this one.
    pub tap: String,
    /// The folder inside the repository.
    pub path: String,
    pub name: String,
    pub description: String,
}

/// What staging a skill came to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InstallOutcome {
    Preview(Box<InstallPreview>),
    /// The place holds several skills: pick one.
    Choose {
        tap: String,
        candidates: Vec<SkillCandidate>,
        note: Option<String>,
    },
    /// An update found nothing newer than what's installed.
    UpToDate {
        name: String,
        sha: String,
    },
    Failed {
        tap: String,
        error: String,
    },
}

/// One result of searching a skill hub.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SkillSearchHit {
    /// What to stage to install it.
    pub tap: String,
    pub name: String,
    pub description: String,
    /// `official`, `community`, …
    pub label: String,
    pub repo: String,
    pub path: String,
    /// For Hermes' own skills, the folder they're filed under (security, documents, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
}

/// One tool offered by an MCP server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpToolInfo {
    pub name: String,
    pub description: String,
    /// The JSON schema of the tool's arguments, as the server gave it.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub input_schema: serde_json::Value,
}

impl McpToolInfo {
    /// A compact call signature from the schema, like `search_text(query: string, path?: string)`.
    #[must_use]
    pub fn signature(&self) -> String {
        let properties = self.input_schema.get("properties").and_then(serde_json::Value::as_object);
        let required: Vec<&str> = self
            .input_schema
            .get("required")
            .and_then(serde_json::Value::as_array)
            .map(|items| items.iter().filter_map(serde_json::Value::as_str).collect())
            .unwrap_or_default();
        let arguments: Vec<String> = properties
            .map(|properties| {
                properties
                    .iter()
                    .map(|(name, schema)| {
                        let kind = schema.get("type").and_then(serde_json::Value::as_str).unwrap_or("any");
                        let optional = if required.contains(&name.as_str()) { "" } else { "?" };
                        format!("{name}{optional}: {kind}")
                    })
                    .collect()
            })
            .unwrap_or_default();
        format!("{}({})", self.name, arguments.join(", "))
    }
}

/// The result of reaching a cloud provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "result")]
pub enum ProviderProbe {
    Reachable {
        model_count: usize,
        /// Free models that can be used from here (a provider's client-only ones aren't counted).
        free_model_count: usize,
        /// What else the provider said: a key's standing, or why some models are hidden.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    Failed {
        error: String,
    },
    /// No API key is stored, and the provider needs one.
    NoKey,
}

/// The result of connecting to a server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "result")]
pub enum McpProbe {
    Connected {
        server_name: Option<String>,
        server_version: Option<String>,
        tools: Vec<McpToolInfo>,
        /// What sending every tool's schema to a model would cost, estimated.
        estimated_schema_tokens: u64,
    },
    Failed {
        error: String,
    },
}

/// Everything configured.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtensionsSnapshot {
    pub servers: Vec<McpServer>,
    pub probes: BTreeMap<String, McpProbe>,
    pub probing: Vec<String>,
    pub tools: Vec<CustomTool>,
    pub skills: Vec<Skill>,
    pub plugins: Vec<Plugin>,
    pub marketplaces: Vec<Marketplace>,
    /// What Claude Code has installed, read live and read-only.
    pub claude_plugins: Vec<ClaudePluginInfo>,
    pub providers: Vec<CloudProvider>,
    pub provider_probes: BTreeMap<String, ProviderProbe>,
    /// Providers that have an API key stored in the keyring.
    pub providers_with_keys: BTreeSet<String>,
    pub config_path: String,
    /// The file couldn't be read. Saving is refused so that a typo can't cost someone their configuration.
    pub load_error: Option<String>,
}

/// A change requested from the management screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtensionCommand {
    SaveServer {
        original: Option<String>,
        server: McpServer,
    },
    DeleteServer(String),
    SetServerEnabled(String, bool),
    /// Toggle whether this server's tool schemas go to the model directly (vs discover-then-call).
    SetServerExposed(String, bool),
    TestServer(String),
    SaveTool {
        original: Option<String>,
        tool: CustomTool,
    },
    DeleteTool(String),
    SetToolEnabled(String, bool),
    SaveProvider {
        original: Option<String>,
        provider: CloudProvider,
        key: Option<String>,
    },
    DeleteProvider(String),
    SetProviderEnabled(String, bool),
    TestProvider(String),
    /// Add MCP servers that are configured in Claude Code but not here.
    ImportClaudeMcp,
    /// Add the MCP servers and providers from OpenCode's configuration (keys are never copied).
    ImportOpenCode,
    /// Search the MCP registry for servers matching a query.
    SearchMcp(String),
    /// Choose which of a server's tools the model may see and call (`None` means all).
    SetToolFilter {
        server: String,
        tools: Option<ToolFilter>,
    },
    /// Resolve a link (http MCP URL, a server.json URL, a GitHub repo, or a local path) into a server.
    AddMcpFromLink {
        name: Option<String>,
        link: String,
    },
    /// Fetch a skill from a hub, GitHub, skills.sh, a `SKILL.md` link or a local folder into a staging
    /// folder and examine it. The answer is a `InstallOutcome`; nothing is installed yet.
    StageSkill(String),
    /// Move a staged skill into place, on or off.
    CommitInstall {
        staging: String,
        enable: bool,
    },
    /// Remove a staged skill.
    DiscardInstall(String),
    /// Stage the newest version of an installed skill, showing what would change.
    UpdateSkill(String),
    /// Browse the Hermes skills index: a source to stay within (`official`, `github`, `clawhub`, …, or
    /// none for all of them) and words to narrow by (empty lists the source). Answered live as typed.
    BrowseSkills {
        source: Option<String>,
        query: String,
    },
    /// Stage several skills at once (pasted links, ticked search hits); the answer is one `InstallBatch`.
    StageSkillBatch(Vec<String>),
    /// Stage a skill from pasted `SKILL.md` text.
    StageSkillText(String),
    /// Move several staged skills into place, each on or off.
    CommitInstallBatch(Vec<(String, bool)>),
    /// Stage several plugins at once.
    StagePluginBatch(Vec<String>),
    /// Move several staged plugins into place.
    CommitPluginBatch(Vec<(String, bool)>),
    /// Fetch a plugin (Hermes catalog name, `name@marketplace`, GitHub, or a folder) into staging and examine it.
    StagePlugin(String),
    /// Move a staged plugin into place, on or off.
    CommitPlugin {
        staging: String,
        enable: bool,
    },
    SetPluginEnabled(String, bool),
    /// Remove an installed plugin and everything it brought.
    DeletePlugin(String),
    /// Stage the newest version of an installed plugin.
    UpdatePlugin(String),
    /// Follow a Claude Code marketplace (`owner/repo[@ref]`, a GitHub link, or a folder).
    AddMarketplace(String),
    RemoveMarketplace(String),
    /// List what a marketplace offers (`hermes` for the Hermes catalog).
    BrowseMarketplace(String),
    /// Search the Hermes plugin catalog and the followed marketplaces.
    SearchPlugins(String),
    CreateSkill {
        name: String,
        description: String,
    },
    DeleteSkill(String),
    SetSkillEnabled(String, bool),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_url_becomes_http_and_anything_else_a_command() {
        let server = parse_mcp_form("a", "http://127.0.0.1:64522/stream", "Authorization=${TOKEN}", true).unwrap();
        assert!(matches!(server.transport, McpTransport::Http { .. }));
        let server = parse_mcp_form("b", "npx -y \"my server\" --flag", "KEY=1, OTHER=2", true).unwrap();
        let McpTransport::Stdio { command, args, env } = server.transport else { panic!("wrong transport") };
        assert_eq!(command, "npx");
        assert_eq!(args, ["-y", "my server", "--flag"]);
        assert_eq!(env.len(), 2);
    }

    #[test]
    fn mistakes_get_a_useful_message() {
        assert!(parse_mcp_form("", "x", "", true).unwrap_err().contains("needs a name"));
        assert!(parse_mcp_form("bad name", "x", "", true).unwrap_err().contains("letters"));
        assert!(parse_mcp_form("a", "  ", "", true).unwrap_err().contains("command"));
        assert!(parse_mcp_form("a", "x", "nope", true).unwrap_err().contains("KEY=value"));
        assert!(parse_mcp_form("a", "x \"open", "", true).unwrap_err().contains("unclosed"));
        let secure = parse_mcp_form("a", "https://x.example/mcp", "", true).unwrap();
        assert!(matches!(secure.transport, McpTransport::Http { url, .. } if url.starts_with("https://")));
    }

    #[test]
    fn a_command_survives_being_shown_and_read_back() {
        let original = parse_mcp_form("a", r#"run "a b" 'c"d' e\ f"#, "", true).unwrap();
        let back = parse_mcp_form("a", &original.connection_text(), "", true).unwrap();
        assert_eq!(original, back);
    }

    #[test]
    fn secrets_are_masked_but_references_are_shown() {
        let server = parse_mcp_form("a", "x", "SECRET=hunter2, TOKEN=${MY_TOKEN}", true).unwrap();
        let shown = server.masked_settings().join(" ");
        assert!(!shown.contains("hunter2"));
        assert!(shown.contains("TOKEN=${MY_TOKEN}"));
    }

    #[test]
    fn names_cannot_escape_a_directory() {
        for name in ["../evil", "a/b", ".hidden", "-flag", "a b"] {
            assert!(validate_name("skill", name).is_err(), "{name}");
        }
        assert!(validate_name("skill", "good_name-1").is_ok());
    }

    #[test]
    fn tools_are_checked() {
        let good = parse_tool_form("search", "Search the docs", "rg {query} docs", "query", true).unwrap();
        assert_eq!(good.parameters, ["query"]);
        assert!(parse_tool_form("shell", "d", "x", "", true).unwrap_err().contains("built-in"));
        assert!(parse_tool_form("t", "", "x", "", true).unwrap_err().contains("Describe"));
        assert!(parse_tool_form("t", "d", "", "", true).unwrap_err().contains("command"));
        assert!(parse_tool_form("t", "d", "x", "q", true).unwrap_err().contains("{q}"));
    }

    #[test]
    fn stored_form_is_readable_and_round_trips() {
        let server = parse_mcp_form("a", "run --x", "K=v", false).unwrap();
        let text = serde_json::to_string(&server).unwrap();
        assert!(text.contains(r#""type":"stdio""#));
        assert_eq!(server, serde_json::from_str(&text).unwrap());
    }
}
