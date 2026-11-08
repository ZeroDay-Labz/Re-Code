//! Reading OpenCode's configuration (`opencode.json` or `opencode.jsonc`, global and per project) so
//! its MCP servers and providers can be brought over. Keys are never copied: `{env:VAR}` becomes a
//! `${VAR}` reference, and a literal key is reported, not stored. Agents, modes, instruction files
//! and JavaScript plugins are listed as things re-code doesn't run.
//!
//! Shape as read on this machine on 2026-11-08 (OpenCode 1.x): top-level `provider`, `mcp`,
//! `instructions`, `agent`/`mode`, `plugin`, `permission`; `mcp.<name>` is `{type: local, command: [...],
//! environment: {...}, enabled}` or `{type: remote, url, headers}`; `provider.<name>` is `{npm, name,
//! options: {baseURL, apiKey}, models: {...}}`.

use crate::paste;
use recode_core::extensions::{ExternalSource, McpServer, validate_name};
use recode_core::providers::{CloudProvider, ProviderKind};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Where OpenCode keeps its global configuration, relative to the config directory.
pub const GLOBAL_FILES: &[&str] = &["opencode/opencode.json", "opencode/opencode.jsonc"];
/// Per project.
pub const PROJECT_FILES: &[&str] = &["opencode.json", "opencode.jsonc"];

/// What OpenCode's configuration holds, mapped to re-code's terms.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Import {
    /// Files that were read.
    pub files: Vec<PathBuf>,
    pub servers: Vec<McpServer>,
    pub providers: Vec<CloudProvider>,
    /// Instruction files OpenCode would load (`instructions: [...]`), not loaded here.
    pub instructions: Vec<String>,
    /// Things re-code has no equivalent for: agents, modes, JavaScript plugins, with one line each.
    pub not_run: Vec<String>,
    /// Things the user should know: a literal key that wasn't copied, a `{file:…}` reference, an SSE server.
    pub warnings: Vec<String>,
    /// A project file was found but the folder isn't trusted, so it wasn't read.
    pub held_back: Option<PathBuf>,
}

impl Import {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.servers.is_empty() && self.providers.is_empty() && self.instructions.is_empty() && self.not_run.is_empty()
    }
}

/// Reads the global file under `config_dir` and, when the project is trusted, the project's own.
#[must_use]
pub fn read(config_dir: Option<&Path>, project: Option<&Path>, project_trusted: bool) -> Import {
    let mut import = Import::default();
    if let Some(config_dir) = config_dir {
        for relative in GLOBAL_FILES {
            let path = config_dir.join(relative);
            if path.is_file() {
                read_file(&path, "OpenCode", &mut import);
                break;
            }
        }
    }
    if let Some(project) = project {
        for relative in PROJECT_FILES {
            let path = project.join(relative);
            if path.is_file() {
                if project_trusted {
                    read_file(&path, "OpenCode · this project", &mut import);
                } else {
                    import.held_back = Some(path);
                }
                break;
            }
        }
    }
    import
}

/// Reads one configuration file into the import. A file that can't be parsed is a warning.
pub fn read_file(path: &Path, label: &str, import: &mut Import) {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) => {
            import.warnings.push(format!("{} couldn't be read: {error}", path.display()));
            return;
        }
    };
    match parse(&text, label) {
        Ok(parsed) => {
            import.files.push(path.to_path_buf());
            import.servers.extend(parsed.servers);
            import.providers.extend(parsed.providers);
            import.instructions.extend(parsed.instructions);
            import.not_run.extend(parsed.not_run);
            import.warnings.extend(parsed.warnings);
        }
        Err(error) => import.warnings.push(format!("{}: {error}", path.display())),
    }
}

/// Parses the JSON(C) text of one file. `label` names the source on each server.
pub fn parse(text: &str, label: &str) -> Result<Import, String> {
    let value: Value = serde_json::from_str(&paste::without_comments(text)).map_err(|error| {
        format!("isn't valid JSON (OpenCode allows comments and trailing commas; both were removed): {error}")
    })?;
    let object = value.as_object().ok_or("the file isn't a JSON object")?;
    let mut import = Import::default();

    // MCP servers: the current `mcp` key, or the older `mcpServers`.
    if let Some(servers) = object.get("mcp").or_else(|| object.get("mcpServers")).and_then(Value::as_object) {
        for (name, entry) in servers {
            let Some(entry_object) = entry.as_object() else { continue };
            let enabled = entry_object.get("enabled").and_then(Value::as_bool).unwrap_or(true);
            let kind = entry_object.get("type").and_then(Value::as_str).unwrap_or("local");
            if kind == "remote" && entry_object.get("oauth").is_some_and(|o| !o.is_null() && o != &Value::Bool(false)) {
                import.not_run.push(format!("MCP server {name}: logs in with OAuth, which re-code can't do yet"));
                continue;
            }
            // paste::parse understands OpenCode's entries (command arrays, `environment`, `url`, `headers`).
            let single = serde_json::json!({ "mcpServers": { name: entry } });
            match paste::parse(None, &single.to_string()) {
                Ok(mut parsed) => {
                    for server in &mut parsed {
                        server.enabled = enabled;
                        server.external = Some(ExternalSource { label: label.to_owned(), unsupported: None });
                        rewrite_references(server, &mut import.warnings, name);
                    }
                    import.servers.extend(parsed);
                }
                Err(error) => import.warnings.push(format!("MCP server {name}: {error}")),
            }
        }
    }

    // Providers: `provider.<name>.options.baseURL`, with the SDK package telling the dialect.
    if let Some(providers) = object.get("provider").or_else(|| object.get("providers")).and_then(Value::as_object) {
        for (name, entry) in providers {
            let Some(entry_object) = entry.as_object() else { continue };
            let options = entry_object.get("options").and_then(Value::as_object);
            let base_url = options.and_then(|o| o.get("baseURL").or_else(|| o.get("baseUrl"))).and_then(Value::as_str);
            let npm = entry_object.get("npm").and_then(Value::as_str).unwrap_or_default();
            let kind = match (npm, name.as_str()) {
                (n, _) if n.contains("anthropic") => ProviderKind::Anthropic,
                (_, "anthropic") => ProviderKind::Anthropic,
                (n, _) if n.contains("google") => ProviderKind::Gemini,
                (_, "google") => ProviderKind::Gemini,
                ("@ai-sdk/openai", _) => ProviderKind::Openai,
                (_, "openai") => ProviderKind::Openai,
                _ => ProviderKind::OpenaiCompatible,
            };
            if kind == ProviderKind::OpenaiCompatible && base_url.is_none() {
                // Nothing to connect to: OpenCode knows this provider by its own built-in list.
                import.not_run.push(format!("provider {name}: OpenCode reaches it through its built-in list; add it on the Providers tab with a preset"));
                continue;
            }
            let safe_name = safe_name(name);
            if validate_name("provider", &safe_name).is_err() {
                import.warnings.push(format!("provider {name}: its name can't be used here"));
                continue;
            }
            if let Some(key) = options.and_then(|o| o.get("apiKey")).and_then(Value::as_str) {
                if let Some(variable) = env_reference(key) {
                    import.warnings.push(format!(
                        "provider {name}: its key is read from ${variable}; re-code reads the same variable when the Providers tab's key field is left empty, or store the key with k"
                    ));
                } else if key.starts_with("{file:") {
                    import.warnings.push(format!("provider {name}: its key comes from a file; paste it on the Providers tab (k) so it lives in the keyring"));
                } else if !key.trim().is_empty() {
                    import.warnings.push(format!(
                        "provider {name}: opencode.json holds its key in plain text; it wasn't copied. Paste it on the Providers tab (k) so it lives in the keyring"
                    ));
                }
            }
            import.providers.push(CloudProvider {
                name: safe_name,
                kind,
                base_url: base_url.map(str::to_owned),
                enabled: true,
            });
        }
    }

    if let Some(instructions) = object.get("instructions").and_then(Value::as_array) {
        import.instructions.extend(instructions.iter().filter_map(Value::as_str).map(str::to_owned));
    }
    for key in ["agent", "mode"] {
        if let Some(entries) = object.get(key).and_then(Value::as_object) {
            for name in entries.keys() {
                import.not_run.push(format!(
                    "{key} {name}: OpenCode agents and modes have no equivalent here (a skill can hold the same prompt)"
                ));
            }
        }
    }
    if let Some(plugins) = object.get("plugin").and_then(Value::as_array) {
        for plugin in plugins.iter().filter_map(Value::as_str) {
            import.not_run.push(format!("plugin {plugin}: a JavaScript plugin for OpenCode, not run here"));
        }
    }
    Ok(import)
}

/// `{env:VAR}` inside an OpenCode value.
fn env_reference(value: &str) -> Option<&str> {
    value.trim().strip_prefix("{env:").and_then(|rest| rest.strip_suffix('}')).filter(|v| !v.is_empty())
}

/// OpenCode spells references `{env:VAR}` and `{file:path}`; re-code reads `${VAR}` from the
/// environment and has no file references, so those become warnings.
fn rewrite_references(server: &mut McpServer, warnings: &mut Vec<String>, name: &str) {
    let fix = |value: &mut String, warnings: &mut Vec<String>, what: &str| {
        // `{env:VAR}` may be the whole value or part of one ("Bearer {env:KEY}").
        while let Some(start) = value.find("{env:") {
            let Some(end) = value[start..].find('}') else { break };
            let variable = value[start + 5..start + end].trim().to_owned();
            value.replace_range(start..start + end + 1, &format!("${{{variable}}}"));
        }
        if value.contains("{file:") {
            warnings.push(format!(
                "MCP server {name}: {what} is read from a file in OpenCode, which re-code doesn't do; set it by hand"
            ));
        }
    };
    match &mut server.transport {
        recode_core::extensions::McpTransport::Stdio { env, args, .. } => {
            for (key, value) in env.iter_mut() {
                fix(value, warnings, &format!("the variable {key}"));
            }
            for arg in args.iter_mut() {
                fix(arg, warnings, "an argument");
            }
        }
        recode_core::extensions::McpTransport::Http { headers, url } => {
            for (key, value) in headers.iter_mut() {
                fix(value, warnings, &format!("the header {key}"));
            }
            fix(url, warnings, "the address");
        }
    }
}

fn safe_name(raw: &str) -> String {
    raw.trim()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use recode_core::extensions::McpTransport;

    // The shape of the developer's own opencode.json on 2026-11-08, with the key redacted.
    const CURRENT: &str = r#"{
      "$schema": "https://opencode.ai/config.json",
      "model": "ollama/gpt-oss:20b",
      "provider": {
        "ollama": {
          "npm": "@ai-sdk/openai-compatible",
          "name": "Local Ollama",
          "options": { "baseURL": "http://127.0.0.1:11434/v1", "timeout": 1200000 },
          "models": { "gpt-oss:20b": { "name": "gpt-oss 20B", "limit": { "context": 32768, "output": 8192 } } }
        },
        "anthropic": { "options": { "apiKey": "{env:ANTHROPIC_API_KEY}" } },
        "openrouter": { "npm": "@openrouter/ai-sdk-provider", "options": { "apiKey": "sk-or-literal" } }
      },
      "mcp": {
        "rea": {
          "type": "local",
          "command": ["npx", "-y", "rea-agents@6.8.0", "mcp"],
          "enabled": false,
          "environment": { "JAVA_HOME": "/usr/lib/jvm/java-25-openjdk", "TOKEN": "{env:REA_TOKEN}" }
        },
        "docs": { "type": "remote", "url": "https://mcp.example/mcp", "headers": { "Authorization": "Bearer {env:DOCS_KEY}" } },
        "sso": { "type": "remote", "url": "https://sso.example/mcp", "oauth": {} }
      },
      "instructions": ["CONTRIBUTING.md", "docs/guidelines.md"],
      "agent": { "reviewer": { "prompt": "Review carefully" } },
      "plugin": ["opencode-notify"],
      "permission": { "skill": { "*": "deny" } }
    }"#;

    #[test]
    fn the_current_shape_maps_servers_providers_and_the_rest() {
        let import = parse(CURRENT, "OpenCode").unwrap();
        assert_eq!(import.servers.len(), 2, "{:?}", import.servers);
        let rea = import.servers.iter().find(|s| s.name == "rea").unwrap();
        assert!(!rea.enabled);
        assert_eq!(rea.external.as_ref().unwrap().label, "OpenCode");
        let McpTransport::Stdio { command, args, env } = &rea.transport else { panic!() };
        assert_eq!(command, "npx");
        assert_eq!(args, &["-y", "rea-agents@6.8.0", "mcp"]);
        assert_eq!(env["TOKEN"], "${REA_TOKEN}", "env references become ${{VAR}}");
        assert_eq!(env["JAVA_HOME"], "/usr/lib/jvm/java-25-openjdk");
        let docs = import.servers.iter().find(|s| s.name == "docs").unwrap();
        let McpTransport::Http { headers, .. } = &docs.transport else { panic!() };
        assert_eq!(headers["Authorization"], "Bearer ${DOCS_KEY}");
        assert!(import.not_run.iter().any(|n| n.contains("sso") && n.contains("OAuth")), "{:?}", import.not_run);

        assert_eq!(import.providers.len(), 2, "{:?}", import.providers);
        let ollama = import.providers.iter().find(|p| p.name == "ollama").unwrap();
        assert_eq!(ollama.kind, ProviderKind::OpenaiCompatible);
        assert_eq!(ollama.base_url.as_deref(), Some("http://127.0.0.1:11434/v1"));
        let anthropic = import.providers.iter().find(|p| p.name == "anthropic").unwrap();
        assert_eq!(anthropic.kind, ProviderKind::Anthropic);
        assert!(
            import.warnings.iter().any(|w| w.contains("anthropic") && w.contains("$ANTHROPIC_API_KEY")),
            "{:?}",
            import.warnings
        );
        // A literal key is reported and never carried; a built-in provider without an address is explained.
        assert!(!format!("{import:?}").contains("sk-or-literal"));
        assert!(import.not_run.iter().any(|n| n.starts_with("provider openrouter")), "{:?}", import.not_run);
        assert_eq!(import.instructions, ["CONTRIBUTING.md", "docs/guidelines.md"]);
        assert!(import.not_run.iter().any(|n| n.starts_with("agent reviewer")));
        assert!(import.not_run.iter().any(|n| n.starts_with("plugin opencode-notify")));
    }

    #[test]
    fn comments_legacy_keys_and_files_are_handled() {
        let legacy = r#"{
          // older spelling
          "mcpServers": { "fs": { "command": ["npx", "-y", "@modelcontextprotocol/server-filesystem", "/tmp"], }, },
          "providers": { "local": { "options": { "baseURL": "http://localhost:8080/v1" } } },
        }"#;
        let import = parse(legacy, "OpenCode").unwrap();
        assert_eq!(import.servers[0].name, "fs");
        assert_eq!(import.providers[0].base_url.as_deref(), Some("http://localhost:8080/v1"));
        assert!(parse("not json", "x").is_err());

        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        std::fs::create_dir_all(config.join("opencode")).unwrap();
        std::fs::write(config.join("opencode/opencode.json"), CURRENT).unwrap();
        let project = dir.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join("opencode.jsonc"),
            r#"{"mcp": {"local-only": {"type": "local", "command": ["./server"]}}}"#,
        )
        .unwrap();
        let held = read(Some(&config), Some(&project), false);
        assert_eq!(held.servers.len(), 2, "the project file waits for trust");
        assert_eq!(held.held_back, Some(project.join("opencode.jsonc")));
        let trusted = read(Some(&config), Some(&project), true);
        assert_eq!(trusted.servers.len(), 3);
        assert_eq!(trusted.servers[2].external.as_ref().unwrap().label, "OpenCode · this project");
        assert_eq!(trusted.files.len(), 2);
        assert!(read(None, None, false).is_empty());
    }
}
