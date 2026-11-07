# Changelog

All notable changes to re-code are recorded here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and versions follow [Semantic Versioning](https://semver.org/). Nothing has been released yet.

## [Unreleased]

### Added

- Runtime discovery: `re-code doctor` finds Ollama, llama.cpp, vLLM and LM Studio by what each answers,
  lists podman and docker containers and pods, says which container or systemd unit serves a runtime and
  whether it has GPU devices, and shows the exact command that would start a stopped one. Read-only.
- A Runtimes tab on the welcome screen with the same view; Enter on a stopped runtime shows the exact
  start command and runs it only when confirmed, then waits for the API to answer.
- Skills in the Agent Skills format with progressive loading: a `skill` tool loads a skill's instructions
  (or one of its files) when a task matches; `/skill-name` invokes one directly. Each skill is scanned for
  dangerous patterns before it can be enabled, and a project's skills load only after the folder is trusted
  (`/trust`, or switching one on, shows what would load and asks).
- MCP: search the official registry from the MCP tab and add a result with one key; paste any config
  snippet, command line, registry id or GitHub link into the add form; PyPI/OCI packages and
  streamable-HTTP remotes in `server.json`; a per-server tool filter the model can't bypass.
- Apache-2.0 licence, `NOTICE`, `CONTRIBUTING.md`.
- OpenRouter: the key's own standing (free-model requests used today, whether credits were ever bought,
  credits left) is shown after connecting and added to a rate-limit error; a refusal caused by the account's
  privacy settings names the settings page; requests carry an `X-Title: re-code` header.
- Provider presets for Ollama Cloud, NVIDIA Build, Hugging Face, Mistral, DeepSeek, Together, Fireworks,
  Moonshot, Z.AI, MiniMax, xAI and Novita (OpenAI-compatible endpoints; chat not yet live-tested).

- Context windows per model with an automatic default: size, compaction trigger and reserve for the reply are
  tunable on the Settings tab and with `/autocompact`; cloud models are compacted too; `/compact <focus>`;
  `/context` and `/context raw`; `/compactions` with the replaced messages kept; a context-left indicator;
  re-injection of the task list, recent files and loaded skills after a summary; a thrash guard; estimates
  calibrated from reported usage; a warning when a local runtime seems to truncate the prompt; opt-in idle
  compaction; the key/value cache counted in the memory estimate; `<thought>` tags shown as reasoning.

- Instruction files: `RE-CODE.md`, `AGENTS.md`, `CLAUDE.md` at user and project level with `@path` imports,
  trust-gated for projects, with size warnings; `/init` to write a first one, `/memory` to list and edit them,
  `# note` to append a line; a "Compact instructions" section steers summaries.

- Permission modes (Shift+Tab): ask, accept edits, plan (reading tools and a `plan` tool, `/plan`), auto in the
  sandbox; `p` on an approval remembers the grant for the project; `/permissions` lists and forgets grants.

- Type-ahead while the model works (queued, delivered after tool calls or as the next turn; Ctrl+Enter sends
  now), `!command` in the sandbox, Ctrl+R history search across projects, Esc Esc / `/rewind` with file
  restore or a summary, `/export` to Markdown.

- Selection anywhere on screen with copy to the clipboard and the primary selection, F2 on every screen,
  `RE_CODE_DISABLE_MOUSE`, a native-selection hint, and transcript mode (Ctrl+X) with search, a scrollback
  dump and an editor hand-off.

- Headless `re-code -p` with text, JSON and streaming JSON output; hooks (`pre_tool_use`, `post_tool_use`,
  `stop`, `session_start`) from the user's and the trusted project's `hooks.toml`.

- Background shell commands: the model's `shell` tool takes `background: true` (and `!command &` does the
  same), `task_output` and `task_kill` tools, `/tasks` to list, read and stop them; everything is killed
  when re-code exits. A `task` tool delegates a self-contained job to a helper conversation on the same
  model with its own, empty context and reading tools by default; only its answer comes back, approvals
  go through the usual card, and tasks don't nest.

### Changed

- OpenCode Zen uses the address its own listing gives (`opencode.ai/inference/openai/v1`), and its $0
  models are hidden with the reason: OpenCode serves them only to its own app. `/free` offers OpenRouter.

[Unreleased]: https://example.invalid/re-code/compare/HEAD
