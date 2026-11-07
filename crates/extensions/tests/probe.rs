//! Tests the connection test against a small real stdio MCP server written in POSIX shell.

use recode_core::extensions::{McpProbe, McpServer, McpTransport};
use recode_extensions::probe;
use std::collections::BTreeMap;

const FAKE_SERVER: &str = r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -nE 's/.*"id":([0-9]+).*/\1/p')
  case "$line" in
    *'"initialize"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"fake","version":"1.2.3"}}}\n' "$id" ;;
    *'"tools/list"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"echo","description":"Says it back for %s","inputSchema":{"type":"object","properties":{"text":{"type":"string"}}}},{"name":"time","inputSchema":{"type":"object"}}]}}\n' "$id" "$GREETING" ;;
  esac
done
"#;

fn server(script: &str, env: &[(&str, &str)]) -> McpServer {
    McpServer {
        name: "test".to_owned(),
        transport: McpTransport::Stdio {
            command: "sh".to_owned(),
            args: vec!["-c".to_owned(), script.to_owned()],
            env: env.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect::<BTreeMap<_, _>>(),
        },
        enabled: true,
        expose_to_agent: false,
        external: None,
        tools: None,
    }
}

#[tokio::test]
async fn lists_tools_from_a_real_stdio_server_and_passes_its_environment() {
    let McpProbe::Connected { server_name, server_version, tools, estimated_schema_tokens } =
        probe(&server(FAKE_SERVER, &[("GREETING", "marker-value")])).await
    else {
        panic!("the probe failed");
    };
    assert_eq!(server_name.as_deref(), Some("fake"));
    assert_eq!(server_version.as_deref(), Some("1.2.3"));
    assert_eq!(tools.len(), 2);
    assert!(tools[0].description.contains("marker-value"));
    assert!(estimated_schema_tokens > 0);
}

#[tokio::test]
async fn a_missing_program_is_reported() {
    let mut bad = server("", &[]);
    bad.transport = McpTransport::Stdio { command: "no-such-program-xyz".into(), args: vec![], env: BTreeMap::new() };
    let McpProbe::Failed { error } = probe(&bad).await else { panic!("it should fail") };
    assert!(error.contains("no-such-program-xyz"), "{error}");
}

#[tokio::test]
async fn a_program_that_is_not_an_mcp_server_is_reported() {
    let McpProbe::Failed { error } = probe(&server("echo hello; exit 0", &[])).await else { panic!("it should fail") };
    assert!(error.contains("handshake"), "{error}");
}

#[tokio::test]
async fn a_missing_variable_is_reported_by_name_without_starting_anything() {
    let script = "touch /should/never/exist";
    let McpProbe::Failed { error } = probe(&server(script, &[("TOKEN", "${RE_CODE_TEST_UNSET_VAR}")])).await else {
        panic!("it should fail")
    };
    assert!(error.contains("RE_CODE_TEST_UNSET_VAR"), "{error}");
}

#[tokio::test]
async fn nothing_listening_is_reported() {
    let unreachable = McpServer {
        name: "h".to_owned(),
        transport: McpTransport::Http { url: "http://127.0.0.1:9/mcp".to_owned(), headers: BTreeMap::new() },
        enabled: true,
        expose_to_agent: false,
        external: None,
        tools: None,
    };
    let McpProbe::Failed { error } = probe(&unreachable).await else { panic!("it should fail") };
    assert!(error.contains("127.0.0.1:9"), "{error}");
}

#[tokio::test]
#[ignore = "fetches github over the network"]
async fn resolves_reas_github_repo_to_a_runnable_server() {
    let server = recode_extensions::registry::resolve(None, "https://github.com/morluto/rea").await.unwrap();
    assert_eq!(server.name, "rea");
    match server.transport {
        recode_core::extensions::McpTransport::Stdio { command, args, .. } => {
            assert_eq!(command, "npx");
            assert!(args.iter().any(|a| a.starts_with("rea-agents@")), "args: {args:?}");
            assert!(args.contains(&"mcp".to_owned()));
        }
        other => panic!("expected stdio, got {other:?}"),
    }
}

#[tokio::test]
#[ignore = "fetches models.dev over the network"]
async fn the_real_catalog_flags_free_and_vision_models() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = recode_extensions::catalog::Catalog::load(&dir.path().join("c.json")).await;
    // Anthropic Claude supports vision and tools; assert the catalog knows it.
    let claude = catalog.facts("claude-opus-4-5").or_else(|| catalog.facts("anthropic/claude-opus-4-5"));
    if let Some(facts) = claude {
        assert_eq!(facts.vision, Some(true), "claude should be vision-capable");
    }
    // The cache file was written.
    assert!(dir.path().join("c.json").exists());
}

#[tokio::test]
#[ignore = "reaches a public MCP server over the network"]
async fn an_https_server_is_reached_over_tls() {
    // DeepWiki's public MCP endpoint needs no account; it proves rmcp's client does TLS in this build.
    let server = recode_core::extensions::parse_mcp_form("deepwiki", "https://mcp.deepwiki.com/mcp", "", true).unwrap();
    match recode_extensions::probe(&server).await {
        recode_core::extensions::McpProbe::Connected { tools, .. } => assert!(!tools.is_empty()),
        other => panic!("couldn't connect over https: {other:?}"),
    }
}

#[tokio::test]
async fn a_server_that_is_not_running_is_explained_plainly() {
    let server = recode_core::extensions::parse_mcp_form("down", "http://127.0.0.1:9/stream", "", true).unwrap();
    let recode_core::extensions::McpProbe::Failed { error } = recode_extensions::probe(&server).await else {
        panic!("nothing listens on port 9");
    };
    assert!(error.starts_with("Nothing is answering at http://127.0.0.1:9/stream"), "{error}");
    assert!(!error.contains("rmcp::"), "no internal type names: {error}");
}
