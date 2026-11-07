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
    pub note: &'static str,
}

/// Built-in MCP servers offered as one-click presets. `@latest` keeps them up to date.
pub const MCP_PRESETS: &[McpPreset] = &[
    McpPreset {
        name: "rea",
        connection: "npx -y rea-agents@latest mcp",
        note: "Reverse-engineer binaries, apps and firmware (github.com/morluto/rea).",
    },
    McpPreset {
        name: "context7",
        connection: "npx -y @upstash/context7-mcp@latest",
        note: "Up-to-date library documentation for the model.",
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
    Ok(McpServer { name: name.to_owned(), transport, enabled, expose_to_agent: false, external: None, tools: None })
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
    /// Copy or clone a skill from a Git URL or local directory into the user skills.
    AddSkillFromLink(String),
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
