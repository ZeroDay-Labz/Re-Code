# IDE Integration

Status: **proposed, awaiting agreement.** Implementation is Milestone 6, with nothing built yet.

## The main finding

The spec suggests a lightweight JetBrains plugin. **We may not need one for a long time,** because
JetBrains IDEs now offer two standard integration points running in opposite directions:

| Mechanism | Direction | Gives us |
| --- | --- | --- |
| **MCP server** (built into the IDE) | re-code → IDE | IDE intelligence as tools the agent can use |
| **ACP** (agent hosted by the IDE) | IDE → re-code | re-code as an agent inside the IDE's AI chat |

A Kotlin plugin is a separate codebase, versioned against changing IDE APIs and distributed through a
separate channel. It should be justified by a real gap that those two can't cover.

## 1. JetBrains MCP: the IDE as a tool provider

**Documented:** built into IntelliJ-based IDEs since 2025.2. Enabled under Settings → Tools → MCP Server.
Supports SSE, stdio and HTTP stream transports. Offers a "brave mode" that runs shell commands and run
configurations without confirmation.

**Observed on this machine:**

- RustRover (2026.3 is the newest config directory present) answers at `http://127.0.0.1:64522/stream`, over HTTP with no auth headers in the client config.
- PyCharm, WebStorm and GoLand are configured on different ports (64462, 64542, 64422). They refused
  connections during this session, so they likely weren't running.
- **Ports vary per IDE,** and the documentation doesn't specify how they're chosen. Reliable discovery is
  an open question. Fallbacks include user configuration and probing.
- RustRover's tool list is big, covering these and more: `get_file_problems`, `search_symbol`,
  `rename_refactoring`, `build_project`, `execute_run_configuration`, `get_symbol_info`, `analyze_calls`,
  `lint_files`, `get_all_open_file_paths`, `open_file_in_editor`, `xdebug_*`, database tools, and terminal.

**Consequences:**

- **Tool lists differ by IDE, version and plugin,** so always read `tools/list` and never hard-code
  capabilities. Tool names are data.
- **Don't load all of them.** A list this size is a clear example of the context inflation problem. The
  engine exposes a small curated group (diagnostics, symbol lookup, rename, run tests) and discovers the
  others on demand.
- **Local HTTP with no visible authentication is a trust issue** (see [SECURITY](SECURITY.md)).
- **All IDE tools pass through our permission engine.** IDE terminal and run-configuration tools count as
  shell execution, and they must not be a way around our sandbox and approvals. This applies especially if
  the user has brave mode enabled.

**The biggest benefit:** IDE-grade semantic refactoring and diagnostics for a model. `rename_refactoring`
is far safer than a model editing every reference with text replacement. When the IDE is available, prefer it.

## 2. ACP: re-code inside the IDE

**Documented:** JetBrains IDEs launch ACP agents as subprocesses. Custom agents are registered in
`~/.jetbrains/acp.json`, and this doesn't need a JetBrains AI subscription:

```json
{
  "agent_servers": {
    "re-code": { "command": "/usr/bin/re-code", "args": ["acp"] }
  }
}
```

The IDE can also pass MCP settings to the agent (`use_custom_mcp`, `use_idea_mcp`,
`idea_mcp_allowed_tools`). The official `agent-client-protocol` Rust crate is at version 5.0.

**This fits the architecture:** an ACP server is another engine client, mapping `Command` and `Event` to the
protocol. The engine remains the source of truth for sessions, models and permissions. Approval requests
appear in the IDE's UI, and sessions are visible in the TUI through the shared store.

**To verify in a spike:** how ACP handles diffs and edits in this IDE version, whether permission prompts
are expressive enough, whether RustRover and PyCharm behave identically, and how session resume behaves.

## 3. Lightweight detection without either

Useful, cheap, and can arrive early:

- `TERMINAL_EMULATOR=JetBrains-JediTerm` shows we're inside a JetBrains terminal (observed in this session).
- Opening a file at a line can use the IDE's command-line launcher, if present.
- The workspace root comes from the current directory.

## 4. Plugin: only for proven gaps

Possible justifications, each to be checked against MCP and ACP first:

- Live selection and caret position, if not available.
- Pushing IDE events (saved file, new diagnostics) to a running session.
- A review experience that ACP doesn't support.

If one is built, it's a **thin client**. It holds no sessions, no providers, and no permission logic.

## 5. IPC, if and when needed

Direction of a local connection: a client (plugin or second TUI) connects to a running re-code.

- **Linux:** Unix domain socket at `$XDG_RUNTIME_DIR/re-code/<id>.sock`.
- **Access control:** the directory is mode `0700`, and we also verify `SO_PEERCRED` matches our UID.
- **Protocol:** JSON-RPC-style framing over serialised `Command` and `Event`, with a version handshake.
- **Windows, later:** named pipes behind the same interface.
- **No TCP listener.** That avoids a network-reachable service, and browsers can't reach a Unix socket.

This is also how D4 (daemon or in-process) could evolve without an engine redesign.

## Other editors

MCP and ACP aren't JetBrains-specific. Zed and other ACP hosts should need little beyond compatibility
testing, and a generic LSP client for diagnostics helps when no IDE is running. LSP arrives after the
JetBrains path has proved itself, because running language servers ourselves adds a meaningful lifecycle to
manage.

## Delivery order

1. Connect as an MCP client to JetBrains, with a curated, permission-checked tool group.
2. `re-code acp`, tested in RustRover and PyCharm.
3. Workflow polish: run tests through the IDE, jump to source, review changes.
4. Reassess whether a plugin is needed, based on the actual gaps found.
