# Architecture

Status: **proposed, awaiting agreement.** Nothing here is implemented.

Evidence labels used across these documents:

- **Observed**: seen on the development machine on 2026-11-06.
- **Documented**: stated in vendor documentation.
- **To verify**: believed true, and must be tested before we rely on it.

## Principles

1. **The engine has no idea a terminal exists.** The TUI is one client of it.
2. **Everything that happens is an event,** and events are written down before they are shown.
3. **A boundary needs a reason.** A crate exists for a compile-time guarantee or a real second
   implementation, not to fill out a diagram.
4. **Provider differences are preserved,** not flattened.

## System overview

```text
   TUI        headless `run`     ACP server (later)     UDS clients (later)
    │               │                   │                        │
    └───────────────┴─────────┬─────────┴────────────────────────┘
                 Commands ↓   │   ↑ Events (serialisable, replayable)
                    ┌─────────┴──────────┐
                    │       Engine       │  one actor per session
                    │  loop · context    │
                    │  permissions       │
                    └─┬──────┬─────────┬─┘
                      │      │         │
                 Providers  Tools    Store (SQLite: event log, FTS)
                      │      │
       Ollama, OpenAI-compat, …    fs · search · git · shell (sandboxed)
```

Each session is a Tokio task that owns its state. Clients send `Command`s (user message, interrupt,
approval response) and receive `Event`s (text delta, tool started, approval required, usage report).
Both are plain serde types, which means a future socket or ACP client needs no new engine design.

Three properties follow:

- **Interrupting works at any moment.** Input handling never waits on the engine. Every turn holds a
  cancellation token, and shell commands run in their own process group so the group can be killed.
- **Crash recovery is replay.** The event log is the source of truth, so a session rebuilds from disk
  and an interrupted turn shows up as interrupted.
- **The TUI can be tested without a model,** by feeding it recorded events.

## Workspace structure

The spec's 19-crate proposal is a good map of subsystems but too many crates to start with. Most of
those boundaries are guesses until a second implementation exists, and every crate adds friction to
each refactor. Seven in total at the start. `core`, `providers`, `store`, `tui`, `extensions` and `app` exist now. `tools` and `engine` will
be created when Milestone 1 gives them real content, as the spec asks for no empty crates:

```text
re-code/
├── Cargo.toml                 workspace
├── crates/
│   ├── core/                  ids, Command/Event, messages, capabilities. No I/O.
│   ├── providers/             Provider trait, Ollama adapter, tool-dialect parsers
│   ├── runtime/               finds inference runtimes: containers, units, processes, probes
│   ├── tools/                 fs, search, git, shell, sandbox, checkpoints
│   ├── store/                 SQLite, migrations, event log, FTS
│   ├── engine/                agent loop, context engine, permissions
│   ├── tui/                   ratatui interface
│   └── app/                   the `re-code` binary: CLI, config, wiring
├── docs/
└── packaging/rpm/
```

```text
app ──► tui ──► core
 │               ▲
 └──► engine ────┤
        ├─► providers ─► core
        ├─► tools ─────► core
        └─► store ─────► core
```

**The important rule: `tui` depends only on `core`.** The compiler then guarantees the interface
can't run a tool or call a provider directly, which keeps other clients possible.

| Later crate | Arrives | Justification |
| --- | --- | --- |
| `runtime` | **Exists (2026-11-07)** | Discovery of Ollama, llama.cpp, vLLM and LM Studio through podman/docker, systemd/Quadlet, `/proc` and identify-by-response probes; read-only, every step time-boxed. Hardware detection still lives in `app/doctor` until a second caller needs it. |
| `extensions` | **Exists** | Stores MCP servers, skills and custom tools, and tests MCP connections. Keeps the `rmcp` dependency isolated. |
| `harness` | With the first external agent | Separate concept from providers |
| `ide` (ACP, IPC) | M6 | Separate protocol surface |

`context` and `permissions` start as modules in `engine`, since they are pure logic with one caller.
`platform` is not a crate: platform-specific code sits behind small traits or `cfg` modules where it
is used, and is extracted if Windows work shows a real need.

## Key dependencies

Versions checked on crates.io on 2026-11-06. **Licences must be confirmed by `cargo-deny` in CI**
when each is added. That check also enforces the licence allowlist transitively.

| Purpose | Choice | Version | Why, and what was rejected |
| --- | --- | --- | --- |
| Async | `tokio` | 1.54 | The ecosystem default. `tokio-util` provides cancellation tokens. |
| HTTP | `reqwest` | 0.13 | Streaming bodies. **Built without TLS for now:** local runtimes use plain HTTP, so HTTPS endpoints don't work yet. rustls arrives with cloud providers, and its crypto provider gets chosen with packaging in mind. |
| TUI | `ratatui` + `crossterm` | 0.30 / 0.29 | The obvious pairing, and we own the layout. |
| Text input | Our own widget | n/a | The input box is the most-used component in the product. `ratatui-textarea` (0.9) is a fair stopgap if the first cut slips. |
| Markdown | `pulldown-cmark` | 0.13 | Event-based parser that suits incremental rendering. |
| Highlighting | `syntect` | 5.3 | Many ready grammars. Tree-sitter is better for code intelligence, so it comes when code navigation does, not for colouring. |
| Diff | `similar` | 3.2 | Mature, with inline change detection. |
| Fuzzy matching | `nucleo` | 0.5 | Command palette and file picker. MPL-2.0, which is fine for use as a dependency but must be on the allowlist. |
| Search | `ignore` + `grep-searcher` | 0.4 / 0.1 | The libraries inside ripgrep. No `rg` runtime dependency. |
| Storage | `rusqlite` (bundled, FTS5) | 0.41 | Simple and synchronous behind one writer task. `sqlx` adds macro and async complexity that one local database doesn't need. |
| Migrations | Our own, about 15 lines | n/a | Ordered SQL and `PRAGMA user_version`. It also refuses a database from a newer version. No dependency is justified for this. |
| CLI | `clap` + `clap_complete` | 4.6 | Also generates shell completions. |
| Logging | `tracing` + `tracing-appender` | 0.1 / 0.2 | Structured logs to a file. Logging to stdout would corrupt the TUI. |
| Config | `toml` + `serde` | 1.2 / 1.0 | Human-editable. |
| Errors | `thiserror`, `anyhow` in `app` | current | Typed errors in libraries. |
| Git | The `git` command | n/a | Honours the user's config, hooks and credentials exactly. `gix` can be added later if status polling proves slow. |
| Testing | `insta`, `wiremock`, `cargo-nextest`, `cargo-deny` | 1.49 / 0.6 | Snapshots for the TUI, HTTP mocks for providers. |
| MCP | `rmcp` | 3.9 | The official Rust SDK. **Built without TLS for now,** so https:// servers are refused with a clear message. |
| Later: ACP | `agent-client-protocol` | 5.0 | Official crate. |
| Later: secrets | `keyring` or `oo7` | 4.2 / 0.7 | Secret Service is running on this machine (observed, via KWallet). Choose at M4. |
| Later: sandbox | `bwrap`, `landlock` | 0.12 (rpm) / 0.4 | See [SECURITY](SECURITY.md). |
| Later: hardware | `sysinfo`, direct sysfs | 0.40 | sysfs gives AMD VRAM without vendor libraries. |

## Configuration and data

XDG paths. Nothing requires root.

| Purpose | Path |
| --- | --- |
| User config | `~/.config/re-code/config.toml` |
| Project config | `<project>/.re-code/config.toml` (inert until the project is trusted) |
| Data, database, checkpoints | `~/.local/share/re-code/` |
| Logs | `~/.local/state/re-code/logs/` |
| Cache | `~/.cache/re-code/` |
| Runtime sockets | `$XDG_RUNTIME_DIR/re-code/` |

## Fedora RPM packaging

**Observed:** `cargo-generate-rpm` is installed. `rpm-build`, `mock` and `rust-packaging` are not.

| Option | Good for | Limits |
| --- | --- | --- |
| `cargo-generate-rpm` | Fast local RPMs, dogfooding from M1 | Not a source RPM, so unsuitable for COPR or Fedora builds |
| Spec file + vendored crates, built in `mock`, shipped on **COPR** | **The real first distribution** | Vendored crates mean we own security updates |
| Official Fedora package | Long term | Fedora policy expects Rust dependencies packaged individually, which is a heavy burden for this dependency tree (to verify current policy) |

**Proposal:** use `cargo-generate-rpm` early, then a proper spec file with COPR for the first
release, and consider official Fedora packaging once the project is stable.

The package will install the binary to `/usr/bin`, completions to the standard bash, zsh and fish
locations, and man pages generated from `clap`. **It must not touch `~/.config` or `~/.local/share`
on removal:** user data stays. Bundled SQLite must be declared with `Provides: bundled(sqlite)`.
Upgrades run database migrations on the next launch, not in an RPM scriptlet, since the database
belongs to the user rather than the system.

To set up when this work begins:

```bash
sudo dnf install rpm-build rpmlint mock rust-packaging
```

Whether `/usr/bin/recode` conflicts with Fedora's GNU `recode` package **was not confirmed**, as the
`dnf` query did not finish. It's relevant if the final name (D1) drops the hyphen.

## Testing strategy

| Layer | Approach |
| --- | --- |
| `core`, context, permissions | Unit and property tests. Pure logic gets the strictest coverage. |
| Providers | `wiremock` with **fixtures recorded from real servers**, covering streaming, tool calls, malformed and truncated responses, and cancellation |
| Tools | Real temp directories and git repos. Includes path escapes via symlink and `..`. |
| Engine | A scripted mock provider driving full turns, including kill-and-recover |
| TUI | `insta` snapshots from recorded events at 60, 100 and 160 columns, plus scripted `tmux` end-to-end runs against the real binary |
| **Context budget** | A failing test if baseline overhead in an empty project exceeds budget |
| **Live model conformance** | Opt-in `#[ignore]`d tests (`cargo test -- --ignored`), because they need a running model and GPU. Results are recorded per model, machine and date. |
| Upgrades | Migration tests against databases from every earlier schema version |

CI gates: `cargo fmt --check`, `clippy -D warnings`, `cargo nextest`, and `cargo deny`.

A provider or runtime is listed as **supported** only after it passes the conformance suite against
a real server, in line with the spec.

## Decisions needed

These are the significant choices I would rather not make silently. Each includes my recommendation.

| # | Decision | Recommendation | Main trade-off |
| --- | --- | --- | --- |
| D1 | ~~Final name.~~ **Decided (2026-11-07):** `re-code` stays, for the binary and the package. Fedora's GNU `recode` is a different name (`recode` vs `re-code`), so there is no file conflict; the RPM declares no `Conflicts:`. | | |
| D2 | ~~Full-screen vs inline TUI.~~ **Decided by the developer (2026-11-06):** full-screen, fitting and looking good at any terminal size. See [TUI_DESIGN](TUI_DESIGN.md#the-main-decision-full-screen-or-inline). | | We must implement scrollback, selection and search ourselves |
| D3 | ~~Merge chat and agent milestones.~~ **Decided by the developer (2026-11-06):** merged. See [ROADMAP](ROADMAP.md). | | Longer until a first demo, but the demo tests the real risk |
| D4 | **Daemon or in-process engine.** Proceeding with the recommendation, as no objection was raised: in-process now, with a serialisable boundary. A socket server comes only when an IDE needs it. | | A background daemon would allow sessions to outlive the terminal, but adds lifecycle, upgrade and security complexity on day one |
| D5 | **OS sandbox for shell commands.** Proceeding with the recommendation: default on using `bwrap` where available. Tested under enforcing SELinux, see [SECURITY](SECURITY.md). | | Some build tools will need a permission adjustment |
| D6 | ~~Starting Ollama for testing.~~ **Resolved (2026-11-06):** the developer starts the `local-ai` pod. We use its API and don't manage it. | | |
| D7 | ~~Licence.~~ **Decided by the developer (2026-11-07): Apache-2.0.** `LICENSE` and `NOTICE` are in the repository root and every crate declares it. | | Permissive with an explicit patent grant |
| D8 | ~~Slim down `CLAUDE.md`.~~ **Done (2026-11-06):** the spec is unchanged in [MASTER_SPEC](MASTER_SPEC.md), and `CLAUDE.md` is a short pointer with the working rules. | | Spec content is read on demand rather than always present |
