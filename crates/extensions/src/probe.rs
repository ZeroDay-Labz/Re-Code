//! Connects to an MCP server, asks what it offers, and disconnects.

use http::{HeaderName, HeaderValue};
use recode_core::extensions::{McpProbe, McpSandbox, McpServer, McpToolInfo, McpTransport, is_reference};
use rmcp::ServiceExt;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::{StreamableHttpClientTransport, TokioChildProcess};
use std::collections::{BTreeMap, HashMap};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

/// Long enough for `npx` to fetch a package on first use.
const TIMEOUT: Duration = Duration::from_secs(45);
const CHARS_PER_TOKEN: usize = 3;
/// How many of a server's stderr lines are kept to explain a failed start.
const STDERR_TAIL: usize = 40;

/// A connected MCP client.
pub(crate) type McpClient = rmcp::service::RunningService<rmcp::RoleClient, ()>;

/// Folders under the home folder that a server's command, arguments and settings name, so the sandbox
/// can show them read-only: a script's project (the nearest folder above it holding `package.json`,
/// `.git`, `pyproject.toml`, `Cargo.toml` or `go.mod`, else its own folder), a named folder itself.
/// Never the home folder or one of its broad shared folders; the sandbox still hides secrets inside.
#[must_use]
pub fn own_paths(
    command: &str,
    args: &[String],
    env: &[(String, String)],
    home: &std::path::Path,
) -> Vec<std::path::PathBuf> {
    use std::path::{Path, PathBuf};
    const MARKERS: &[&str] = &["package.json", ".git", "pyproject.toml", "Cargo.toml", "go.mod"];
    const TOO_BROAD: &[&str] =
        &[".config", ".local", ".local/share", ".local/state", ".cache", "Documents", "Downloads"];
    const SECRET: &[&str] = &[".ssh", ".gnupg", ".password-store", ".aws", ".kube", ".docker"];
    let mut candidates: Vec<&str> = vec![command];
    for text in args.iter().chain(env.iter().map(|(_, value)| value)) {
        // `--flag=/path` as well as `/path`; a PATH-like list of folders too.
        let text = text.split_once('=').map_or(text.as_str(), |(_, value)| value);
        candidates.extend(text.split(':'));
    }
    let allowed = |path: &Path| {
        path.starts_with(home)
            && path != home
            && !TOO_BROAD.iter().any(|broad| path == home.join(broad))
            && !SECRET.iter().any(|secret| path.starts_with(home.join(secret)))
    };
    let mut found: Vec<PathBuf> = Vec::new();
    for candidate in candidates {
        let path = Path::new(candidate.trim());
        if !path.is_absolute() || !path.starts_with(home) || !path.exists() {
            continue;
        }
        let root = if path.is_dir() {
            path.to_path_buf()
        } else {
            let parent = path.parent().unwrap_or(path);
            parent
                .ancestors()
                .take_while(|ancestor| allowed(ancestor))
                .find(|ancestor| MARKERS.iter().any(|marker| ancestor.join(marker).exists()))
                .unwrap_or(parent)
                .to_path_buf()
        };
        if allowed(&root) && !found.iter().any(|known| root.starts_with(known)) {
            found.retain(|known| !known.starts_with(&root));
            found.push(root);
        }
    }
    found
}

/// Whether `command` can be found: an existing path, or a name on PATH.
fn program_exists(command: &str) -> bool {
    let path = std::path::Path::new(command);
    if path.components().count() > 1 {
        return path.is_file();
    }
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(command).is_file()))
        .unwrap_or(false)
}

/// Where a stdio server's program runs when it's sandboxed: the project is its writable folder, the
/// home folder is emptied except toolchains and package caches, and `writable` folders (a plugin's
/// own directory) are bound read-write.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Launch {
    /// The project folder; the current directory when there's no project.
    pub project: Option<std::path::PathBuf>,
    pub home: Option<std::path::PathBuf>,
    pub writable: Vec<std::path::PathBuf>,
}

impl Launch {
    /// What the current process sees.
    #[must_use]
    pub fn from_environment() -> Self {
        Self {
            project: std::env::current_dir().ok(),
            home: std::env::var_os("HOME").map(std::path::PathBuf::from),
            writable: Vec::new(),
        }
    }

    /// The launch for a project with re-code's installed plugins bound writable, read now, so a plugin
    /// installed a moment ago is visible to its own server.
    #[must_use]
    pub fn for_project(project: Option<std::path::PathBuf>, plugins_dir: &std::path::Path) -> Self {
        let writable = std::fs::read_dir(plugins_dir)
            .map(|entries| entries.flatten().map(|entry| entry.path()).filter(|path| path.is_dir()).collect())
            .unwrap_or_default();
        Self {
            project: project.or_else(|| std::env::current_dir().ok()),
            home: std::env::var_os("HOME").map(std::path::PathBuf::from),
            writable,
        }
    }

    /// The program and arguments that start `command args` for this server: wrapped in bubblewrap
    /// when the server allows it and bubblewrap works here, otherwise as given. The environment the
    /// server needs goes in through `--setenv`, so a key reaches only that server.
    #[must_use]
    pub fn command_for(
        &self,
        server: &McpServer,
        command: &str,
        args: &[String],
        env: &[(String, String)],
    ) -> (String, Vec<String>, Vec<(String, String)>, bool) {
        let sandboxed = server.sandbox == McpSandbox::Bubblewrap && recode_tools::sandbox::isolation().is_sandboxed();
        if !sandboxed {
            return (command.to_owned(), args.to_vec(), env.to_vec(), false);
        }
        let project = self.project.clone().unwrap_or_else(|| std::path::PathBuf::from("/tmp"));
        let mut grants: Vec<recode_tools::sandbox::Grant> = self
            .writable
            .iter()
            .map(|path| recode_tools::sandbox::Grant { path: path.clone(), writable: true })
            .collect();
        // What the server's own configuration names (its script, JAVA_HOME, a jar…) it may read.
        if let Some(home) = &self.home {
            for path in own_paths(command, args, env, home) {
                if !grants.iter().any(|grant| path.starts_with(&grant.path)) {
                    grants.push(recode_tools::sandbox::Grant { path, writable: false });
                }
            }
        }
        let policy = recode_tools::sandbox::Policy { network: true };
        let mut bwrap = recode_tools::sandbox::bwrap_args(&project, self.home.as_deref(), &[], policy, &grants);
        for (key, value) in env {
            bwrap.extend(["--setenv".to_owned(), key.clone(), value.clone()]);
        }
        bwrap.push(command.to_owned());
        bwrap.extend(args.iter().cloned());
        ("bwrap".to_owned(), bwrap, Vec::new(), true)
    }
}

/// Connects to a server (starting a stdio server's program). The caller must have the user's consent.
pub(crate) async fn connect(server: &McpServer, launch: &Launch) -> Result<McpClient, String> {
    match &server.transport {
        McpTransport::Stdio { command, args, env } => {
            let resolved: Vec<(String, String)> = resolve(env)?.into_iter().collect();
            // Inside bubblewrap a missing program only shows as a closed pipe, so it's checked here first.
            if !program_exists(command) {
                return Err(format!("Couldn't start {command}: it isn't installed (not found on PATH)."));
            }
            let (program, argv, direct_env, sandboxed) = launch.command_for(server, command, args, &resolved);
            let mut process = Command::new(&program);
            process.args(&argv).kill_on_drop(true);
            for (key, value) in direct_env {
                process.env(key, value);
            }
            // The server's own error output must never reach the terminal: drawn over the interface it
            // garbles the screen (a crashing Node server prints its whole stack). rmcp inherits stderr
            // unless told otherwise, so it's piped here, logged, and its last lines kept for errors.
            let (transport, stderr) =
                TokioChildProcess::builder(process).stderr(Stdio::piped()).spawn().map_err(|error| {
                    if sandboxed {
                        format!("Couldn't start {command} in the sandbox: {error}")
                    } else {
                        format!("Couldn't start {command}: {error}")
                    }
                })?;
            let tail = std::sync::Arc::new(std::sync::Mutex::new(std::collections::VecDeque::<String>::new()));
            let (finished, ended) = tokio::sync::oneshot::channel::<()>();
            if let Some(stderr) = stderr {
                let (tail, name) = (tail.clone(), server.name.clone());
                tokio::spawn(async move {
                    let _finished = finished;
                    use tokio::io::AsyncBufReadExt;
                    let mut lines = tokio::io::BufReader::new(stderr).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        tracing::debug!(server = %name, "{line}");
                        if let Ok(mut tail) = tail.lock() {
                            // The start of the output names the problem; the rest of a stack adds little.
                            if tail.len() < STDERR_TAIL {
                                tail.push_back(line.chars().take(300).collect());
                            }
                        }
                    }
                });
            }
            match ().serve(transport).await {
                Ok(client) => Ok(client),
                Err(error) => {
                    // A crash usually explains itself on stderr: wait for that output to end (briefly).
                    let _ = tokio::time::timeout(Duration::from_secs(3), ended).await;
                    let lines: Vec<String> = tail
                        .lock()
                        .map(|tail| tail.iter().map(|line| line.trim().to_owned()).filter(|l| !l.is_empty()).collect())
                        .unwrap_or_default();
                    // The line naming the error says the most (Node's comes before its stack); else the last.
                    let said = lines
                        .iter()
                        .find(|line| line.contains("Error") || line.contains("error:") || line.starts_with("panicked"))
                        .or(lines.last())
                        .map(|line| format!(" It said: {}.", line.trim_end_matches('.')))
                        .unwrap_or_default();
                    let hint = if sandboxed {
                        " It ran in bubblewrap: if it needs files or folders the sandbox hides, set its Sandbox to none in the form."
                    } else {
                        ""
                    };
                    Err(format!("The server didn't complete the MCP handshake: {error}.{said}{hint}"))
                }
            }
        }
        McpTransport::Http { url, headers } => {
            let mut config = StreamableHttpClientTransportConfig::with_uri(url.as_str());
            let mut custom = HashMap::new();
            for (key, value) in resolve(headers)? {
                if key.eq_ignore_ascii_case("authorization") {
                    config = config.auth_header(value.trim_start_matches("Bearer ").to_owned());
                } else {
                    let name =
                        HeaderName::try_from(key.as_str()).map_err(|_| format!("{key} isn't a valid header."))?;
                    // The value is deliberately left out of this message, as it may be a secret.
                    let value = HeaderValue::try_from(value).map_err(|_| format!("The value of {key} isn't valid."))?;
                    custom.insert(name, value);
                }
            }
            config = config.custom_headers(custom);
            let transport = StreamableHttpClientTransport::from_config(config);
            match ().serve(transport).await {
                Ok(client) => Ok(client),
                Err(error) => Err(explain_http_failure(url, &error.to_string()).await),
            }
        }
    }
}

/// Puts a failed HTTP connection in plain words. The MCP client reports a failed send without its
/// cause, so when that happens the address is tried directly to find out why.
async fn explain_http_failure(url: &str, error: &str) -> String {
    let lower = error.to_lowercase();
    if lower.contains("401") || lower.contains("unauthorized") || lower.contains("403") || lower.contains("forbidden") {
        return format!("{url} refused the request: it needs a valid key or token (set it as a header).");
    }
    if lower.contains("certificate") {
        return format!("{url} has a TLS certificate problem.");
    }
    if let Some(reason) = diagnose_address(url).await {
        return reason;
    }
    // Keep the readable end of the message, not the transport's type names.
    let readable = error.rsplit("error: ").next().unwrap_or(error);
    format!("Couldn't connect to {url}: {readable}")
}

/// Tries a plain TCP connection to the server's address, returning why it fails, if it does.
async fn diagnose_address(url: &str) -> Option<String> {
    let uri: http::Uri = url.parse().ok()?;
    let host = uri.host()?.to_owned();
    let port = uri.port_u16().unwrap_or(if uri.scheme_str() == Some("https") { 443 } else { 80 });
    let attempt = tokio::time::timeout(Duration::from_secs(3), tokio::net::TcpStream::connect((host.as_str(), port)));
    match attempt.await {
        Ok(Ok(_)) => None,
        Ok(Err(error)) if error.kind() == std::io::ErrorKind::ConnectionRefused => Some(format!(
            "Nothing is answering at {url}. If it's an IDE's MCP server, the IDE probably isn't running \
             (JetBrains IDEs also pick a new port each time they start)."
        )),
        Ok(Err(error)) if error.to_string().contains("lookup") || error.kind() == std::io::ErrorKind::NotFound => {
            Some(format!("Couldn't find {host}: the address's host name doesn't resolve."))
        }
        Ok(Err(error)) => Some(format!("Couldn't reach {url}: {error}.")),
        Err(_) => Some(format!("{url} didn't answer within 3 seconds.")),
    }
}

/// Tests a server. This starts a stdio server's program, so the caller must have the user's permission first.
pub async fn probe(server: &McpServer) -> McpProbe {
    probe_with(server, &Launch::from_environment()).await
}

/// Like [`probe`], with the stdio sandbox set up for a particular project and plugin folders.
pub async fn probe_with(server: &McpServer, launch: &Launch) -> McpProbe {
    let attempt = async { describe(connect(server, launch).await?).await };
    match tokio::time::timeout(TIMEOUT, attempt).await {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => McpProbe::Failed { error },
        Err(_) => McpProbe::Failed { error: format!("No answer after {} seconds.", TIMEOUT.as_secs()) },
    }
}

async fn describe(client: rmcp::service::RunningService<rmcp::RoleClient, ()>) -> Result<McpProbe, String> {
    let info = client.peer_info();
    let listed = client.list_all_tools().await.map_err(|error| format!("Connected, but listing tools failed: {error}"));
    // Shutting down is best effort: the answer is already in hand, and dropping it ends a child process anyway.
    let _ = client.cancel().await;
    let listed = listed?;
    let chars: usize =
        listed.iter().map(|tool| serde_json::to_string(tool).map_or(0, |text| text.chars().count())).sum();
    let tools = listed.into_iter().map(tool_info).collect();
    Ok(McpProbe::Connected {
        server_name: info.as_ref().and_then(|info| info.server_info.as_ref()).map(|server| server.name.clone()),
        server_version: info.as_ref().and_then(|info| info.server_info.as_ref()).map(|server| server.version.clone()),
        tools,
        estimated_schema_tokens: u64::try_from(chars / CHARS_PER_TOKEN).unwrap_or(u64::MAX),
    })
}

/// What re-code keeps about a listed tool: its name, description and argument schema.
pub(crate) fn tool_info(tool: rmcp::model::Tool) -> McpToolInfo {
    McpToolInfo {
        name: tool.name.into_owned(),
        description: tool.description.map(std::borrow::Cow::into_owned).unwrap_or_default(),
        input_schema: serde_json::Value::Object(tool.input_schema.as_ref().clone()),
    }
}

/// Replaces `${VAR}` values from the environment. A missing variable is an error, and its absence is not sent onward.
pub(crate) fn resolve(values: &BTreeMap<String, String>) -> Result<BTreeMap<String, String>, String> {
    values
        .iter()
        .map(|(key, value)| {
            if is_reference(value) {
                let name = &value[2..value.len() - 1];
                let resolved = std::env::var(name).map_err(|_| format!("{key} needs ${{{name}}}, which isn't set."))?;
                Ok((key.clone(), resolved))
            } else {
                Ok((key.clone(), value.clone()))
            }
        })
        .collect()
}

#[cfg(test)]
mod launch_tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn a_server_may_read_what_its_own_configuration_names_and_nothing_broad() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let repo = home.join(".local/share/rea/rea-git");
        std::fs::create_dir_all(repo.join("scripts")).unwrap();
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join("scripts/rea.mjs"), "").unwrap();
        let jdk = home.join("jdk-21");
        std::fs::create_dir_all(&jdk).unwrap();
        let loose = home.join("tools/thing.jar");
        std::fs::create_dir_all(loose.parent().unwrap()).unwrap();
        std::fs::write(&loose, "").unwrap();
        std::fs::create_dir_all(home.join(".ssh")).unwrap();
        std::fs::create_dir_all(home.join(".config")).unwrap();
        let args = vec![repo.join("scripts/rea.mjs").display().to_string(), "mcp".to_owned()];
        let env = vec![
            ("JAVA_HOME".to_owned(), jdk.display().to_string()),
            ("JAR".to_owned(), format!("--jar={}", loose.display())),
            ("KEYS".to_owned(), home.join(".ssh").display().to_string()),
            ("CONF".to_owned(), home.join(".config").display().to_string()),
            ("HOME_ITSELF".to_owned(), home.display().to_string()),
            ("ELSEWHERE".to_owned(), "/usr/lib".to_owned()),
        ];
        let paths = own_paths("/usr/bin/node", &args, &env, home);
        // The script's repository (marked by .git), the named JDK folder, the jar's own folder.
        assert_eq!(paths, vec![repo, jdk, home.join("tools")]);
    }

    fn server(sandbox: McpSandbox) -> McpServer {
        McpServer {
            name: "s".to_owned(),
            transport: McpTransport::Stdio {
                command: "npx".to_owned(),
                args: vec!["-y".to_owned(), "x".to_owned()],
                env: BTreeMap::new(),
            },
            enabled: true,
            expose_to_agent: false,
            external: None,
            tools: None,
            sandbox,
        }
    }

    #[test]
    fn a_sandboxed_server_runs_under_bwrap_with_its_env_set_inside() {
        let launch = Launch {
            project: Some("/proj".into()),
            home: Some("/home/x".into()),
            writable: vec!["/data/plugins/p".into()],
        };
        let env = vec![("TOKEN".to_owned(), "secret".to_owned())];
        let (program, argv, direct, sandboxed) =
            launch.command_for(&server(McpSandbox::Bubblewrap), "npx", &["-y".to_owned(), "x".to_owned()], &env);
        if recode_tools::sandbox::isolation().is_sandboxed() {
            assert!(sandboxed);
            assert_eq!(program, "bwrap");
            assert!(direct.is_empty(), "the environment goes in through --setenv, not the host process");
            let text = argv.join(" ");
            assert!(text.contains("--bind /proj /proj") && text.contains("--share-net"), "{text}");
            assert!(text.contains("--setenv TOKEN secret"), "{text}");
            assert!(text.ends_with("--setenv TOKEN secret npx -y x") || text.ends_with("npx -y x"), "{text}");
            assert!(
                !argv.iter().any(|a| a == "--bind") || !text.contains("--bind /data/plugins/p"),
                "a folder that doesn't exist isn't bound"
            );
        } else {
            assert!(!sandboxed && program == "npx");
        }
        // Opting out runs the program as given, with its environment on the process.
        let (program, argv, direct, sandboxed) =
            launch.command_for(&server(McpSandbox::None), "npx", &["-y".to_owned()], &env);
        assert!(!sandboxed);
        assert_eq!((program.as_str(), argv.len(), direct.len()), ("npx", 1, 1));
    }
}
