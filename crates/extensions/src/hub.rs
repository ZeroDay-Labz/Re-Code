//! A live connection pool for MCP servers, so the agent can discover and call their tools.
//!
//! Servers connect lazily on first use and stay connected for the session. A server that can't be
//! reached is reported and skipped; the rest keep working, and it's tried again next time. A dropped
//! connection is re-established, but a call is only repeated when it provably never reached the
//! server, since a tool may have side effects. The default path is discover-then-call (two meta
//! tools), which keeps huge servers like REA out of the request until a tool is actually needed.

use crate::probe::{McpClient, connect, tool_info};
use recode_core::extensions::{McpServer, McpToolInfo};
use rmcp::model::CallToolRequestParams;
use rmcp::service::ServiceError;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(45);
const CALL_TIMEOUT: Duration = Duration::from_secs(120);

/// One MCP tool, with the server it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubTool {
    pub server: String,
    pub tool: McpToolInfo,
}

/// What listing found: the tools of servers that answered, and why the others didn't.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Listing {
    pub tools: Vec<HubTool>,
    /// Server name and the reason it couldn't be reached.
    pub unavailable: Vec<(String, String)>,
}

/// Keeps MCP servers connected for the agent.
pub struct McpHub {
    launch: crate::probe::Launch,
    servers: BTreeMap<String, McpServer>,
    clients: HashMap<String, McpClient>,
    listed: HashMap<String, Vec<McpToolInfo>>,
}

impl McpHub {
    /// Creates a hub over the enabled, usable servers. Nothing connects until it's used.
    #[must_use]
    pub fn new(servers: Vec<McpServer>) -> Self {
        Self::with_launch(servers, crate::probe::Launch::from_environment())
    }

    /// A hub whose stdio servers run in the sandbox set up for `launch` (the project, the plugin folders).
    #[must_use]
    pub fn with_launch(servers: Vec<McpServer>, launch: crate::probe::Launch) -> Self {
        let servers = servers
            .into_iter()
            .filter(|server| server.enabled && server.external.as_ref().is_none_or(|e| e.unsupported.is_none()))
            .map(|server| (server.name.clone(), server))
            .collect();
        Self { launch, servers, clients: HashMap::new(), listed: HashMap::new() }
    }

    /// The names of the known (enabled) servers, sorted.
    #[must_use]
    pub fn server_names(&self) -> Vec<String> {
        self.servers.keys().cloned().collect()
    }

    /// Whether the hub was built from exactly these servers (names *and* settings), so a changed
    /// address — JetBrains IDEs pick a new port each launch — causes a rebuild.
    #[must_use]
    pub fn matches(&self, servers: &[McpServer]) -> bool {
        let other = McpHub::new(servers.to_vec());
        other.servers == self.servers
    }

    /// Whether any server is configured.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.servers.is_empty()
    }

    async fn ensure(&mut self, name: &str) -> Result<(), String> {
        // `is_transport_closed` sees a dead connection as soon as its loop ends, before anything is sent.
        if self.clients.get(name).is_some_and(|client| !client.is_closed() && !client.is_transport_closed()) {
            return Ok(());
        }
        // A connection that closed (the server exited, the IDE restarted) is replaced.
        self.clients.remove(name);
        let server = self.servers.get(name).ok_or_else(|| format!("no MCP server called '{name}'"))?.clone();
        let client = tokio::time::timeout(CONNECT_TIMEOUT, connect(&server, &self.launch))
            .await
            .map_err(|_| format!("{name} didn't answer within {}s", CONNECT_TIMEOUT.as_secs()))??;
        let tools = client
            .list_all_tools()
            .await
            .map_err(|error| format!("{name} connected, but listing its tools failed: {error}"))?;
        // Only a server that listed its tools counts as connected; otherwise it's tried again next time.
        self.listed.insert(name.to_owned(), tools.into_iter().map(tool_info).collect());
        self.clients.insert(name.to_owned(), client);
        Ok(())
    }

    /// Lists tools for one server, or all of them. Connects as needed.
    pub async fn list(&mut self, server: Option<&str>) -> Listing {
        let targets: Vec<String> = match server {
            Some(name) => vec![name.to_owned()],
            None => self.server_names(),
        };
        let mut listing = Listing::default();
        for name in targets {
            match self.ensure(&name).await {
                Ok(()) => {
                    let allows = |tool: &str| self.servers.get(&name).is_none_or(|server| server.allows_tool(tool));
                    for tool in self.listed.get(&name).into_iter().flatten().filter(|tool| allows(&tool.name)) {
                        listing.tools.push(HubTool { server: name.clone(), tool: tool.clone() });
                    }
                }
                Err(error) => listing.unavailable.push((name, error)),
            }
        }
        listing
    }

    /// The tool schemas for servers marked `expose_to_agent`, named `mcp__<server>__<tool>`.
    pub async fn direct_tool_defs(&mut self) -> Vec<recode_core::message::ToolDefinition> {
        let exposed: Vec<String> =
            self.servers.values().filter(|server| server.expose_to_agent).map(|server| server.name.clone()).collect();
        let mut defs = Vec::new();
        for name in exposed {
            if self.ensure(&name).await.is_err() {
                continue;
            }
            let allows = |tool: &str| self.servers.get(&name).is_none_or(|server| server.allows_tool(tool));
            for tool in self.listed.get(&name).into_iter().flatten().filter(|tool| allows(&tool.name)) {
                let parameters = if tool.input_schema.is_object() {
                    tool.input_schema.clone()
                } else {
                    serde_json::json!({"type": "object"})
                };
                defs.push(recode_core::message::ToolDefinition {
                    name: format!("mcp__{name}__{}", tool.name),
                    description: tool.description.clone(),
                    parameters,
                });
            }
        }
        defs
    }

    /// Calls a tool on a server, returning its text output.
    pub async fn call(&mut self, server: &str, tool: &str, arguments: Value) -> Result<String, String> {
        let arguments = match arguments {
            Value::Object(map) => Some(map),
            Value::Null => None,
            other => Some([("value".to_owned(), other)].into_iter().collect()),
        };
        let mut params = CallToolRequestParams::new(tool.to_owned());
        if let Some(arguments) = arguments {
            params = params.with_arguments(arguments);
        }
        if self.servers.get(server).is_some_and(|entry| !entry.allows_tool(tool)) {
            return Err(format!("{tool} is switched off for {server} in re-code's tool filter."));
        }
        let mut retried = false;
        loop {
            self.ensure(server).await?;
            let client = self.clients.get(server).ok_or_else(|| format!("{server} isn't connected"))?;
            let outcome = tokio::time::timeout(CALL_TIMEOUT, client.call_tool(params.clone()))
                .await
                .map_err(|_| format!("{server}/{tool} didn't finish within {}s", CALL_TIMEOUT.as_secs()))?;
            let result = match outcome {
                Ok(result) => result,
                // The request never left, so it's safe to reconnect and send it once more.
                Err(ServiceError::TransportSend(error)) if !retried => {
                    tracing::info!(%server, %error, "MCP send failed; reconnecting");
                    self.clients.remove(server);
                    retried = true;
                    continue;
                }
                Err(ServiceError::TransportClosed) => {
                    self.clients.remove(server);
                    return Err(format!(
                        "The connection to {server} closed during {tool}, so it may or may not have run. \
                         Check its effect before trying again."
                    ));
                }
                Err(error) => {
                    if matches!(error, ServiceError::TransportSend(_)) {
                        self.clients.remove(server);
                    }
                    return Err(format!("{server}/{tool} failed: {error}"));
                }
            };
            let text: String = result
                .content
                .iter()
                .filter_map(|block| block.as_text().map(|text| text.text.as_str()))
                .collect::<Vec<_>>()
                .join("\n");
            let text = if text.is_empty() { "(the tool returned no text)".to_owned() } else { text };
            return if result.is_error.unwrap_or(false) { Err(text) } else { Ok(text) };
        }
    }

    /// The listed tool, if its server has been listed.
    #[must_use]
    pub fn tool(&self, server: &str, tool: &str) -> Option<&McpToolInfo> {
        self.listed.get(server)?.iter().find(|info| info.name == tool)
    }

    /// Disconnects every server. Best effort.
    pub async fn shutdown(self) {
        for (_, client) in self.clients {
            let _ = client.cancel().await;
        }
    }
}
