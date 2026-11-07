//! Resolving what someone gives us into MCP servers: a direct http endpoint, an MCP `server.json`
//! (by URL, a GitHub repo, or a local path), a registry id such as `io.github.owner/server`, a pasted
//! config snippet, or a GitHub repo whose README shows how to run it. Also searches the official
//! registry.
//!
//! `server.json` is the Model Context Protocol server manifest; the registry serves the same shape.

use crate::paste;
use recode_core::extensions::{McpSearchHit, McpServer, McpTransport, validate_name};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::time::Duration;

const FETCH_TIMEOUT: Duration = Duration::from_secs(20);
/// The official MCP registry.
pub const REGISTRY_URL: &str = "https://registry.modelcontextprotocol.io";

/// What a link, id, snippet or command resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub servers: Vec<McpServer>,
    /// Something the user should know, such as "taken from the README; check it".
    pub note: Option<String>,
}

/// Resolves a link to one server. `name` overrides the derived name when given.
pub async fn resolve(name: Option<&str>, link: &str) -> Result<McpServer, String> {
    let mut resolved = resolve_any(name, link).await?;
    if resolved.servers.len() > 1 {
        return Err(format!(
            "That describes {} servers; paste it into the add form to add them all.",
            resolved.servers.len()
        ));
    }
    resolved.servers.pop().ok_or_else(|| "Nothing to add was found.".to_owned())
}

/// Resolves anything pasteable into servers, with their names made valid.
pub async fn resolve_any(name: Option<&str>, text: &str) -> Result<Resolved, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("Enter a link, address, registry id, command, or pasted config.".to_owned());
    }
    let mut resolved = if paste::looks_like_snippet(text) {
        Resolved { servers: paste::parse(name, text)?, note: None }
    } else if let Some(path) = local_json_path(text) {
        let json = std::fs::read_to_string(&path).map_err(|error| format!("can't read {}: {error}", path.display()))?;
        Resolved { servers: vec![from_server_json(&json, text)?], note: None }
    } else if let Some((owner, repo)) = github_repo(text) {
        resolve_github(&owner, &repo).await?
    } else if text.starts_with("http://") || text.starts_with("https://") {
        if text.ends_with(".json") || text.contains("server.json") {
            Resolved { servers: vec![from_server_json(&fetch(text).await?, text)?], note: None }
        } else {
            // A direct MCP HTTP endpoint.
            Resolved {
                servers: vec![McpServer {
                    name: "server".to_owned(),
                    transport: McpTransport::Http { url: text.to_owned(), headers: BTreeMap::new() },
                    enabled: true,
                    expose_to_agent: false,
                    external: None,
                    tools: None,
                }],
                note: None,
            }
        }
    } else if is_registry_id(text) {
        let hit = lookup(text).await?;
        match (hit.server, hit.unsupported) {
            (Some(server), _) => Resolved { servers: vec![server], note: None },
            (None, Some(why)) => return Err(format!("{text} is in the registry but can't be added: {why}")),
            (None, None) => return Err(format!("{text} is in the registry but has no package re-code can run.")),
        }
    } else {
        return Err(
            "That doesn't look like a link, registry id or config snippet. For a command, use the normal add form."
                .to_owned(),
        );
    };

    let override_name = name.map(str::trim).filter(|name| !name.is_empty());
    if resolved.servers.len() == 1
        && let Some(name) = override_name
    {
        validate_name("server", name)?;
        resolved.servers[0].name = name.to_owned();
    }
    for server in &resolved.servers {
        validate_name("server", &server.name)
            .map_err(|_| format!("Couldn't derive a valid name ({}). Enter one in the Name field.", server.name))?;
    }
    Ok(resolved)
}

/// `io.github.owner/server`, `com.example/thing`: a reverse-DNS namespace, a slash, a name.
#[must_use]
pub fn is_registry_id(text: &str) -> bool {
    let Some((namespace, name)) = text.split_once('/') else { return false };
    namespace.contains('.')
        && !namespace.is_empty()
        && !name.is_empty()
        && !name.contains('/')
        && namespace.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
        && !text.contains("://")
}

fn local_json_path(link: &str) -> Option<std::path::PathBuf> {
    let path = std::path::Path::new(link);
    (link.ends_with(".json") && path.is_file()).then(|| path.to_path_buf())
}

/// `https://github.com/owner/repo[...]` → (owner, repo).
fn github_repo(link: &str) -> Option<(String, String)> {
    let rest = link.strip_prefix("https://github.com/")?;
    let mut parts = rest.split('/');
    let owner = parts.next().filter(|s| !s.is_empty())?;
    let repo = parts.next().filter(|s| !s.is_empty())?.trim_end_matches(".git");
    Some((owner.to_owned(), repo.to_owned()))
}

/// A GitHub repo: its `server.json` when it has one, otherwise what its README shows.
async fn resolve_github(owner: &str, repo: &str) -> Result<Resolved, String> {
    let raw = format!("https://raw.githubusercontent.com/{owner}/{repo}/HEAD/server.json");
    if let Ok(json) = fetch(&raw).await {
        return Ok(Resolved { servers: vec![from_server_json(&json, &raw)?], note: None });
    }
    let readme = format!("https://raw.githubusercontent.com/{owner}/{repo}/HEAD/README.md");
    let text = fetch(&readme)
        .await
        .map_err(|_| format!("{owner}/{repo} has no server.json at its root and its README.md couldn't be read."))?;
    let mut servers = from_readme(Some(repo), &text).ok_or_else(|| {
        format!("{owner}/{repo} has no server.json, and its README shows no MCP config or command re-code recognises.")
    })?;
    for server in &mut servers {
        if server.name == "server" {
            server.name = repo.to_lowercase();
        }
    }
    Ok(Resolved {
        servers,
        note: Some(format!(
            "Taken from {owner}/{repo}'s README rather than a server.json: check the command before testing it."
        )),
    })
}

/// The first MCP config block or runnable command line in a README.
#[must_use]
pub fn from_readme(name: Option<&str>, text: &str) -> Option<Vec<McpServer>> {
    // Fenced code blocks, in order; the first that parses as a config wins.
    let mut in_block = false;
    let mut block = String::new();
    let mut blocks: Vec<String> = Vec::new();
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            if in_block {
                blocks.push(std::mem::take(&mut block));
            }
            in_block = !in_block;
            continue;
        }
        if in_block {
            block.push_str(line);
            block.push('\n');
        }
    }
    for candidate in &blocks {
        let trimmed = candidate.trim();
        if (trimmed.contains("mcpServers") || trimmed.contains("\"servers\"") || trimmed.contains("\"mcp\""))
            && let Ok(servers) = paste::parse(name, trimmed)
            && !servers.is_empty()
        {
            return Some(servers);
        }
    }
    for candidate in &blocks {
        for line in candidate.lines() {
            let line = line.trim().trim_start_matches('$').trim();
            if ["npx ", "uvx ", "docker run ", "podman run ", "bunx "].iter().any(|prefix| line.starts_with(prefix))
                && let Ok(server) = paste::from_command_line(name, line)
            {
                return Some(vec![server]);
            }
        }
    }
    None
}

async fn fetch(url: &str) -> Result<String, String> {
    let client = reqwest::Client::builder().timeout(FETCH_TIMEOUT).build().map_err(|error| error.to_string())?;
    let response = client.get(url).send().await.map_err(|error| format!("can't reach {url}: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("{url} returned HTTP {}", response.status().as_u16()));
    }
    response.text().await.map_err(|error| error.to_string())
}

/// Searches the official registry. Each hit says how re-code would run it, or why it can't.
pub async fn search(query: &str, limit: usize) -> Result<Vec<McpSearchHit>, String> {
    let query = query.trim();
    if query.is_empty() {
        return Err("Type something to search for.".to_owned());
    }
    let url =
        format!("{REGISTRY_URL}/v0.1/servers?search={}&limit={}&version=latest", urlencode(query), limit.clamp(1, 50));
    let text = fetch(&url).await?;
    parse_search(&text)
}

/// Finds one server by its registry id.
pub async fn lookup(id: &str) -> Result<McpSearchHit, String> {
    let needle = id.rsplit('/').next().unwrap_or(id);
    let hits = search(needle, 50).await?;
    hits.into_iter()
        .find(|hit| hit.name == id)
        .ok_or_else(|| format!("{id} isn't in the registry. Search for part of its name to see what is."))
}

/// Parses a registry listing (`{"servers": [{"server": {...}}]}`), keeping one entry per name.
pub fn parse_search(text: &str) -> Result<Vec<McpSearchHit>, String> {
    let listing: Listing =
        serde_json::from_str(text).map_err(|error| format!("the registry answered unexpectedly: {error}"))?;
    let mut hits: Vec<McpSearchHit> = Vec::new();
    for item in listing.servers {
        let manifest = item.server;
        let hit = McpSearchHit {
            name: manifest.name.clone(),
            description: manifest.description.clone().unwrap_or_default(),
            version: manifest.version.clone(),
            repository: manifest.repository.as_ref().and_then(|r| r.url.clone()),
            server: None,
            unsupported: None,
        };
        let hit = match from_manifest(&manifest) {
            Ok(server) => McpSearchHit { server: Some(server), ..hit },
            Err(why) => McpSearchHit { unsupported: Some(why), ..hit },
        };
        // The same name appears once per published version; the latest listed wins.
        if let Some(existing) = hits.iter_mut().find(|h| h.name == hit.name) {
            *existing = hit;
        } else {
            hits.push(hit);
        }
    }
    Ok(hits)
}

fn urlencode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(byte as char),
            b' ' => out.push('+'),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Builds a server from an MCP `server.json`.
pub fn from_server_json(text: &str, source: &str) -> Result<McpServer, String> {
    let manifest: ServerJson =
        serde_json::from_str(text).map_err(|error| format!("{source} isn't a valid server.json: {error}"))?;
    from_manifest(&manifest)
}

/// The first way re-code can run a manifest: an npm package (`npx`), a PyPI package (`uvx`), a
/// streamable-HTTP remote, or an OCI image (`podman run`). SSE remotes aren't supported.
fn from_manifest(manifest: &ServerJson) -> Result<McpServer, String> {
    let name = manifest.name.rsplit('/').next().unwrap_or("server");
    let name = name.split('@').next().unwrap_or(name).to_lowercase();
    let base = McpServer {
        name,
        transport: McpTransport::Http { url: String::new(), headers: BTreeMap::new() },
        enabled: true,
        expose_to_agent: false,
        external: None,
        tools: None,
    };
    let package_of = |kind: &str| {
        manifest.packages.iter().find(|package| {
            package.identifier.is_some()
                && (package.registry_type.as_deref().is_some_and(|t| t.eq_ignore_ascii_case(kind))
                    || (kind == "npm" && package.registry_type.is_none()))
                && package.transport.as_ref().is_none_or(|t| t.kind.as_deref().is_none_or(|k| k == "stdio"))
        })
    };
    let env_of = |package: &Package| -> BTreeMap<String, String> {
        package
            .environment_variables
            .iter()
            .filter_map(|variable| variable.name.clone())
            .map(|name| (name.clone(), format!("${{{name}}}")))
            .collect()
    };
    let args_of = |package: &Package| -> Vec<String> {
        let mut args = Vec::new();
        for argument in &package.package_arguments {
            match argument.kind.as_deref() {
                Some("named") => {
                    if let Some(arg_name) = &argument.name {
                        args.push(arg_name.clone());
                    }
                    if let Some(value) = &argument.value {
                        args.push(value.clone());
                    }
                }
                _ => {
                    if let Some(value) = &argument.value {
                        args.push(value.clone());
                    }
                }
            }
        }
        args
    };
    if let Some(package) = package_of("npm") {
        let identifier = package.identifier.clone().unwrap_or_default();
        let version = package.version.clone().unwrap_or_else(|| "latest".to_owned());
        let runner = match package.runtime_hint.as_deref() {
            Some("bunx") => "bunx",
            _ => "npx",
        };
        let mut args = vec!["-y".to_owned(), format!("{identifier}@{version}")];
        args.extend(args_of(package));
        return Ok(McpServer {
            transport: McpTransport::Stdio { command: runner.to_owned(), args, env: env_of(package) },
            ..base
        });
    }
    if let Some(package) = package_of("pypi") {
        let identifier = package.identifier.clone().unwrap_or_default();
        let spec = match &package.version {
            Some(version) => format!("{identifier}=={version}"),
            None => identifier,
        };
        let mut args = vec![spec];
        args.extend(args_of(package));
        return Ok(McpServer {
            transport: McpTransport::Stdio { command: "uvx".to_owned(), args, env: env_of(package) },
            ..base
        });
    }
    if let Some(remote) = manifest.remotes.iter().find(|remote| remote.kind.as_deref() == Some("streamable-http")) {
        let url = remote.url.clone().ok_or("the remote has no url")?;
        let headers = remote
            .headers
            .iter()
            .filter_map(|header| {
                let name = header.name.clone()?;
                // `Bearer {api_key}` placeholders become ${API_KEY} references for the user to fill in.
                let value =
                    header.value.clone().unwrap_or_else(|| format!("${{{}}}", name.to_uppercase().replace('-', "_")));
                Some((name, braces_to_env(&value)))
            })
            .collect();
        return Ok(McpServer { transport: McpTransport::Http { url, headers }, ..base });
    }
    if let Some(package) = package_of("oci") {
        let image = package.identifier.clone().unwrap_or_default();
        let image = match &package.version {
            Some(version) if !image.contains(':') => format!("{image}:{version}"),
            _ => image,
        };
        let mut args = vec!["run".to_owned(), "--rm".to_owned(), "-i".to_owned()];
        let env = env_of(package);
        for key in env.keys() {
            args.push("-e".to_owned());
            args.push(key.clone());
        }
        args.push(image);
        args.extend(args_of(package));
        return Ok(McpServer { transport: McpTransport::Stdio { command: "podman".to_owned(), args, env }, ..base });
    }
    if manifest.remotes.iter().any(|remote| remote.kind.as_deref() == Some("sse")) {
        return Err("only an SSE remote is offered, and re-code speaks streamable HTTP".to_owned());
    }
    Err("no npm, PyPI or OCI package and no streamable-HTTP remote re-code can run".to_owned())
}

/// `Bearer {smithery_api_key}` → `Bearer ${SMITHERY_API_KEY}`.
fn braces_to_env(value: &str) -> String {
    let mut out = String::new();
    let mut rest = value;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('}') {
            Some(end) if !after[..end].is_empty() && !rest[..start].ends_with('$') => {
                out.push_str(&format!("${{{}}}", after[..end].to_uppercase().replace('-', "_")));
                rest = &after[end + 1..];
            }
            _ => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

#[derive(Deserialize)]
struct Listing {
    #[serde(default)]
    servers: Vec<ListingItem>,
}

#[derive(Deserialize)]
struct ListingItem {
    server: ServerJson,
}

#[derive(Deserialize)]
struct ServerJson {
    #[serde(default)]
    name: String,
    description: Option<String>,
    version: Option<String>,
    repository: Option<Repository>,
    #[serde(default)]
    packages: Vec<Package>,
    #[serde(default)]
    remotes: Vec<Remote>,
}

#[derive(Deserialize)]
struct Repository {
    url: Option<String>,
}

#[derive(Deserialize)]
struct Remote {
    #[serde(rename = "type")]
    kind: Option<String>,
    url: Option<String>,
    #[serde(default)]
    headers: Vec<Header>,
}

#[derive(Deserialize)]
struct Header {
    name: Option<String>,
    value: Option<String>,
}

#[derive(Deserialize)]
struct Package {
    #[serde(rename = "registryType")]
    registry_type: Option<String>,
    identifier: Option<String>,
    version: Option<String>,
    #[serde(rename = "runtimeHint")]
    runtime_hint: Option<String>,
    transport: Option<Transport>,
    #[serde(rename = "packageArguments", default)]
    package_arguments: Vec<Argument>,
    #[serde(rename = "environmentVariables", default)]
    environment_variables: Vec<EnvVar>,
}

#[derive(Deserialize)]
struct Transport {
    #[serde(rename = "type")]
    kind: Option<String>,
}

#[derive(Deserialize)]
struct Argument {
    #[serde(rename = "type")]
    kind: Option<String>,
    name: Option<String>,
    value: Option<String>,
}

#[derive(Deserialize)]
struct EnvVar {
    name: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    // morluto/rea's actual server.json shape.
    const REA: &str = r#"{
        "name": "io.github.morluto/rea",
        "packages": [{
            "registryType": "npm", "identifier": "rea-agents", "version": "6.8.0",
            "runtimeHint": "npx", "transport": {"type": "stdio"},
            "packageArguments": [{"type": "positional", "value": "mcp"}]
        }]
    }"#;

    #[test]
    fn parses_reas_server_json() {
        let server = from_server_json(REA, "x").unwrap();
        assert_eq!(server.name, "rea");
        let McpTransport::Stdio { command, args, .. } = server.transport else { panic!("not stdio") };
        assert_eq!(command, "npx");
        assert_eq!(args, ["-y", "rea-agents@6.8.0", "mcp"]);
    }

    #[test]
    fn named_args_and_env_become_flags_and_references() {
        let json = r#"{"name":"a/b","packages":[{"registryType":"npm","identifier":"p","packageArguments":[{"type":"named","name":"--port","value":"3000"}],"environmentVariables":[{"name":"API_KEY"}]}]}"#;
        let server = from_server_json(json, "x").unwrap();
        let McpTransport::Stdio { args, env, .. } = server.transport else { panic!() };
        assert_eq!(args, ["-y", "p@latest", "--port", "3000"]);
        assert_eq!(env.get("API_KEY").unwrap(), "${API_KEY}");
    }

    #[test]
    fn pypi_oci_and_remote_manifests_are_runnable_and_sse_is_not() {
        let pypi =
            r#"{"name":"x/git","packages":[{"registryType":"pypi","identifier":"mcp-server-git","version":"0.6.2"}]}"#;
        let McpTransport::Stdio { command, args, .. } = from_server_json(pypi, "x").unwrap().transport else {
            panic!()
        };
        assert_eq!((command.as_str(), args[0].as_str()), ("uvx", "mcp-server-git==0.6.2"));
        let oci = r#"{"name":"x/gh","packages":[{"registryType":"oci","identifier":"ghcr.io/github/github-mcp-server","version":"1.0","environmentVariables":[{"name":"GITHUB_TOKEN"}]}]}"#;
        let McpTransport::Stdio { command, args, env } = from_server_json(oci, "x").unwrap().transport else {
            panic!()
        };
        assert_eq!(command, "podman");
        assert_eq!(args, ["run", "--rm", "-i", "-e", "GITHUB_TOKEN", "ghcr.io/github/github-mcp-server:1.0"]);
        assert_eq!(env["GITHUB_TOKEN"], "${GITHUB_TOKEN}");
        assert!(!args.iter().any(|a| a == "--privileged"));
        let remote = r#"{"name":"ai.smithery/x","remotes":[{"type":"streamable-http","url":"https://s.example/mcp","headers":[{"name":"Authorization","value":"Bearer {smithery_api_key}"}]}]}"#;
        let McpTransport::Http { url, headers } = from_server_json(remote, "x").unwrap().transport else { panic!() };
        assert_eq!(url, "https://s.example/mcp");
        assert_eq!(headers["Authorization"], "Bearer ${SMITHERY_API_KEY}");
        let sse = r#"{"name":"x/old","remotes":[{"type":"sse","url":"https://s.example/sse"}]}"#;
        assert!(from_server_json(sse, "x").unwrap_err().contains("SSE"));
    }

    #[test]
    fn a_registry_listing_from_the_live_api_is_understood() {
        let text = include_str!("../tests/fixtures/registry_search.json");
        let hits = parse_search(text).unwrap();
        assert_eq!(hits.len(), 1);
        let hit = &hits[0];
        assert_eq!(hit.name, "com.pulsemcp/remote-filesystem");
        assert_eq!(hit.version.as_deref(), Some("0.1.2"));
        assert!(hit.repository.as_deref().unwrap().contains("github.com/pulsemcp"));
        let server = hit.server.as_ref().expect("an npm package re-code can run");
        assert_eq!(server.name, "remote-filesystem");
        let McpTransport::Stdio { command, args, env } = &server.transport else { panic!() };
        assert_eq!((command.as_str(), args[1].as_str()), ("npx", "remote-filesystem-mcp-server@0.1.2"));
        assert_eq!(env["GCS_BUCKET"], "${GCS_BUCKET}");
        // Several versions of one name collapse into one hit.
        let listing: serde_json::Value = serde_json::from_str(text).unwrap();
        let item = listing["servers"][0].clone();
        let doubled = serde_json::json!({"servers": [item.clone(), item]});
        assert_eq!(parse_search(&doubled.to_string()).unwrap().len(), 1);
    }

    #[test]
    fn registry_ids_and_github_links_are_recognised() {
        assert!(is_registry_id("io.github.morluto/rea"));
        assert!(is_registry_id("com.pulsemcp/remote-filesystem"));
        assert!(!is_registry_id("npx -y thing"));
        assert!(!is_registry_id("https://github.com/a/b"));
        assert!(!is_registry_id("owner/repo"), "a plain owner/repo has no namespace");
        assert_eq!(
            github_repo("https://github.com/morluto/rea/tree/main"),
            Some(("morluto".to_owned(), "rea".to_owned()))
        );
        assert_eq!(github_repo("https://example.com/x"), None);
    }

    #[test]
    fn a_readme_yields_its_config_block_or_its_command() {
        let readme = "# Server\n\nInstall:\n\n```json\n{\n  \"mcpServers\": {\n    \"thing\": {\"command\": \"npx\", \"args\": [\"-y\", \"thing-mcp\"]}\n  }\n}\n```\n";
        let servers = from_readme(None, readme).unwrap();
        assert_eq!(servers[0].name, "thing");
        let plain = "Run it with:\n\n```bash\n$ uvx thing-mcp --flag\n```\n";
        let servers = from_readme(Some("repo"), plain).unwrap();
        assert_eq!(servers[0].name, "repo");
        assert!(from_readme(None, "nothing here").is_none());
    }

    #[tokio::test]
    async fn a_direct_http_url_becomes_an_http_server() {
        let server = resolve(Some("ide"), "http://127.0.0.1:64522/stream").await.unwrap();
        assert!(matches!(server.transport, McpTransport::Http { .. }));
        assert_eq!(server.name, "ide");
    }

    #[tokio::test]
    async fn a_pasted_snippet_resolves_to_all_its_servers() {
        let snippet =
            r#"{"mcpServers": {"a": {"command": "npx", "args": ["x"]}, "b": {"url": "https://b.example/mcp"}}}"#;
        let resolved = resolve_any(None, snippet).await.unwrap();
        assert_eq!(resolved.servers.len(), 2);
        assert!(resolve(None, snippet).await.unwrap_err().contains("2 servers"));
    }

    #[tokio::test]
    async fn a_plain_command_is_rejected_as_not_a_link() {
        assert!(resolve(None, "npx -y thing").await.unwrap_err().contains("doesn't look like a link"));
    }
}
