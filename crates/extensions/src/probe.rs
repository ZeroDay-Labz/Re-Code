//! Connects to an MCP server, asks what it offers, and disconnects.

use http::{HeaderName, HeaderValue};
use recode_core::extensions::{McpProbe, McpServer, McpToolInfo, McpTransport, is_reference};
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

/// A connected MCP client.
pub(crate) type McpClient = rmcp::service::RunningService<rmcp::RoleClient, ()>;

/// Connects to a server (starting a stdio server's program). The caller must have the user's consent.
pub(crate) async fn connect(server: &McpServer) -> Result<McpClient, String> {
    match &server.transport {
        McpTransport::Stdio { command, args, env } => {
            let mut process = Command::new(command);
            process.args(args).stderr(Stdio::null()).kill_on_drop(true);
            for (key, value) in resolve(env)? {
                process.env(key, value);
            }
            let transport =
                TokioChildProcess::new(process).map_err(|error| format!("Couldn't start {command}: {error}"))?;
            ().serve(transport).await.map_err(|error| format!("The server didn't complete the MCP handshake: {error}"))
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
    let attempt = async { describe(connect(server).await?).await };
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
