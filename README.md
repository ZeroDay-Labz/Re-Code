# re-code

A terminal-native AI coding agent for Linux, written in Rust. It is built to work properly with
**local models** first and to treat cloud models and external agents as equals later.

> **Status: Milestone 1 in progress.** `re-code` is a Claude Code-style agent for your own models. In a
> project folder, a tool-capable model reads, searches, edits and writes files — **anywhere on your
> computer, with your approval** — runs commands in a bubblewrap sandbox, and uses **your MCP servers,
> including the ones set up in Claude Code**, plus custom tools and skills. Type `/` for commands and `@` to
> mention a file; Ctrl+Z undoes a turn's file changes; sessions are saved (`re-code --continue`, or `/continue`
> to pick one) and long conversations are compacted. This is verified on local Ollama models; the cloud
> adapters (OpenAI-compatible and Anthropic) have tool-calling wired and unit-tested but not yet verified
> against a live cloud provider. Much of these documents still describes the plan.
>
> `re-code` is a working name. See decision D1 in [ARCHITECTURE](docs/ARCHITECTURE.md#decisions-needed).

## What makes it different

Most local-model agents fail for reasons that have nothing to do with the model: prompts silently
truncated, 30-second model loads with no feedback, tool calls in a dialect the client can't parse,
and tens of thousands of tokens of overhead before the user has typed anything. re-code is designed
around those failures:

- **Every token is accounted for.** Each request has an itemised ledger, and estimated counts are
  always labelled separately from provider-reported ones.
- **Tool calling is measured, not assumed.** Models are probed with a conformance suite and the
  results decide how the agent talks to them.
- **It tells you what is actually happening.** "Loading model", "evaluating prompt" and "generating"
  are different states, and the interface shows which one you are in.
- **It respects what you already run.** It finds your existing runtimes and won't start, stop or
  change them without asking.

## Documentation

| Document | Covers |
| --- | --- |
| [PRODUCT_SPEC](docs/PRODUCT_SPEC.md) | Product understanding, defining features, non-goals, missing requirements |
| [ARCHITECTURE](docs/ARCHITECTURE.md) | Crates, dependencies, packaging, testing, **decisions needed** |
| [TUI_DESIGN](docs/TUI_DESIGN.md) | Interface model, layout, interaction, rendering |
| [PROVIDER_ARCHITECTURE](docs/PROVIDER_ARCHITECTURE.md) | Providers, runtimes, harnesses, capabilities |
| [HARDWARE_AND_RUNTIME](docs/HARDWARE_AND_RUNTIME.md) | Hardware detection, recommendations, runtime management |
| [AGENT_ENGINE](docs/AGENT_ENGINE.md) | Execution loop, tools, malformed calls, permissions flow |
| [CONTEXT_ENGINE](docs/CONTEXT_ENGINE.md) | Token ledger, budgeting, compaction, memory |
| [IDE_INTEGRATION](docs/IDE_INTEGRATION.md) | JetBrains MCP, ACP, plugin, IPC |
| [SECURITY](docs/SECURITY.md) | Threat model, sandboxing, credentials |
| [ROADMAP](docs/ROADMAP.md) | Milestones, risks, open questions |
| [MASTER_SPEC](docs/MASTER_SPEC.md) | The original full requirements |
| [Ollama spike](docs/spikes/2026-11-06-ollama.md) | Measured API behaviour and model test results |

**Providers:** local Ollama, plus cloud providers (Anthropic, OpenAI, Google Gemini, OpenRouter and any OpenAI-compatible server) added on the Providers tab with keys kept in the OS keyring. Tool use (files, shell, MCP, custom tools, skills) runs today on tool-capable local Ollama models; the same path is wired for cloud models but not yet live-verified.

## Development

Requires a stable Rust toolchain (developed on 1.99, edition 2024).

```bash
cargo run                                       # welcome screen and chat, needs Ollama
cargo run -- demo                               # the interface with a recording, no model needed
cargo run -- doctor                             # check this machine
./scripts/check.sh                               # formatting, lints, tests, licences
cargo test -p recode-providers -- --ignored      # live tests, needs Ollama with llama3.2:3b
```
