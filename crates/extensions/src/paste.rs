//! "Paste anything": turns what people copy from an MCP server's README into server entries. Accepted
//! shapes are the ones other tools use for their config files, plus a bare command line:
//!
//! - Claude Desktop / Claude Code / Cursor: `{"mcpServers": {"name": {"command", "args", "env"} | {"url", "headers"}}}`
//! - VS Code: `{"servers": {"name": {"type": "stdio" | "http", …}}}`
//! - OpenCode: `{"mcp": {"name": {"type": "local", "command": [..], "environment": {}} | {"type": "remote", "url"}}}`
//! - A bare object of the same entries, or a single entry (`{"command": "npx", "args": [..]}`)
//! - A command line: `npx -y @scope/pkg …`, `uvx pkg`, `docker run -i --rm image`, …
//!
//! Nothing here runs anything; it only produces entries for the user to test.

use recode_core::extensions::{McpServer, McpTransport, validate_name};
use serde_json::Value;
use std::collections::BTreeMap;

/// Parses pasted text into servers. `name` is used when the paste doesn't carry one.
pub fn parse(name: Option<&str>, text: &str) -> Result<Vec<McpServer>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("Paste a config snippet or type a command.".to_owned());
    }
    if text.starts_with('{') {
        let value: Value = serde_json::from_str(&without_comments(text))
            .or_else(|_| serde_json::from_str(&format!("{{{text}}}")))
            .map_err(|error| format!("That isn't valid JSON: {error}"))?;
        return from_json(name, &value);
    }
    // A command line.
    let server = from_command_line(name, text)?;
    Ok(vec![server])
}

/// Whether text looks like a config snippet rather than a command or address.
#[must_use]
pub fn looks_like_snippet(text: &str) -> bool {
    let text = text.trim();
    text.starts_with('{')
        || text.starts_with("\"mcpServers\"")
        || text.starts_with("\"servers\"")
        || text.starts_with("\"mcp\"")
}

/// Strips `//` and `/* */` comments and trailing commas, which JSONC config files allow.
#[must_use]
pub fn without_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let mut index = 0;
    let mut in_string = false;
    while index < chars.len() {
        let c = chars[index];
        if in_string {
            out.push(c);
            if c == '\\' && index + 1 < chars.len() {
                out.push(chars[index + 1]);
                index += 2;
                continue;
            }
            if c == '"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
                index += 1;
            }
            '/' if chars.get(index + 1) == Some(&'/') => {
                while index < chars.len() && chars[index] != '\n' {
                    index += 1;
                }
            }
            '/' if chars.get(index + 1) == Some(&'*') => {
                index += 2;
                while index + 1 < chars.len() && !(chars[index] == '*' && chars[index + 1] == '/') {
                    index += 1;
                }
                index += 2;
            }
            ',' => {
                // A trailing comma before } or ] is dropped.
                let mut look = index + 1;
                while look < chars.len() && chars[look].is_whitespace() {
                    look += 1;
                }
                if look < chars.len() && (chars[look] == '}' || chars[look] == ']') {
                    index += 1;
                    continue;
                }
                out.push(c);
                index += 1;
            }
            _ => {
                out.push(c);
                index += 1;
            }
        }
    }
    out
}

fn from_json(name: Option<&str>, value: &Value) -> Result<Vec<McpServer>, String> {
    let object = value.as_object().ok_or("Expected a JSON object.")?;
    // A wrapper key, or a bare map of entries, or a single entry.
    let entries: Vec<(String, &Value)> = if let Some(map) = ["mcpServers", "servers", "mcp", "mcp_servers"]
        .iter()
        .find_map(|key| object.get(*key).and_then(Value::as_object))
    {
        map.iter().map(|(k, v)| (k.clone(), v)).collect()
    } else if is_entry(object) {
        vec![(name.unwrap_or_default().to_owned(), value)]
    } else if object.values().all(|v| v.as_object().is_some_and(is_entry)) && !object.is_empty() {
        object.iter().map(|(k, v)| (k.clone(), v)).collect()
    } else {
        return Err("No MCP servers found: expected \"mcpServers\", \"servers\" or \"mcp\" entries.".to_owned());
    };
    let mut servers = Vec::new();
    for (key, entry) in entries {
        let entry_object = entry.as_object().ok_or_else(|| format!("{key} isn't an object."))?;
        let mut server = from_entry(entry_object).map_err(|error| format!("{key}: {error}"))?;
        let chosen = name.filter(|n| !n.trim().is_empty() && servers.is_empty() && entries_len_is_one(entry_object));
        server.name = derive_name(chosen.unwrap_or(&key), &server);
        servers.push(server);
    }
    Ok(servers)
}

fn entries_len_is_one(_: &serde_json::Map<String, Value>) -> bool {
    true
}

fn is_entry(object: &serde_json::Map<String, Value>) -> bool {
    object.contains_key("command") || object.contains_key("url")
}

fn from_entry(entry: &serde_json::Map<String, Value>) -> Result<McpServer, String> {
    let kind = entry.get("type").and_then(Value::as_str).unwrap_or_default();
    let text = |key: &str| entry.get(key).and_then(Value::as_str).map(str::to_owned);
    let map = |key: &str| -> BTreeMap<String, String> {
        entry
            .get(key)
            .and_then(Value::as_object)
            .map(|m| m.iter().filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_owned()))).collect())
            .unwrap_or_default()
    };
    if let Some(url) = text("url") {
        if kind == "sse" {
            return Err("SSE servers aren't supported; this one needs the streamable HTTP transport.".to_owned());
        }
        let mut headers = map("headers");
        // OpenCode spells them the same; VS Code may nest them under "headers" too.
        if headers.is_empty() {
            headers = map("requestInit");
        }
        return Ok(McpServer {
            name: String::new(),
            transport: McpTransport::Http { url, headers },
            enabled: true,
            expose_to_agent: false,
            external: None,
            tools: None,
        });
    }
    let command_value = entry.get("command").ok_or("has neither a command nor a url")?;
    let (command, mut args): (String, Vec<String>) = match command_value {
        // OpenCode: the whole command line as an array.
        Value::Array(parts) => {
            let parts: Vec<String> = parts.iter().filter_map(Value::as_str).map(str::to_owned).collect();
            let (first, rest) = parts.split_first().ok_or("has an empty command")?;
            (first.clone(), rest.to_vec())
        }
        Value::String(command) => (command.clone(), Vec::new()),
        _ => return Err("has a command that isn't text".to_owned()),
    };
    if let Some(list) = entry.get("args").and_then(Value::as_array) {
        args.extend(list.iter().filter_map(Value::as_str).map(str::to_owned));
    }
    let mut env = map("env");
    if env.is_empty() {
        env = map("environment");
    }
    Ok(McpServer {
        name: String::new(),
        transport: McpTransport::Stdio { command, args, env },
        enabled: true,
        expose_to_agent: false,
        external: None,
        tools: None,
    })
}

/// `npx -y @scope/pkg …`, `uvx pkg`, `docker run -i --rm image`, or any `program args…`.
pub fn from_command_line(name: Option<&str>, line: &str) -> Result<McpServer, String> {
    let words = recode_core::extensions::split_command(line)?;
    let (command, args) = words.split_first().ok_or("Type a command to run.")?;
    let mut server = McpServer {
        name: String::new(),
        transport: McpTransport::Stdio { command: command.clone(), args: args.to_vec(), env: BTreeMap::new() },
        enabled: true,
        expose_to_agent: false,
        external: None,
        tools: None,
    };
    let derived = name.map(str::to_owned).unwrap_or_else(|| package_name(command, args));
    server.name = derive_name(&derived, &server);
    Ok(server)
}

/// A readable name from the package a runner would fetch: `@modelcontextprotocol/server-filesystem` →
/// `server-filesystem`, `mcp-server-git` → `mcp-server-git`, `ghcr.io/x/y:tag` → `y`.
#[must_use]
pub fn package_name(command: &str, args: &[String]) -> String {
    let runner = command.rsplit('/').next().unwrap_or(command);
    let candidate = match runner {
        "npx" | "bunx" | "pnpx" | "uvx" | "pipx" => {
            args.iter().filter(|arg| !arg.starts_with('-') && *arg != "run").find(|arg| !arg.is_empty()).cloned()
        }
        "docker" | "podman" => {
            args.iter().filter(|arg| !arg.starts_with('-')).find(|arg| arg.contains('/') || arg.contains(':')).cloned()
        }
        _ => None,
    };
    let candidate = candidate.unwrap_or_else(|| runner.to_owned());
    let candidate = candidate.split('@').find(|part| !part.is_empty()).unwrap_or(&candidate);
    let candidate = candidate.rsplit('/').next().unwrap_or(candidate);
    let candidate = candidate.split(':').next().unwrap_or(candidate);
    candidate.to_owned()
}

/// Makes a pasted name valid for re-code: lowercase, `-` for anything else, trimmed.
fn derive_name(raw: &str, server: &McpServer) -> String {
    let mut name: String = raw
        .trim()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c.to_ascii_lowercase() } else { '-' })
        .collect();
    while name.contains("--") {
        name = name.replace("--", "-");
    }
    let name = name.trim_matches('-').to_owned();
    if validate_name("server", &name).is_ok() {
        return name;
    }
    match &server.transport {
        McpTransport::Stdio { command, args, .. } => derive_name(
            &package_name(command, args),
            &McpServer {
                name: String::new(),
                transport: McpTransport::Http { url: String::new(), headers: BTreeMap::new() },
                enabled: true,
                expose_to_agent: false,
                external: None,
                tools: None,
            },
        ),
        McpTransport::Http { .. } => "server".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stdio(server: &McpServer) -> (String, Vec<String>, BTreeMap<String, String>) {
        match &server.transport {
            McpTransport::Stdio { command, args, env } => (command.clone(), args.clone(), env.clone()),
            McpTransport::Http { .. } => panic!("expected stdio: {server:?}"),
        }
    }

    #[test]
    fn claude_desktop_shape_with_comments_and_trailing_commas() {
        let text = r#"{
          // from the README
          "mcpServers": {
            "filesystem": {
              "command": "npx",
              "args": ["-y", "@modelcontextprotocol/server-filesystem", "/home/me"],
              "env": {"DEBUG": "1"},
            },
            "remote": {"type": "http", "url": "https://mcp.example.com/mcp", "headers": {"Authorization": "Bearer ${TOKEN}"}},
          },
        }"#;
        let servers = parse(None, text).unwrap();
        assert_eq!(servers.len(), 2);
        let (command, args, env) = stdio(&servers[0]);
        assert_eq!(servers[0].name, "filesystem");
        assert_eq!(command, "npx");
        assert_eq!(args, ["-y", "@modelcontextprotocol/server-filesystem", "/home/me"]);
        assert_eq!(env.get("DEBUG").map(String::as_str), Some("1"));
        assert!(
            matches!(&servers[1].transport, McpTransport::Http { url, headers } if url == "https://mcp.example.com/mcp" && headers.len() == 1)
        );
    }

    #[test]
    fn vscode_and_opencode_shapes_and_a_bare_entry() {
        let vscode = r#"{"servers": {"git": {"type": "stdio", "command": "uvx", "args": ["mcp-server-git"]}}}"#;
        let servers = parse(None, vscode).unwrap();
        assert_eq!((servers[0].name.as_str(), stdio(&servers[0]).0.as_str()), ("git", "uvx"));
        let opencode = r#"{"mcp": {"context7": {"type": "local", "command": ["npx", "-y", "@upstash/context7-mcp"], "environment": {"K": "v"}, "enabled": true}, "remote": {"type": "remote", "url": "https://x.example/mcp"}}}"#;
        let servers = parse(None, opencode).unwrap();
        let (command, args, env) = stdio(&servers[0]);
        assert_eq!((command.as_str(), args.len(), env.len()), ("npx", 2, 1));
        assert!(matches!(servers[1].transport, McpTransport::Http { .. }));
        let bare = r#"{"command": "npx", "args": ["-y", "@modelcontextprotocol/server-memory"]}"#;
        let servers = parse(None, bare).unwrap();
        assert_eq!(servers[0].name, "server-memory", "a bare entry is named after its package");
        let named = parse(Some("Memory Server"), bare).unwrap();
        assert_eq!(named[0].name, "memory-server", "a given name is made valid");
    }

    #[test]
    fn sse_is_refused_and_garbage_explained() {
        let sse = r#"{"mcpServers": {"old": {"type": "sse", "url": "https://x/sse"}}}"#;
        assert!(parse(None, sse).unwrap_err().contains("SSE"));
        assert!(parse(None, "{\"foo\": 1}").unwrap_err().contains("No MCP servers"));
        assert!(parse(None, "{not json").unwrap_err().contains("valid JSON"));
    }

    #[test]
    fn command_lines_are_named_after_their_package() {
        let server = from_command_line(None, "npx -y @modelcontextprotocol/server-filesystem /tmp").unwrap();
        assert_eq!(server.name, "server-filesystem");
        assert_eq!(from_command_line(None, "uvx mcp-server-git --repo .").unwrap().name, "mcp-server-git");
        assert_eq!(
            from_command_line(None, "docker run -i --rm ghcr.io/github/github-mcp-server:latest").unwrap().name,
            "github-mcp-server"
        );
        assert_eq!(from_command_line(None, "/usr/local/bin/my-server --port 1").unwrap().name, "my-server");
        assert_eq!(package_name("npx", &["-y".into(), "@scope/thing@1.2.3".into()]), "thing");
    }
}
