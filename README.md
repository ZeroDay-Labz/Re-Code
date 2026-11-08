<p align="center">
  <img src="assets/icon/re-code-256.png" width="128" height="128" alt="re-code icon: a gradient diamond over a terminal prompt">
</p>

<h1 align="center">re-code</h1>

<p align="center"><strong>A coding agent for your own machine.</strong><br>
Terminal-native, written in Rust, local models first. It reads, searches, edits and writes your files with your approval, runs commands in a sandbox, and uses the MCP servers, skills and plugins you already have.</p>

<p align="center">
  <a href="https://github.com/ZeroDay-Labz/Re-Code/releases/latest"><img alt="Latest release" src="https://img.shields.io/github/v/release/ZeroDay-Labz/Re-Code?style=flat-square&color=ff4f8b"></a>
  <a href="LICENSE"><img alt="Apache-2.0" src="https://img.shields.io/badge/licence-Apache--2.0-8b5cf6?style=flat-square"></a>
  <img alt="Rust" src="https://img.shields.io/badge/Rust-edition%202024-ff8c3a?style=flat-square">
  <img alt="Linux" src="https://img.shields.io/badge/Linux-Fedora%20%C2%B7%20Debian%20%C2%B7%20Ubuntu-16151e?style=flat-square">
</p>

<p align="center">
  <img src="assets/screenshots/approval.png" alt="re-code asking for approval before editing a file, with the diff shown" width="900">
</p>

<p align="center"><em>Every screenshot in this README was captured from the real program, driving a local model.</em></p>

---

## Why re-code

Most local-model agents fail for reasons that have nothing to do with the model: prompts silently truncated, a thirty-second model load with no feedback, tool calls in a dialect the client can't parse, and tens of thousands of tokens of overhead before you've typed a word. re-code is built around those failures.

- **Every token is accounted for.** Each request has an itemised ledger (`/context`), and estimates are always labelled separately from what the provider reported.
- **It asks before it acts.** Edits, writes, deletes and commands wait for you, with a live diff. Say yes once, for the session, or for the project. Ctrl+Z takes a turn's file changes back.
- **It respects what you already run.** It finds your Ollama, llama.cpp, vLLM or LM Studio, your podman pods and systemd units, and never starts, stops or reconfigures them without asking.
- **It reaches what Claude Code reaches, on your models.** The MCP servers you set up in Claude Code are used live. Skills come from the Hermes index, GitHub and skills.sh. Plugins come from the Hermes catalog and Claude Code marketplaces. All of it is fetched without running anything, scanned, and shown to you first.
- **It tells you what's actually happening.** Loading a model, evaluating the prompt and generating are different states, and the interface shows which one you're in.

## Install

Packages for Fedora and Debian/Ubuntu are on the [releases page](https://github.com/ZeroDay-Labz/Re-Code/releases):

```sh
sudo dnf install ./re-code-*.x86_64.rpm     # Fedora
sudo apt install ./re-code_*_amd64.deb      # Debian, Ubuntu (needs glibc 2.39 or newer)
```

Or build from source with a current stable Rust toolchain:

```sh
cargo install --path crates/app
```

**You'll want:** a running [Ollama](https://ollama.com) (or another runtime it can find) with a tool-capable model such as `gpt-oss:20b` or `qwen3`, and `bubblewrap` for the command sandbox (`sudo dnf install bubblewrap`). Without bubblewrap, commands still ask, but the interface says plainly that they'd run unsandboxed.

**First run:**

```sh
cd your-project
re-code              # the home screen: pick a model, start typing
re-code doctor       # what re-code found on this machine, and why
re-code demo         # the interface with a recorded session, no model needed
```

## A look around

| | |
| --- | --- |
| **Home.** Saved chats to continue, the last model, free models to try, and live GPU and memory meters. ![Home](assets/screenshots/home.png) | **Models.** Every model your runtime serves with its real size, what it declares (tools, vision, thinking), and whether it fits your GPU, with the context memory estimated on top of the weights. ![Models](assets/screenshots/models.png) |
| **A turn.** Tool calls as rows, reasoning folded to one line, and a footer with the provider's own token figures. ![Conversation](assets/screenshots/conversation.png) | **Context.** `/context` breaks the next request down part by part and names the largest tool results. ![Context](assets/screenshots/context.png) |
| **Runtimes.** Found through podman, docker, systemd and the processes actually answering; Enter on a stopped one shows the exact start command. ![Runtimes](assets/screenshots/runtimes.png) | **MCP.** Your servers plus the ones set up in Claude Code, used live; the registry is one key away. ![MCP](assets/screenshots/mcp.png) |
| **Skills from hubs.** Fetched, scanned and shown before anything is installed. ![Skill install](assets/screenshots/skill_install.png) | **Plugins.** A marketplace plugin previewed: what runs here, what doesn't. ![Plugin install](assets/screenshots/plugin_install.png) |
| **Plugins tab.** Installed plugins, followed marketplaces, and what Claude Code itself has installed. ![Plugins](assets/screenshots/plugins.png) | **Skills tab.** Yours, the project's (off until trusted), Claude Code's, each with its scan verdict. ![Skills](assets/screenshots/skills.png) |
| **Command palette.** Ctrl+K for everything, recent commands first. ![Palette](assets/screenshots/palette.png) | **Themes.** Eleven palettes with dim and bright variants, previewed live; your own theme files hot-reload. ![Themes](assets/screenshots/themes.png) |

## What it does

### Models and runtimes

- **Local first.** Ollama is native. llama.cpp, vLLM and LM Studio are found by what they answer, and `re-code doctor` explains each one: which container or systemd unit serves it, whether it has GPU devices, and the exact command that would start it.
- **Honest model facts.** Sizes come from the runtime. Tool, vision and thinking support is shown as *declared* until a model is tested. Context limits come from the model's own metadata, and the memory its key/value cache needs is estimated per context size.
- **Cloud when you want it.** Anthropic, OpenAI, Google Gemini, OpenRouter and any OpenAI-compatible endpoint, with presets for a dozen hosts. Keys live in the system keyring, never in a file. OpenRouter's free models are listed with their real facts, and its daily allowance is shown after connecting.
- **Switch mid-session.** Ctrl+L changes model; the conversation carries on and the ledger keeps counting.

### The agent

- **Tools:** read (with paging), list, grep, write, edit, delete, shell, git, plus a `todo` list, a `task` helper, memory, skills and MCP.
- **Approvals with a diff.** `y` once, `a` for the session, `p` remembered for the project, `n` deny, `d` to see the whole diff. Permission modes cycle with Shift+Tab: ask, accept edits, plan (reading tools plus a `plan` file), and auto inside the sandbox.
- **The sandbox.** Commands run under bubblewrap: the project writable, everything else read-only, your home emptied except toolchains, secrets and `/run` hidden, no network unless the model asks and you approve, a scrubbed environment, its own PID namespace, a ten-minute limit, killed on Esc.
- **Files anywhere, scoped.** The agent can reach outside the project with approval; grants can be exact, project-wide for edits or deletes, or a whole folder (`--add-dir`). Files that look like secrets always ask.
- **Undo and rewind.** Ctrl+Z takes back a turn's file changes. Esc Esc (or `/rewind`) goes back to an earlier request: conversation only, conversation and files, or with a summary of what was removed.
- **Type while it works.** Enter queues your message; it reaches the model after the current tool calls. `!command` runs something yourself in the sandbox (`!!` with network, `&` in the background).

### Context you can see

- `/context` shows what fills the next request: system prompt, memory, skills index, tool definitions, conversation, tool results, with the three largest results named. `/context raw` shows the request itself, secrets hidden.
- Compaction is preflighted on every request. From a threshold, old tool results shrink to handles (a `read_result` tool brings them back), then older messages are summarised by the model with the objective and recent steps kept verbatim. `/compact <focus>` steers it; `/compactions` lists what each one kept and replaced; `/autocompact 100k|auto|off` sets where it starts.
- Each model has a context window re-code budgets against, tunable on the Settings tab. Estimates calibrate themselves per model from the tokens the provider reports, and a runtime that reports far fewer prompt tokens than were sent is called out as probably truncating.

### MCP

- Your own servers, plus the ones configured in Claude Code (user, project and approved `.mcp.json`), used live with their tool schemas.
- Discover-then-call by default so a 200-tool server costs a small index, or expose a server's tools directly with `x`. Per-server tool filters the model can't bypass.
- Add anything: a command line, an `http(s)://` address, a registry id, a `server.json` link, a GitHub repo, or a pasted config snippet from any other tool. Search the official registry with `s`. Bring over what you set up in OpenCode with `o` (keys are never copied).
- Stdio servers run in bubblewrap by default, with the project writable and a key reaching only that server; a server that needs the host can opt out in its form.
- `/web` gives the model web search through a server of your choice: your own SearXNG by address, DuckDuckGo and a page reader without a key, or Brave, Tavily and Exa with one. The form opens prefilled.

### Skills and plugins

- **Skills** follow the [Agent Skills](https://agentskills.io) format. A small index goes in the prompt; a skill's instructions load only when a task matches, or when you type `/skill-name`. Every skill is scanned for piped-to-shell downloads, decode-and-execute, destructive deletes, credential reads and prompt injection before it can be enabled. A project's skills stay off until you `/trust` the folder.
- **Install from hubs:** `/skill install official/security/godmode`, a bare name from the Hermes index, `owner/repo/folder`, a GitHub, skills.sh or `SKILL.md` link, or a folder. Nothing fetched is executed and no git runs: files come through GitHub's tree listing, into staging, scanned, then shown with the pinned commit, what the skill declares, its size and the verdict. `u` checks for an update and lists the files that would change.
- **Plugins** come from the Hermes plugin catalog, Claude Code marketplaces you follow, GitHub or a folder, with Hermes' own wording: `name [tier] pinned @ sha · maintained by …`. A plugin's skills and MCP servers work here. Its Python code, desktop parts, agents and LSP servers are listed as not run. Claude Code hooks are translated into a file beside the plugin for you to read and copy, never loaded on their own. Marketplace sources that execute code on install are refused with the reason.

### Memory and instructions

- `RE-CODE.md`, `AGENTS.md` and `CLAUDE.md` are read at user and project level with `@path` imports, so a project set up for another agent works as it is. `/init` writes a first one from what the tree proves; `/memory` lists what's in force and its cost; `# note` appends a line.
- re-code also keeps **memory of its own**: the model saves facts with a `memory` tool (after approval), and later sessions get them back. Project and user scopes, files in re-code's data folder, a Settings row for on, read-only or off, and `--no-memory` for scripts.

### Sessions

Every session is saved as it happens. `re-code --continue` or `/continue` picks one up, including after a crash, with the model told what it missed. Chats can be deleted from the Home list. `/export` writes the conversation as Markdown; Ctrl+X opens it as a searchable transcript, and `[` writes it into your terminal's own scrollback.

### Automation

- **The same management from the shell:** `re-code mcp add <command|address|link>`, `re-code skill install official/security/godmode --yes`, `re-code plugin marketplace add anthropics/claude-plugins-official`, `re-code memory list --json`, `re-code trust`. Installs print the same preview as the interface and ask, or take `--yes` / `--keep-off`.
- `re-code -p "prompt"` runs one request without the interface, with text, JSON or streaming JSON output, `--max-turns`, `--allowed-tools` and `--permission-mode`. Exit codes say what happened: 0 done, 1 failed, 2 a tool that needed approval was denied, 3 no such model.
- **Hooks** from `~/.config/re-code/hooks.toml` and a trusted project's `.re-code/hooks.toml`: `pre_tool_use` (block, change the arguments, add context), `post_tool_use`, `stop` and `session_start`, JSON on stdin, project hooks sandboxed.
- **Background commands** keep running while you work (`/tasks`), and a `task` tool lets the model delegate a self-contained job to a helper with its own empty context.

### The interface

- Fits any terminal size, checked by property tests across thousands of sizes, down to 30×12.
- Drag to select and copy anywhere on screen; double-click a word, triple-click a message; middle-click pastes. F2 hands the mouse to your terminal.
- Side panel with the session (model, tools, context left, files changed) or the project tree; click a file to open it in `$EDITOR`.
- Eleven themes, dim and bright variants, an accent override, and your own TOML theme files in `~/.config/re-code/themes/` that hot-reload. `theme = "nord:bright+#ff8800"` in `config.toml` works too.
- Toasts, desktop notifications when a turn needs you while the window is unfocused, a reduced-motion spinner, and a light theme detected from the terminal.

## Keys and commands

| Key | Does |
| --- | --- |
| `Enter` / `Alt+Enter` | Send / new line |
| `Esc` | Stop the agent, or close a window; twice to rewind |
| `Ctrl+K` | Command palette |
| `Ctrl+L` / `Ctrl+E` | Switch model / MCP, skills, plugins, tools |
| `Shift+Tab` | Permission mode: ask, accept edits, plan, auto |
| `Ctrl+B` | Side panel: session, files, hidden |
| `Ctrl+T` / `Ctrl+O` | Show reasoning / last tool output |
| `Ctrl+Z` / `Ctrl+Y` | Undo the last turn's file changes / copy the last code block |
| `Ctrl+R` / `Ctrl+X` | Search earlier prompts / transcript mode |
| `y` `a` `p` `n` `d` | On an approval: once, this session, this project, deny, diff |
| `/` and `@` | Commands, and mention a file |
| `F1` | Every key, in the program |

| Command | Does |
| --- | --- |
| `/context [raw]`, `/compact [focus]`, `/compactions`, `/autocompact <size>` | The context window and compaction |
| `/mcp`, `/skills`, `/plugins`, `/tools`, `/providers` | The home tabs |
| `/skill install\|search\|update`, `/plugin install\|browse\|marketplace add` | Hubs and marketplaces |
| `/web` | Web search for the model |
| `/trust`, `/permissions`, `/plan` | Project trust, grants and the plan file |
| `/memory`, `/init` | Instruction files and memory |
| `/continue`, `/new`, `/rewind`, `/export`, `/transcript`, `/diff` | Sessions and history |
| `/tasks`, `!command` | Background commands |
| `/theme`, `/free`, `/files`, `/help` | Looks, free models, the tree, keys |

## Where things live

| Path | Holds |
| --- | --- |
| `~/.config/re-code/config.toml` | Ollama address, default context size, theme |
| `~/.config/re-code/extensions.toml` | MCP servers, providers, custom tools, marketplaces, on/off choices (keys stay in the keyring) |
| `~/.config/re-code/skills/`, `themes/`, `hooks.toml` | Your skills, theme files and hooks |
| `~/.config/re-code/RE-CODE.md` | Your own instructions, read with every request |
| `~/.local/share/re-code/` | Sessions (`re-code.sqlite3`), plugins, memory, plans |
| `~/.cache/re-code/` | The models.dev catalog, hub indexes, install staging |
| `~/.local/state/re-code/logs/` | Logs |
| `.re-code/` in a project | Project skills, hooks and instructions, off until trusted |

## Security in one paragraph

Nothing the agent fetches or installs is executed by the install. Commands run in bubblewrap with the project writable and little else visible. Secrets never go into files re-code writes: keys live in the keyring, and `${VARIABLE}` references are read from the environment. A project's configuration is inert until you trust it, and trust is pinned to its content. Every skill is scanned before it can be enabled; a dangerous verdict can't be overridden. The details, the threat model and what isn't covered yet are in [SECURITY](docs/SECURITY.md).

## Status

re-code is young and says so. Verified live on local Ollama models: the agent loop, approvals, the sandbox, undo, sessions, compaction, MCP against real servers, skill and plugin installs against the real hubs. Wired and unit-tested but **not yet verified against a live provider:** tool calling on the cloud adapters. The `.deb` is built on Fedora and needs a recent glibc; a Debian-built package comes with CI. The roadmap and what each milestone proved are in [ROADMAP](docs/ROADMAP.md); everything shipped is in [CHANGELOG](CHANGELOG.md).

## Documentation

| Document | Covers |
| --- | --- |
| [PRODUCT_SPEC](docs/PRODUCT_SPEC.md) | Product understanding, defining features, non-goals |
| [ARCHITECTURE](docs/ARCHITECTURE.md) | Crates, dependencies, packaging, testing, decisions |
| [TUI_DESIGN](docs/TUI_DESIGN.md) | Interface model, layout, interaction, rendering |
| [PROVIDER_ARCHITECTURE](docs/PROVIDER_ARCHITECTURE.md) | Providers, runtimes, harnesses, capabilities |
| [HARDWARE_AND_RUNTIME](docs/HARDWARE_AND_RUNTIME.md) | Hardware detection, runtime management |
| [AGENT_ENGINE](docs/AGENT_ENGINE.md) | Execution loop, tools, permissions flow |
| [CONTEXT_ENGINE](docs/CONTEXT_ENGINE.md) | Token ledger, budgeting, compaction, memory |
| [IDE_INTEGRATION](docs/IDE_INTEGRATION.md) | JetBrains MCP, ACP, plugin, IPC |
| [SECURITY](docs/SECURITY.md) | Threat model, sandboxing, credentials, installs |
| [ROADMAP](docs/ROADMAP.md) | Milestones, what each one proved, risks |
| [MASTER_SPEC](docs/MASTER_SPEC.md) | The original full requirements |

## Development

Requires a stable Rust toolchain (developed on 1.99, edition 2024).

```sh
cargo run                                        # the program, needs a runtime
cargo run -- demo                                # the interface with a recording, no model needed
./scripts/check.sh                               # formatting, lints, tests, licences
cargo test -p recode-providers -- --ignored      # live tests against a real Ollama
cargo test -p recode-extensions --test skillhub_live -- --ignored   # live hub tests (spends GitHub requests)
scripts/screenshots/capture.sh [model]           # recapture every screenshot from the real program
scripts/package.sh                               # build the rpm and deb into dist/
```

Contributions are welcome; see [CONTRIBUTING](CONTRIBUTING.md). Licensed under [Apache-2.0](LICENSE).
