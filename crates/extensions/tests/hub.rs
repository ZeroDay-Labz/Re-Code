//! Tests the agent's MCP hub against a small real stdio MCP server written in POSIX shell: it keeps
//! the connection alive, lists tools, and calls one, returning its text.

use recode_core::extensions::{McpServer, McpTransport};
use recode_extensions::hub::McpHub;
use serde_json::json;
use std::collections::BTreeMap;

// Speaks just enough MCP: initialize, tools/list, and tools/call (echoes the `text` argument back).
const ECHO_SERVER: &str = r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -nE 's/.*"id":([0-9]+).*/\1/p')
  case "$line" in
    *'"initialize"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"echo-srv","version":"0.1.0"}}}\n' "$id" ;;
    *'"tools/list"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"echo","description":"Echoes text","inputSchema":{"type":"object","properties":{"text":{"type":"string"}}}}]}}\n' "$id" ;;
    *'"tools/call"'*)
      text=$(printf '%s' "$line" | sed -nE 's/.*"text":"([^"]*)".*/\1/p')
      printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"you said: %s"}]}}\n' "$id" "$text" ;;
  esac
done
"#;

fn server(name: &str, expose: bool) -> McpServer {
    McpServer {
        name: name.to_owned(),
        transport: McpTransport::Stdio {
            command: "sh".to_owned(),
            args: vec!["-c".to_owned(), ECHO_SERVER.to_owned()],
            env: BTreeMap::new(),
        },
        enabled: true,
        expose_to_agent: expose,
        external: None,
        tools: None,
        sandbox: recode_core::extensions::McpSandbox::default(),
    }
}

#[tokio::test]
async fn lists_and_calls_a_tool_through_the_hub() {
    let mut hub = McpHub::new(vec![server("echo", false)]);
    assert_eq!(hub.server_names(), vec!["echo".to_owned()]);

    let tools = hub.list(None).await.tools;
    assert_eq!(tools.len(), 1, "tools: {tools:?}");
    assert_eq!(tools[0].server, "echo");
    assert_eq!(tools[0].tool.name, "echo");

    let result = hub.call("echo", "echo", json!({"text": "hello"})).await.unwrap();
    assert!(result.contains("you said: hello"), "{result}");
}

#[tokio::test]
async fn a_disabled_server_is_not_in_the_hub() {
    let mut disabled = server("off", false);
    disabled.enabled = false;
    let hub = McpHub::new(vec![disabled]);
    assert!(hub.is_empty());
    assert!(hub.server_names().is_empty());
}

#[tokio::test]
async fn an_unknown_server_is_reported_not_panicked() {
    let mut hub = McpHub::new(vec![]);
    let error = hub.call("nope", "x", json!({})).await.unwrap_err();
    assert!(error.contains("nope"), "{error}");
}

#[tokio::test]
#[ignore = "runs REA via npx: needs node and network"]
async fn discovers_and_lists_real_rea_tools() {
    // The agent's discover-then-call path against the real REA stdio server (the plan's check).
    let rea = recode_extensions::registry::resolve(None, "https://github.com/morluto/rea").await.unwrap();
    let mut hub = McpHub::new(vec![rea]);
    let tools = hub.list(Some("rea")).await.tools;
    assert!(tools.len() > 50, "REA offers many tools; got {}", tools.len());
    assert!(tools.iter().any(|entry| entry.tool.name == "open_binary"), "expected open_binary among REA's tools");
}

#[tokio::test]
async fn an_exposed_server_offers_its_tools_directly() {
    let mut hub = McpHub::new(vec![server("echo", true)]);
    let defs = hub.direct_tool_defs().await;
    assert_eq!(defs.len(), 1, "defs: {defs:?}");
    assert_eq!(defs[0].name, "mcp__echo__echo");
}

#[tokio::test]
async fn tool_arguments_are_kept_so_the_model_can_see_them() {
    let mut hub = McpHub::new(vec![server("echo", true)]);
    let tools = hub.list(None).await.tools;
    assert_eq!(tools[0].tool.signature(), "echo(text?: string)");
    let defs = hub.direct_tool_defs().await;
    assert_eq!(defs[0].parameters["properties"]["text"]["type"], "string", "direct tools carry the real schema");
}

// Answers one tools/call, then exits, as a server does when it crashes or its IDE restarts.
const ONE_SHOT_SERVER: &str = r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -nE 's/.*"id":([0-9]+).*/\1/p')
  case "$line" in
    *'"initialize"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"one-shot","version":"0.1.0"}}}\n' "$id" ;;
    *'"tools/list"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"ping","description":"Pings","inputSchema":{"type":"object"}}]}}\n' "$id" ;;
    *'"tools/call"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"pong"}]}}\n' "$id"
      exit 0 ;;
  esac
done
"#;

#[tokio::test]
async fn a_server_that_went_away_is_reconnected_on_the_next_call() {
    let mut one_shot = server("flaky", false);
    one_shot.transport = McpTransport::Stdio {
        command: "sh".to_owned(),
        args: vec!["-c".to_owned(), ONE_SHOT_SERVER.to_owned()],
        env: BTreeMap::new(),
    };
    let mut hub = McpHub::new(vec![one_shot]);
    assert_eq!(hub.call("flaky", "ping", json!({})).await.unwrap(), "pong");
    // The process has exited; give the client a moment to notice the closed pipe.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(hub.call("flaky", "ping", json!({})).await.unwrap(), "pong", "a fresh connection was made");
}

#[tokio::test]
async fn an_unreachable_server_is_named_once_and_the_others_still_work() {
    let down = McpServer {
        name: "goland".to_owned(),
        // Nothing listens on port 9 of localhost (the discard port is never served here).
        transport: McpTransport::Http { url: "http://127.0.0.1:9/stream".to_owned(), headers: BTreeMap::new() },
        enabled: true,
        expose_to_agent: false,
        external: None,
        tools: None,
        sandbox: recode_core::extensions::McpSandbox::default(),
    };
    let mut hub = McpHub::new(vec![server("echo", false), down]);
    let listing = hub.list(None).await;
    assert_eq!(listing.tools.len(), 1, "echo still lists: {listing:?}");
    assert_eq!(listing.unavailable.len(), 1);
    assert_eq!(listing.unavailable[0].0, "goland");
}

#[tokio::test]
async fn a_changed_address_means_the_hub_is_rebuilt() {
    let mut first = server("ide", false);
    first.transport = McpTransport::Http { url: "http://127.0.0.1:64522/stream".to_owned(), headers: BTreeMap::new() };
    let hub = McpHub::new(vec![first.clone()]);
    assert!(hub.matches(&[first.clone()]));
    let mut moved = first;
    moved.transport = McpTransport::Http { url: "http://127.0.0.1:61234/stream".to_owned(), headers: BTreeMap::new() };
    assert!(!hub.matches(&[moved]), "same name, new port");
}

#[tokio::test]
#[ignore = "needs RustRover running with its MCP server; reads ~/.claude.json"]
async fn the_running_rustrover_mcp_lists_tools_with_their_arguments() {
    let home = std::env::var_os("HOME").unwrap();
    let servers = recode_extensions::claude::servers(&std::path::Path::new(&home).join(".claude.json"), None);
    let rustrover = servers.into_iter().find(|s| s.name == "rustrover").expect("rustrover in ~/.claude.json");
    let mut hub = McpHub::new(vec![rustrover]);
    let listing = hub.list(Some("rustrover")).await;
    assert!(listing.unavailable.is_empty(), "{:?}", listing.unavailable);
    assert!(listing.tools.len() > 10, "got {}", listing.tools.len());
    let with_arguments = listing.tools.iter().filter(|t| t.tool.signature().contains(':')).count();
    eprintln!(
        "RUSTROVER {} tools, {with_arguments} with arguments; e.g. {}",
        listing.tools.len(),
        listing.tools.iter().take(3).map(|t| t.tool.signature()).collect::<Vec<_>>().join(" | ")
    );
    assert!(with_arguments > 0);
}
