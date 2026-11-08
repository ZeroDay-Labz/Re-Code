//! MCP servers configured in Claude Code, read live so they stay in sync with it.
//!
//! Sources, in Claude Code's order of precedence (a later one replaces an earlier one of the same
//! name): user servers (`mcpServers` in `~/.claude.json`), servers from the project's `.mcp.json`
//! that Claude Code was told to trust, and local servers for this project
//! (`projects.<path>.mcpServers`). A repository's `.mcp.json` alone never adds a server: Claude Code
//! asks before using those, so re-code only takes the ones already approved there.

use recode_core::extensions::{ExternalSource, McpServer, McpTransport};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

const LABEL: &str = "Claude Code";

/// Reads the servers Claude Code would use in `project`. Missing or unreadable files give none.
#[must_use]
pub fn servers(claude_config: &Path, project: Option<&Path>) -> Vec<McpServer> {
    let Some(config) = read_json(claude_config) else { return Vec::new() };
    let project_file = project.and_then(|dir| read_json(&dir.join(".mcp.json")));
    collect(&config, project, project_file.as_ref())
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn collect(config: &Value, project: Option<&Path>, project_file: Option<&Value>) -> Vec<McpServer> {
    let mut found: BTreeMap<String, McpServer> = BTreeMap::new();
    let mut add = |servers: Option<&Value>, label: &str, allowed: &dyn Fn(&str) -> bool| {
        for (name, entry) in servers.and_then(Value::as_object).into_iter().flatten() {
            if allowed(name) {
                found.insert(name.clone(), server(name, entry, label));
            }
        }
    };
    add(config.get("mcpServers"), LABEL, &|_| true);

    let settings = project.and_then(|dir| config.get("projects")?.get(dir.to_string_lossy().as_ref()));
    let names = |key: &str| -> Vec<String> {
        settings
            .and_then(|s| s.get(key))
            .and_then(Value::as_array)
            .map(|items| items.iter().filter_map(|item| item.as_str().map(str::to_owned)).collect())
            .unwrap_or_default()
    };
    let (enabled, disabled) = (names("enabledMcpjsonServers"), names("disabledMcpjsonServers"));
    let all = settings.and_then(|s| s.get("enableAllProjectMcpServers")).and_then(Value::as_bool).unwrap_or(false);
    let approved = |name: &str| !disabled.iter().any(|n| n == name) && (all || enabled.iter().any(|n| n == name));
    add(project_file.and_then(|file| file.get("mcpServers")), "Claude Code · .mcp.json", &approved);

    add(settings.and_then(|s| s.get("mcpServers")), "Claude Code · this project", &|_| true);
    found.into_values().collect()
}

fn server(name: &str, entry: &Value, label: &str) -> McpServer {
    let text = |key: &str| entry.get(key).and_then(Value::as_str).map(str::to_owned);
    let strings = |key: &str| -> BTreeMap<String, String> {
        entry
            .get(key)
            .and_then(Value::as_object)
            .map(|map| map.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned()))).collect())
            .unwrap_or_default()
    };
    let kind = text("type").unwrap_or_else(|| if entry.get("command").is_some() { "stdio" } else { "http" }.to_owned());
    let mut unsupported = None;
    let transport = match (kind.as_str(), text("command"), text("url")) {
        ("stdio", Some(command), _) => {
            let args = entry
                .get("args")
                .and_then(Value::as_array)
                .map(|items| items.iter().filter_map(|item| item.as_str().map(str::to_owned)).collect())
                .unwrap_or_default();
            McpTransport::Stdio { command, args, env: strings("env") }
        }
        ("http" | "streamable-http" | "streamable_http", _, Some(url)) => {
            McpTransport::Http { url, headers: strings("headers") }
        }
        (other, _, url) => {
            unsupported = Some(if other == "sse" {
                "It's an SSE server, which re-code doesn't support yet (it speaks stdio and streamable HTTP)."
                    .to_owned()
            } else {
                format!("Its type ({other}) or address is missing or not one re-code supports.")
            });
            McpTransport::Http { url: url.unwrap_or_default(), headers: strings("headers") }
        }
    };
    McpServer {
        name: name.to_owned(),
        transport,
        enabled: true,
        expose_to_agent: false,
        external: Some(ExternalSource { label: label.to_owned(), unsupported }),
        tools: None,
        sandbox: recode_core::extensions::McpSandbox::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn by_name<'a>(servers: &'a [McpServer], name: &str) -> &'a McpServer {
        servers.iter().find(|s| s.name == name).unwrap_or_else(|| panic!("{name} missing: {servers:?}"))
    }

    #[test]
    fn user_servers_of_every_supported_kind_are_read_with_their_settings() {
        let config = json!({"mcpServers": {
            "rustrover": {"type": "http", "url": "http://127.0.0.1:64522/stream"},
            "remote": {"type": "http", "url": "https://mcp.example.com/mcp", "headers": {"Authorization": "Bearer ${TOKEN}"}},
            "rea": {"command": "npx", "args": ["-y", "rea-agents@6.8.0", "mcp"], "env": {"X": "1"}},
            "old": {"type": "sse", "url": "http://localhost:9000/sse"}
        }});
        let servers = collect(&config, None, None);
        assert_eq!(servers.len(), 4);
        assert!(
            matches!(&by_name(&servers, "rustrover").transport, McpTransport::Http { url, .. } if url.ends_with("/stream"))
        );
        let McpTransport::Http { headers, .. } = &by_name(&servers, "remote").transport else { panic!() };
        assert_eq!(headers["Authorization"], "Bearer ${TOKEN}", "headers are kept, references unresolved");
        let McpTransport::Stdio { command, args, env } = &by_name(&servers, "rea").transport else { panic!() };
        assert_eq!((command.as_str(), args.len(), env["X"].as_str()), ("npx", 3, "1"));
        let old = by_name(&servers, "old").external.as_ref().unwrap();
        assert!(old.unsupported.as_ref().unwrap().contains("SSE"), "shown, not silently dropped");
        assert!(servers.iter().all(|s| s.external.as_ref().unwrap().label == "Claude Code"));
    }

    #[test]
    fn this_projects_servers_are_added_and_replace_user_ones_of_the_same_name() {
        let config = json!({
            "mcpServers": {"rea": {"command": "npx", "args": ["old"]}},
            "projects": {
                "/home/me/proj": {"mcpServers": {"rea": {"command": "npx", "args": ["new"]}, "local": {"command": "x"}}},
                "/home/me/other": {"mcpServers": {"elsewhere": {"command": "y"}}}
            }
        });
        let servers = collect(&config, Some(Path::new("/home/me/proj")), None);
        let names: Vec<&str> = servers.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["local", "rea"], "another project's servers stay out");
        let McpTransport::Stdio { args, .. } = &by_name(&servers, "rea").transport else { panic!() };
        assert_eq!(args, &["new"]);
        assert_eq!(by_name(&servers, "local").external.as_ref().unwrap().label, "Claude Code · this project");
    }

    #[test]
    fn a_repositorys_mcp_json_only_counts_once_approved_in_claude_code() {
        let file = json!({"mcpServers": {"trusted": {"command": "a"}, "unknown": {"command": "b"}, "blocked": {"command": "c"}}});
        let config = json!({"projects": {"/p": {
            "enabledMcpjsonServers": ["trusted", "blocked"],
            "disabledMcpjsonServers": ["blocked"]
        }}});
        let servers = collect(&config, Some(Path::new("/p")), Some(&file));
        let names: Vec<&str> = servers.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["trusted"]);
        assert!(collect(&json!({}), Some(Path::new("/p")), Some(&file)).is_empty(), "nothing without approval");

        let all = json!({"projects": {"/p": {"enableAllProjectMcpServers": true}}});
        assert_eq!(collect(&all, Some(Path::new("/p")), Some(&file)).len(), 3);
    }

    #[test]
    fn a_missing_or_damaged_file_gives_no_servers() {
        let dir = tempfile::tempdir().unwrap();
        assert!(servers(&dir.path().join("none.json"), None).is_empty());
        std::fs::write(dir.path().join("bad.json"), "{ not json").unwrap();
        assert!(servers(&dir.path().join("bad.json"), None).is_empty());
    }
}
