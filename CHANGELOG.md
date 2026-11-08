# Changelog

All notable changes to re-code are recorded here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and versions follow [Semantic Versioning](https://semver.org/). Nothing has been released yet.

## [Unreleased]

## [0.1.7] - 2026-11-08

### Added

- A dangerous scan verdict can be accepted, knowingly. The scan can't tell a CVE write-up that quotes
  `rm -rf /` from instructions to run it; the person reading can. `o` on a dangerous preview (or
  `space` on a dangerous skill in the Skills tab) lists every finding, says what enabling means, and
  asks; `y` installs or switches it on by your word. The acceptance is recorded against a fingerprint
  of exactly those findings, so an update that changes them switches the skill off again until you look
  anew. From the shell: `re-code skill install --yes --accept-dangerous <what>` and
  `re-code skill enable <name> --accept-dangerous`; `--yes` alone still keeps a dangerous skill off. A
  plugin's dangerous skill is accepted afterwards, skill by skill, never with the plugin. An accepted
  skill shows `▲` and "dangerous, accepted" in the list.

## [0.1.6] - 2026-11-08

### Added

- The skill search window is now a browser of the whole Hermes index: it lists at once, narrows live as
  you type, and `←`/`→` step through its sources (Hermes' own skills first, then ClawHub, skills.sh,
  GitHub, LobeHub) with each one's count. Hermes' own skills show their category, and words match
  categories and tags as well as names and descriptions. The header says how many matched past the
  page shown. `/skill browse [source]` opens within one source; `re-code skill browse [--source X]
  [words]` does the same from the shell.

### Fixed

- Piping the command line's output into `head` wrote a crash file ("failed printing to stdout: Broken
  pipe"); a closed pipe now ends the process quietly with exit code 141, as a pipe death does for other
  tools.

## [0.1.5] - 2026-11-08

### Added

- Tick boxes on every install list. A repository or Hermes category with several skills lists them with
  `space` to tick and `a` to tick all; Enter fetches the ticked set into a checklist that shows each
  one's verdict as it lands, and `y` installs the ticked ones (a dangerous one stays off), `k` installs
  them off, `n` discards them all. Skill search hits and plugin listings tick with `tab` and `^a`.
- The plugin browser groups the Hermes catalog by its categories (tools, desktop, models, memory,
  general, automation, web, platform, voice): `←`/`→` move along them, and `^a` ticks everything shown.
  A checklist takes 25 at a time, since each one is a GitHub listing and GitHub allows 60 an hour
  without a `GITHUB_TOKEN`.
- Paste into the install prompt: several links or names (one per line) become a checklist; a pasted
  `SKILL.md` is staged from the text, scanned and previewed, and recorded as pasted so an update knows
  there is nowhere to fetch from.
- `re-code skill install` and `re-code plugin install` take several things at once and preview each in
  turn; the exit code is the worst of them.

### Fixed

- A repository that is itself a skill but holds more than the file limit (a monorepo with evals and
  tooling around a root `SKILL.md`, such as conorbronsdon/avoid-ai-writing with 676 files) failed with
  the limit; it now offers its skill folders to choose from, and says why the whole wasn't offered. A
  small repository that is a skill and also holds skill folders lists the whole repository as the first
  choice.

## [0.1.4] - 2026-11-08

### Added

- `/untrust` (also in the palette) switches a trusted folder's configuration off again after a confirmation
  that lists what goes off; `re-code untrust` does the same from the shell.
- A runtime can be started whenever re-code opens: `a` on the start confirmation remembers the exact
  command; it runs at launch only while the command is still that one (a changed setup asks again), and the
  Settings tab lists it under "Runtimes started when re-code opens" with `d` to remove.
- The pager (`/diff`, `/context raw`, a tool's output) searches with `/`, `n` and `N`, like the transcript.
- The Help overlay (F1) scrolls on short terminals.

### Fixed

- A stdio MCP server from a plugin installed during the session, or tested from the command line, couldn't
  see its own files inside the sandbox: the plugin folders are now read when the server starts, not once at
  launch.

### Changed

- The sandbox shows `~/.local/opt` (where user-installed toolchains such as Node often live) to commands
  and stdio MCP servers, read-only, like the other toolchain folders.

## [0.1.3] - 2026-11-08

### Added

- A crash writes a file next to the logs (`~/.local/state/re-code/crash-<ms>.txt`) with the message, the
  place in the code, the version and the system, and nothing from the conversation; its path is named
  once the terminal is restored. `re-code doctor --report` writes a bug-report bundle: the doctor report,
  the version, recent log lines with secrets hidden, and any crash files.
- `--version` names the commit and the build date. Logs rotate daily and keep a week.
- GitHub Actions: the gate (fmt, clippy, tests, licences) on every push, the rpm built on Fedora 44 and the
  deb built on Ubuntu 22.04 (glibc 2.35, so it installs on Debian 12 and Ubuntu 22.04 or newer); a `v*` tag
  builds both and publishes the release with checksums and the changelog section as notes.
- A live cloud test (`crates/providers/tests/live_cloud.rs`, ignored; key from the environment) that chats,
  calls a tool and sends the result back on any OpenAI-compatible host or on Anthropic.

### Fixed

- Tool calls carry provider data back: Gemini's OpenAI-compatible endpoint attaches a `thought_signature`
  to each tool call and refuses the next request without it, which broke every tool round trip on Gemini.
  The adapter now keeps the signature with the call and returns it. **Verified live on Gemini 3.8 Flash**
  (chat, tool call, round trip); the first live confirmation of tool calling on a cloud provider.

## [0.1.2] - 2026-11-08

### Added

- OpenCode import: `o` on the MCP and Providers tabs, `/import opencode`, or `re-code mcp import opencode`
  reads `~/.config/opencode/opencode.json` (and a trusted project's) and brings over its MCP servers and
  providers. Keys are never copied: `{env:VAR}` becomes a `${VAR}` reference, a literal key is reported;
  OAuth remotes, agents, modes, JavaScript plugins and `instructions` are listed as not run here.
- The command line does what the home screen does: `re-code mcp list|add|remove|enable|disable|test|import`,
  `re-code skill list|search|install|update|remove|enable|disable`, `re-code plugin list|search|install|
  update|remove|enable|disable|marketplace add|list|remove|browse`, `re-code memory list|show|forget`,
  `re-code trust [--yes]` and `re-code untrust`. Lists take `--json`; installs print the same preview as
  the interface and take `--yes` or `--keep-off` (exit 4 when a decision is needed and there's no terminal,
  2 when refused).
- Stdio MCP servers run in bubblewrap by default: the project writable, the home folder emptied except
  toolchains and package caches, secrets hidden, network on, the server's environment passed in with
  `--setenv` so a key reaches only that server. Each server has a Sandbox setting (bubblewrap or none) in
  its form; the row says `unsandboxed` when it's off, and the test prompt says where the program runs.
  Installed plugins' folders are bound writable for their servers.

### Changed

- "Free" on the Models tab now means the provider charges nothing at all for the model on your account:
  a model that is $0 per token but charges per request (OpenRouter's music and image models) is no longer
  called free, Hugging Face's "free" models (served from monthly included credits that run out) aren't
  either, and models that don't answer in text aren't listed for chat. Paid models are grouped per
  provider ("Paid · your gemini account"), so what one key pays for isn't mixed with another.

## [0.1.1] - 2026-11-08

Adds `/web` to 0.1.0, which was tagged earlier the same day as the first packaged release (rpm and deb).

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
- Skill hubs: `/skill install <what>` (or `i` on the Skills tab) fetches a skill from the Hermes index
  (`official/security/godmode`, a bare name), GitHub (`owner/repo/folder[@ref]`, links), skills.sh, a
  `SKILL.md` link or a local folder into a staging folder, scans it and shows a Hermes-style preview
  (label, pinned commit, what it declares, files, tokens, scan) before `y` enable / `k` keep off / `n`
  discard / `r` read. Nothing fetched is executed, no git is run. `/skill update <name>` (or `u`) shows the
  files that would change, `/skill search <words>` searches the Hermes index. Each installed skill carries a
  `.re-code-source.toml` with where it came from and which commit.
- `/web` (also on the Home tab) sets up web search for the model as an MCP server: your own SearXNG by
  address, DuckDuckGo and a page reader without any key, or Brave, Tavily and Exa with a key. The pick
  opens the MCP form prefilled; the address or key stays in your own configuration.
- Saved chats can be deleted: `d` on a chat in the Home list or in the `/continue` picker asks, then removes
  its messages, snapshot and compaction history. Deleting the open conversation starts a new one.
- Plugins: a Plugins tab and `/plugin install <what>` install from the Hermes plugin catalog (a name such as
  `better-session-appearance`), a Claude Code marketplace you follow (`/plugin marketplace add
  anthropics/claude-plugins-official`, then `name@marketplace` or `b` to browse), GitHub, or a folder,
  including what Claude Code itself has installed (listed read-only; Enter brings one over). The preview
  uses Hermes' wording (`name [tier] pinned @ sha · maintained by …`) and says what runs here: a plugin's
  skills and command files become skills and its `mcp.json` servers join the MCP tab; Python code, desktop
  parts, agents and LSP servers are listed as not run; Claude Code hooks are translated into
  `hooks.converted.toml` beside the plugin for you to copy, never loaded by themselves. `npm` and `command`
  marketplace sources are refused because they run code on install.

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

- Memory that re-code keeps itself: the model saves facts with a `memory` tool (after approval, or always
  once `a`/`p` is pressed), and later sessions get them back in the prompt. Project and user scopes, files
  in re-code's data folder, `/memory` to list, edit and forget, a Settings row (on, read-only, off) and
  `--no-memory`. Headless JSON lists `memories_changed`.
- `read` reads a part of a file with `offset` and `limit`, and `read_result` pages through long results.

### Changed

- No single tool result may take more than a quarter of the context window: longer ones are cut, with the
  whole text a `read_result` away. When a request still won't fit, the newest results are shortened too.
  This fixes conversations on small windows that refused every message after a few large reads.
- A `task` row shows the helper's outcome only (its steps already show as rows), and a helper's compaction
  is no longer reported as the main conversation's.

- OpenCode Zen uses the address its own listing gives (`opencode.ai/inference/openai/v1`), and its $0
  models are hidden with the reason: OpenCode serves them only to its own app. `/free` offers OpenRouter.

[Unreleased]: https://example.invalid/re-code/compare/HEAD
