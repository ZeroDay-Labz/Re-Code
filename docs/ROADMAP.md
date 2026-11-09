# Roadmap

Status: **proposed, awaiting agreement.** Each milestone ends with something you can use, and with a demo that
either works or doesn't. No dates: estimates would be invented.

## What changed from the spec's roadmap, and why

| Change | Why |
| --- | --- |
| Chat (M1) and agent engine (M2) merged | Chat doesn't test the main risk. The TUI's hardest states (approvals, tool timelines, interrupts) only exist with tools. |
| Context ledger and preflight moved to the first milestone | Without them, the first long session silently truncates, and we would be debugging ghosts. |
| Checkpoints and undo added early | An agent that edits files needs a way back. |
| Basic RPM moved early | You should be able to install and dogfood it, and it surfaces packaging problems while cheap. |
| IDE work leads with MCP and ACP | They might remove the need for a plugin. |

## M0: Foundations (done, 2026-11-06, except where noted)

- Decisions resolved, apart from the final name and licence (D1, D7), neither of which blocks M1.
- Cargo workspace, quality gates (`scripts/check.sh`), logging, config, and SQLite with migrations.
- `Command` and `Event` types defined.
- Ollama adapter with fixtures recorded from a real server, and live tests.
- **Not done:** hosted CI, as this repo has no remote yet.

**Spikes that could change the design,** run against the real system:

| Spike | Question |
| --- | --- |
| Ollama behaviour | **Done:** see the [spike report](spikes/2026-11-06-ollama.md). Recording fixtures remains. |
| Model reality check | **First pass done:** 5 of 8 models passed a simple tool loop. Multi-step coding tasks remain. |
| TUI feel | **Moved to the start of M1:** streaming markdown, scrolling, resizing and selection in Konsole and JediTerm, with recorded events |
| `bwrap` under SELinux | **Done:** works, about 6 ms per command. See [SECURITY](SECURITY.md). |

**Demo:** `re-code doctor` reports the true state of the machine and Ollama. Running and active-but-not-answering
were seen on the real machine. The installed-but-stopped path has unit tests only, since testing it for real would
mean stopping your pod.

## Agent engine (M1 core) — landed 2026-11-06

The model now uses tools. A new `tools` crate provides read/list/grep/write/edit/delete/shell/git, each
confined to the workspace with `openat2`/`RESOLVE_BENEATH` (symlink-escape tested). The driver runs an
agent loop for local Ollama models that declare tools: it sends tool schemas, parses tool calls, runs
read-only tools automatically, and shows an approval card (with a live diff) before any edit, write,
delete or shell command — `y`/`a`/`n`, with per-session "always". Verified live: `gpt-oss:20b` read a
file and answered, then edited it through an approved diff that was actually written to disk.

**Also landed:** image input for vision models (clipboard paste + `@path`, tested live on `qwen3.8:27b`);
MCP presets and add-from-link (a `server.json` URL, GitHub repo, or local path — REA resolves to
`npx -y rea-agents@latest mcp`); free cloud providers via the models.dev catalog (OpenRouter, OpenCode
Zen and more as one-click presets, free/vision/tools flagged); copy a code block with Ctrl+Y.

**Landed 2026-11-06 (D2, D3):** `shell` and custom tools run in a bubblewrap sandbox (workspace
writable, everything else read-only, home emptied except toolchains, secrets and `/run` hidden, network
off unless the model asks and the user approves, scrubbed environment, own PID namespace, 10-minute
timeout, killed on Esc); per-turn checkpoints with undo (Ctrl+Z); MCP servers, skills and custom tools as
agent tools (discover-then-call, or direct per server); tool-calling in the OpenAI-compatible and
Anthropic adapters. Live-tested on `gpt-oss:20b`: a sandboxed command, an undone write, and an MCP call.
Cloud tool-calling is unit-tested only — not yet against a live provider.

**Landed 2026-11-06 (D4):** sessions are saved as they happen (the event log, plus a snapshot of the exact
conversation after each turn) and `re-code --continue`, or Ctrl+K → "Continue the last session here", picks
the last one up; a turn cut off by a crash is shown as interrupted, not re-run, and the model is told.
Every request is preflighted: from 80% of the input budget, old tool results are shortened to handles
(`read_result` brings one back), then older messages are summarised by the model if needed, keeping the
objective, current request and recent steps verbatim. Each compaction is reported with sizes labelled as
estimates. Agent mode previously had no preflight at all and could silently overflow Ollama's context.
Verified live on `gpt-oss:20b` (a real summary, and edit → undo → restart → `--continue` in the real
interface, with the model aware of the undo).

**Landed 2026-11-06 ("Claude Code, but local"):** MCP servers configured in Claude Code are used live
(user and project scope, approved `.mcp.json`), with tool argument schemas, progressive discovery
(`list_mcp_tools` with `query`/`tool`), reconnection and plain-language failures; verified against the user's
running RustRover. Files anywhere on the computer with scoped approvals (see SECURITY). Interface: `/`
commands and `@` file mentions with a completion popup (mentioned files are attached), prompt history,
Ctrl+V pasting text too, click-to-expand tool output, one-line reasoning summaries, a session picker,
ranked/scrolling palette, scroll badge, link addresses, the output graph only while working, and a light
theme (`theme = "auto" | "dark" | "light"`, detected with OSC 11).

**Also landed:** drag-to-select-and-copy in the conversation (wl-copy/xclip and OSC 52), middle-click
paste, Ctrl+End/Ctrl+click to jump to the latest, and 11 themes with a live-preview picker (`/theme`).

**Landed 2026-11-06 (themes and free models):** a real theme engine (tokens resolved per frame, no
post-pass remap), dim/normal/bright variants, accent override, square or rounded borders, compact density,
code colours derived from the theme, and user theme files in `~/.config/re-code/themes/*.toml` that
hot-reload; `theme = "nord:bright+#ff8800"` in `config.toml`. Free models: Zen and OpenRouter listings are
read live with each model's own tool/vision/price facts (no stale catalog, no `:free` guessing); the Models
tab groups Local / Free / Paid / Needs a key with Ctrl+F/T/V filters; `/free` opens the sign-up page and
takes the key; rate limits and missing keys are explained in plain words. Chat on the free providers still
needs a real key to be live-tested (`crates/providers/tests/live_free.rs`).

**Landed 2026-11-07 (automation, part 1):** `re-code -p "prompt"` runs one request without the interface
(`--output-format text|json|stream-json`, `--model`, `--max-turns`, `--allowed-tools`, `--permission-mode`,
`-` for a prompt on stdin); a tool that would need approval is denied and named, and the run ends with exit
code 2; 3 means no such model (`crates/app/src/headless.rs`). Hooks from `~/.config/re-code/hooks.toml` and
the trusted project's `.re-code/hooks.toml`: `pre_tool_use` (block, change the arguments, add context),
`post_tool_use` (add context), `stop` (exit 2 sends the words back as the next request, twice at most) and
`session_start`, with JSON on stdin and project hooks sandboxed (`crates/app/src/hooks.rs`).

**Landed 2026-11-07 (automation, part 2):** background shell commands (`shell` with `background: true`,
`!command &`), in the same sandbox and their own process group, with `task_output`/`task_kill` for the model
and `/tasks` for the user; running ones are reminded to the model after a compaction and killed when re-code
exits (`crates/app/src/background.rs`). The `task` tool (`crates/app/src/subagent.rs`) runs a helper
conversation on the same model with its own empty context, the user's instruction files, reading tools by
default (or tools the parent names, approved as usual), twelve steps at most, one level deep and one at a
time; the parent gets the helper's answer (cut at 8k characters) and a list of what it did, nothing else. It
is offered only when the window is at least 16k, since a helper costs a second prompt.

**Live-checked 2026-11-07 on gpt-oss:20b (tmux, isolated config):** `re-code -p … --output-format json`
(exit 0, the `list` call and Ollama's figures in the JSON); a user `pre_tool_use` hook blocking a read
(`hook_ran … blocked`, the model explained the refusal); the context meter and `/context` breakdown after a
turn; Shift+Tab through all four modes; `!cmd` as a tool row; `/memory`; `!cmd &` and `/tasks`; a `task`
call whose helper ran three listings and a read (the hook applied to it too) while the parent's context
stayed under 2k; `/compact <focus>`, `/compactions`, Esc Esc rewind, `/permissions`; transcript mode with
the `[` scrollback dump and return. Found and fixed on the way: a helper's approval card had no tool row to
sit on (the helper's tool events now reach the user), a call blocked by a hook or refused by the mode drew
two rows, and a manual compaction with nothing to shorten claimed it had compacted and measured before and
after against different tool lists. Not checked live: mouse selection and middle-click paste (unit-tested;
tmux can't drive the mouse), idle compaction, and a compaction that really summarises on this model.

**Landed 2026-11-07 (memory, milestone 4 track M0):** re-code keeps memory between sessions
(`crates/app/src/memory.rs`, `docs/CONTEXT_ENGINE.md` "Memory"). Live on gpt-oss:20b: asked to remember
that packages come from dnf, it saved a user memory; a new session answered `sudo dnf install ripgrep`, and
with `--no-memory` it answered `cargo install ripgrep`. In the TUI a save showed its approval card, `p`
allowed it for the project, and a new session answered `cargo nextest run`. Also from a user's report on a
32k window: one tool result can no longer fill the window, `read` pages with `offset`/`limit`, and a
conversation over budget recovers instead of refusing every message ("One result never fills the window").

**Landed 2026-11-07 (select anything):** a drag selects text anywhere on screen, not only in the
conversation: the side panel, the prompt, the home tabs, overlays; it is clipped to the region it started
in and copied from the frame as it was seen. Copies go to the clipboard and to the primary selection
(`wl-copy`, `xclip`/`xsel`, tmux's buffer, and OSC 52 for both), and the toast names the path. F2 hands the
mouse to the terminal from any screen and the editor hand-off respects it; `RE_CODE_DISABLE_MOUSE=1` starts
that way; the status line names the terminal's own key for a native selection while dragging. Ctrl+X (or
`/transcript`) opens the whole conversation as a pager with `/` search and `n`/`N`, `[` writes it into the
terminal's own scrollback so native search, selection and tmux copy mode work on it, and `v` opens it in the
editor (`crates/tui/src/{terminal_hint,export}.rs`, `view::frame_cells`).

**Landed 2026-11-07 (input power):** Enter while the model works queues the message; it reaches the model
after the current batch of tool calls (shown as the user's message at that point) or becomes the next turn,
with a strip above the prompt, Backspace to take it back and Ctrl+Enter to stop the turn and send now (on
terminals with the kitty keyboard protocol). `!command` runs in the sandbox without the model and shows like
a tool call (`!!` with network, `\!` for a message); the model hears the tail of the output next time.
Ctrl+R searches earlier prompts, Tab widens it to every project. Esc Esc (or `/rewind`) goes back to before
a request: conversation only, conversation and files (through the turn checkpoints, this process only), or
with a summary of what was removed kept in place. `/export [path|clipboard]` writes the conversation as
Markdown.

**Landed 2026-11-07 (permission modes):** Shift+Tab cycles ask → accept edits → plan → auto (sandbox),
shown in the status line and as the prompt glyph (`❯ » ⏸ ⚡`). Accept edits runs edits and new files inside
the project without asking; deletes, commands and anything outside still ask. Plan mode offers reading tools
only and a `plan` tool that writes to a file outside the project (`/plan` shows it); a modifying call is
refused with an explanation the model sees. Auto runs anything the sandbox contains; files that look like
secrets and commands outside the sandbox still ask, and the mode is refused without bubblewrap. On an
approval `p` remembers the grant for this project across sessions (the `permissions` table, keyed by the
canonical project path); `/permissions` lists the mode, the session's grants and the project's, and `d`
forgets one (`crates/app/src/permissions.rs`).

**Landed 2026-11-07 (instruction files):** `RE-CODE.md` is read with every request, and so are `AGENTS.md`
and `CLAUDE.md`, so a project set up for another agent works as it is: the user's own file in re-code's
config folder (plus `~/.claude/CLAUDE.md`), then the project root, `.re-code/instructions.md`, and
subdirectory files that load once the model touches something below them (`crates/app/src/instructions.rs`).
`@path` lines import files, confined to the project (or the home folder for the user's files), five deep,
without cycles. Project files load only after `/trust`, whose confirmation now lists them; the user is told
once per session what is held back, and when a file costs more than ~4k tokens or is too long to send.
A "Compact instructions" section steers summaries. `/init` writes a first `RE-CODE.md` from what the tree
proves (crates, scripts, package scripts, make targets) after a preview; `/memory` lists the files in force
with their cost and opens one in the editor; a message starting `# ` is a note appended to a chosen file
(`\#` sends it as a message). Files re-code writes at the user's request keep the folder's trust.

**Landed 2026-11-07 (context you can tune):** every model has a context window re-code budgets against
(`recode_core::context`): local models get `num_ctx` capped at the model's own limit, cloud models their
listed limit, and the reserve for the reply and the compaction trigger follow from it unless the user pins
them. Cloud models are compacted now, like local ones. The Settings tab shows the active model's window
("auto (16k of 128k) · compacts at ~13k · reserve 2k", with the KV cache's memory where the runtime says);
`/autocompact 100k|auto|off` sets it from the keyboard; `/compact <focus>` keeps a focus for every later
summary; `/context` breaks the last request down by part (system prompt, skills index, tool definitions,
conversation, tool results, shortened results, summary, images) with the three largest tool results named,
and `/context raw` shows the request itself with secrets hidden; the status line says how much room is left
and turns bold under 15 %; `/compactions` lists what each compaction did, and Enter shows the messages the
summary replaced, which are now kept in the database. After a summary the model gets its task list, the
files it was working on (re-read from disk, inside the project only) and the skills it had loaded back as
notes; three summaries in one turn stop the turn with an explanation instead of thrashing. Estimates
calibrate themselves per model from the tokens the provider reports, and a local runtime that reports far
fewer prompt tokens than were sent is called out as probably truncating. Idle compaction is opt-in on the
Settings tab. The memory estimate for a local model now includes its key/value cache for the context it
would get (from the GGUF layer and head counts, or measured from a loaded model). Reasoning that a server
leaves inside the text as `<thought>`/`<think>` tags is shown as reasoning. Live-verified against Ollama
figures only in unit tests so far; the cloud compaction path is tested against a mock.

**Landed 2026-11-07 (providers):** OpenRouter shows its own figures after connecting ("Free models: 3 of 50
requests used today; no credits bought yet, so the daily allowance is the small one"), appends them when it
rate-limits a free model, and a "data policy" refusal (the account's privacy settings exclude a free model's
hosts) is explained with the settings page to fix it. OpenCode Zen is listed honestly: its listing's own
`api.chat-completions.base_url` is used, and its $0 models are hidden with the reason, because OpenCode's
documentation says free-tier models are served only to its own app (so Zen is no longer offered by `/free`).
New OpenAI-compatible presets whose endpoints answered as such on 2026-11-07, chat not yet live-tested:
Ollama Cloud, NVIDIA Build, Hugging Face router, Mistral, DeepSeek, Together, Fireworks, Moonshot, Z.AI,
MiniMax, xAI, Novita. The presets overlay windows around the selection so the longer list fits any height.
Still not live-tested: a chat on OpenRouter's free models (needs a key; `crates/providers/tests/live_free.rs`).

**Landed 2026-11-06 (UX):** toasts; flicker-free streaming (synchronized updates); settings kept in the
session database (`ui:*`), with a Settings tab and a Home tab (sessions to continue, free models to start
with, live GPU meters while on the welcome screen); a `todo` tool with a Tasks list in the side panel; per-tool
durations and a turn footer with provider-reported tokens; bell and desktop notifications when a turn needs
approval or takes long while the window is unfocused; line numbers in code, syntax-highlighted and
side-by-side diffs, `/diff` for the session's changes, a code-block chooser for Ctrl+Y; a Files pane in the
side panel (Ctrl+B cycles session → files → hidden; click folders to fold, files to open in `$EDITOR`), project
paths in the conversation underlined and clickable, the panel's edge draggable (width remembered), and the
palette listing recently used commands first. Checked in the real interface against the project's own tree.

**Landed 2026-11-07 (runtimes, the start of M3):** a `runtime` crate finds every inference runtime on the
machine the way the spec asks: addresses are identified by what they answer (Ollama by its root greeting,
llama.cpp by `/props`, vLLM by `/version`, LM Studio by `/api/v0/models`, anything else by `/v1/models`),
podman and docker containers and pods are listed and matched to runtimes by published port with known
images recognised by repository path, Quadlet and systemd units and native processes fill in what serves
each one, and a container's GPU devices are read from how it was created. All read-only and time-boxed.
`re-code doctor` shows it; the welcome screen has a **Runtimes** tab, and Enter on a stopped runtime shows
the exact start command and runs it only on `y`, then waits for the API and reports. Nothing can stop,
restart or reconfigure a user-managed runtime. Verified on the development machine (the `local-ai` pod's
rocm Ollama, GPU attached) and the start flow on a throwaway container. Also: Apache-2.0 licence,
`NOTICE`, `CONTRIBUTING.md`, `CHANGELOG.md`.

**Landed 2026-11-07 (skills done properly):** skills follow the Agent Skills specification (agentskills.io):
the full front matter is read (`name` checked against the rules and the folder, `description`, `license`,
`compatibility`, `metadata`, `allowed-tools`), and loading is **progressive**: the system prompt carries only
each usable skill's name and description, the model loads a skill's body with the new `skill` tool when a
task matches (and can read a skill's own reference files, confined to its folder), and `/skill-name request`
invokes one directly, with usable skills offered in the `/` completion. Every skill is scanned before it can
be enabled; project skills load only after the folder is trusted (see SECURITY). The old name-word matching
is gone.

**Landed 2026-11-07 (one-command MCP):** the MCP tab searches the official registry (`s`) and adds a hit
with Enter, showing how each would run or why it can't; the add form takes anything people copy from a
README (Claude Desktop/Cursor/VS Code/OpenCode snippets, `npx`/`uvx`/`docker run` lines, registry ids,
GitHub links with or without a `server.json`), adding several servers at once; `server.json` manifests
with PyPI and OCI packages and streamable-HTTP remotes resolve; and each server has a tool filter (`f`)
the hub enforces. Verified live: registry search, a pasted snippet added and tested, a filter saved.

**Still to do for the agent:** compaction for cloud models (their context limit isn't used yet, so they
aren't compacted); a `/compactions` view (each report is in the transcript and event log); checkpoints
that survive a restart; the Landlock fallback when bubblewrap can't run (today such commands are
labelled "WITHOUT a sandbox" and asked every time); sandboxing stdio MCP servers.

## M1: A real coding agent, with a TUI to match

The core product. Local models via Ollama only.

**In progress.** The terminal interface exists and can be tried with `re-code demo`, which plays a labelled recording.
Built so far: full-screen layout at any size, streaming markdown, syntax highlighting, tool timeline, approval cards,
line-level diffs, status line, scrollback, side panel, command palette, help, and a text input. Still to build for the
interface: conversation search, mouse text selection, word-level diffs, grouped tool rows, session and model switching,
and the file picker.

**MCP, skills and custom tools can be managed now**, ahead of M5: add, edit, delete, switch on and off, and test MCP connections for real. Models can't use any of them until the agent engine exists, and the screen says so.

Running `re-code` opens the welcome screen, which lists real models from Ollama and estimates how each fits this GPU.
Choosing one starts a real streaming chat with interrupt, an oversized-request check, and context figures. **It's chat
only:** tools and the agent loop aren't built, and conversations aren't saved yet.

- **TUI:** conversation, streaming markdown, syntax highlighting, diffs, tool timeline, status line, approval
  cards, scrollback, search, palette, model and session switching.
- **Engine:** the full loop, cancellation, recovery from malformed calls, loop detection, and crash recovery.
- **Tools:** read, list, grep, edit, write, delete, shell, git, todo.
- **Safety:** permission system, workspace confinement, sandbox if the spike is successful, checkpoints and undo.
- **Context:** ledger, `/context`, preflight, cached-token display, baseline budget test, tool-result budgets.
- **Models:** capability detection and the first conformance probe levels.
- **Also:** session persistence, headless `re-code run`, basic RPM.

**Demo:** in a real project, on a local model on this machine, ask it to find and fix a failing test. It reads
code, proposes an edit, gets approval, runs tests, iterates, and finishes. Interrupt it mid-command, kill the
terminal, relaunch, resume, and undo.

**Quality bar:** you choose to use it for a real task.

Note that the demo depends on the spikes showing that an installed model can do this at a usable speed. If none
can, that's important to learn in M0, and could change which models we recommend.

## M2: Long sessions

- Compaction with reports, then session recovery hardening.
- Project memory (FTS5), inspection and editing UI, and selective retrieval.
- Multiple concurrent sessions and project navigation.
- Layouts, themes, user keybindings.

**Demo:** a task that exceeds the context window several times finishes correctly without losing the objective,
and `/compactions` explains what happened.

## M3: Hardware and runtime management

- Full hardware profile, for AMD (tested here), plus NVIDIA, Intel and CPU-only (implemented, and marked untested
  until suitable hardware is available).
- Memory estimator calibrated against real loads.
- Recommendations and first-run onboarding.
- Model downloads, and app-managed Podman and native runtimes.
- Resource monitoring and benchmarks.
- `llama-server` as the second local runtime.

**Demo:** on a machine with no models, go from install to a working session with a suitable model. On this
machine, the recommendations match what happens in real loads.

## M4: Universal providers and harnesses — started early

Done ahead of schedule: Anthropic, OpenAI and Gemini reachable (OpenAI-compatible plus a native Anthropic adapter), custom OpenAI-compatible endpoints, OpenRouter free models, keyring-stored API keys, model switching within a session, and importing MCP servers from Claude Code. Still to do: harnesses (ACP, Claude Code), per-provider capability probing, and OAuth where a provider permits it.

## M4: Universal providers and harnesses (original plan)

- Anthropic, OpenAI and Gemini native adapters, each passing compatibility tests.
- Custom OpenAI-compatible endpoints, LM Studio.
- Keyring credentials, and verified auth policies per provider.
- Model switching within a session.
- Harnesses: generic ACP client, Claude Code (unmodified binary, the user's own sign-in).

**Demo:** the same session continues across a local and a cloud model, with an accurate ledger for each.

## M5: MCP, skills and extensibility

- **Done early:** management screens, stored configuration, real connection testing, cached metadata, and token cost per server.
- **Still to do:** giving these to models with lazy discovery and permissions, agent profiles, hooks, project trust, importing from other tools, and https:// servers.
- **Landed 2026-11-08 (skill hubs, milestone 4 track X3):** `/skill install official/security/godmode`
  (and `i` on the Skills tab) installs the way Hermes Agent does, from the Hermes index (a bare name or
  `hermes/<identifier>`; the index is ~42 MB, fetched once a day), GitHub (`owner/repo[/folder][@ref]`, tree
  and blob links, raw `SKILL.md` links), skills.sh links (its API needs a login, so the files come from the
  repository it points at, and the window says so), any `SKILL.md` link, or a local folder. Nothing fetched
  is run: files come through GitHub's tree listing and raw endpoint (no git, no tarball), into a staging
  folder under the cache, with limits (5 MB, 400 files, 2 MB per file), links and submodules skipped and
  reported, paths that leave the folder refused. The preview reads like Hermes' (`godmode [official] pinned
  @ 4134cbef · declares no tools, hooks, middleware, or env vars · 7 files · ~3.4k tokens`), then the scan,
  then `y` enable, `k` keep off, `n` discard, `r` read `SKILL.md`; a dangerous verdict can only be kept off.
  `.re-code-source.toml` in the installed folder records tap, repository, path and commit; `u` (or
  `/skill update <name>`) fetches the newest commit and lists the files that would change before replacing.
  `/skill search <words>` searches the Hermes index. A repository holding several skills offers them to pick
  from. Verified against a mock GitHub/skills.sh/Hermes (`crates/extensions/tests/skillhub.rs`) and live
  (`skillhub_live.rs`, ignored: `official/security/godmode` pinned, 9 files, two package-installer cautions
  in its scripts; `anthropics/skills/skills/pdf` with 8 scripts; the skills.sh link falling back to GitHub;
  the whole `anthropics/skills` repository offering 20 skills; the 42 MB Hermes index searched in ~2 s). The
  live run found a crash on multi-byte text in the env-var scan, fixed. Not supported yet: other git hosts
  (X8).
- **Landed 2026-11-08:** saved chats can be deleted from the Home list and the session picker (`d`, with a
  confirmation; the session writer deletes and re-sends the list; deleting the open chat starts a new one).
- **Landed 2026-11-08 (M small items, U5):** `/untrust` in the interface and palette, runtime autostart
  (`a` on the start confirmation, `ui:autostart:<id>` holds the exact command, Settings row with `d`), pager
  search (`/`, `n`, `N` over the lines as drawn), a scrolling Help overlay. Fixed: the stdio MCP sandbox read
  the plugin folders once at startup and not at all from the command line. Since 0.1.9: click-to-copy a code
  line (U5), hub progress in the TUI and CLI (U2), and the first-run tour (U4, `ui:onboarded`,
  `--no-onboarding`, `/tour`). 0.1.8 made the home screen fit the terminal and added `v` to read a row's
  details in full, which covers most of U1; U3 (runtimes fold rows, port-conflict banner) is not started.
- **Landed 2026-11-08 (production P1, P2, P4; live cloud L):** crash files and `doctor --report`
  (`crates/app/src/crash.rs`), `--version` with commit and date (`build.rs`), a week of logs, GitHub Actions
  CI (gate on Ubuntu, rpm in a Fedora 44 container, deb on Ubuntu 22.04 for an old glibc) and a tag-driven
  release workflow that publishes both packages with checksums and the changelog section. The first live
  cloud verification: `crates/providers/tests/live_cloud.rs` on Gemini 3.8 Flash did a chat, a tool call and
  the round trip, after fixing the adapter to echo Gemini's `thought_signature` (`ToolCall.extra`). Not
  reached live: OpenRouter's free quota was spent for the day, Groq is blocked from this network, Ollama
  Cloud wants credits, Hugging Face's included credits were spent; the Anthropic test exists and costs
  money to run. Still open from production: COPR (P3) and Flatpak (P5).
- **Landed 2026-11-08 (X6–X8):** OpenCode import (`crates/extensions/src/opencode.rs`, shape read from the
  developer's own config; keys never copied), command-line parity (`crates/app/src/cli.rs`: mcp, skill,
  plugin, memory, trust, untrust; tested end to end against the built binary in
  `crates/app/tests/cli.rs`), and stdio MCP servers under bubblewrap with a per-server opt-out
  (`Launch` in `probe.rs`, `McpServer.sandbox`). Not done from X8: cloning other git hosts under bwrap
  (GitHub remains the only hub source). Also: a stricter "free" rule for OpenRouter and paid models grouped
  per provider, after the developer hit a per-request-priced music model listed as free.
- **Landed 2026-11-08 (plugins, track X5):** `crates/extensions/src/plugins.rs`. The Hermes plugin catalog
  (`plugin-catalog.json`, 1,200 entries, shape read live: `entries[{name, repo, sha, subdir?, tier,
  maintainer, capabilities}]` plus `removed[]`, cached 6 h) and Claude Code marketplaces
  (`.claude-plugin/marketplace.json` read at a pinned commit; sources `./path`, `url`, `git-subdir`,
  `github` resolve to GitHub folders, `archive` is refused until zip support, `npm` and `command` are refused
  because they run code) feed the same fetch layer as skills. A plugin folder is read part by part:
  `skills/*/SKILL.md` and `commands/*.md` become skills (`SkillSource::Plugin`, removed with the plugin),
  `mcp.json`/`.mcp.json` servers join the MCP list with `${CLAUDE_PLUGIN_ROOT}` resolved, `hooks/hooks.json`
  is translated into `hooks.converted.toml` (PreToolUse/PostToolUse/Stop/SessionStart and the tool matchers
  that map) and never loaded on its own, and Python code, `desktop/`, `agents/`, `.lsp.json`, `bin/`, output
  styles and `settings.json` are listed as not run here. Installed plugins live under
  `~/.local/share/re-code/plugins/<name>/` with a `.re-code-source.toml`; `extensions.toml` keeps the
  followed `[[marketplace]]` records and `disabled_plugins`. The Plugins tab lists installed plugins,
  followed marketplaces and what Claude Code has installed (read from `installed_plugins.json`, read-only;
  Enter installs one from its folder). Verified against a mock GitHub and catalog
  (`crates/extensions/tests/plugins.rs`: catalog install with Hermes' header, removed entries, marketplace
  add/browse/install with converted hooks, a dangerous skill keeping a plugin off, a local marketplace) and
  in the TUI tests. Live checks are listed in the CHANGELOG once run.

**Demo:** add a server with many tools. Baseline context grows only by a small index, and tools still work.

## M6: IDE integration

- JetBrains MCP client with a curated tool group.
- `re-code acp` tested in RustRover and PyCharm.
- Reassess the plugin decision with real evidence. IPC as needed.

**Demo:** inside RustRover, use re-code from AI chat, with IDE diagnostics and a semantic rename, then pick up
the same session in the TUI.

## M7: Release

- Spec file, `mock` builds, COPR, signing. Upgrade and removal testing.
- Completions, man pages, documentation.
- Performance work, and an independent security review.
- Compatibility matrix, published with only what's been tested.

**Demo:** `dnf install` on a clean Fedora VM gives a working product. Removal leaves user data intact.

## Major technical risks

| Risk | Likelihood | Impact | Mitigation |
| --- | --- | --- | --- |
| **Models that fit this GPU may be too weak for real agent work.** `gpt-oss:20b` fits and passes a simple tool loop. The 27B coding models run partly on CPU at 22–25 tok/s. Neither has done real coding work. | Medium | High | Test multi-step tasks in M0. Honest recommendations. Lean tool set and prompts. |
| Local tool-call reliability varies by model and version | High | High | Dialects, profiles, conformance probe, recorded fixtures |
| Own scrollback and selection never feel as good as native | Medium | High | Prototype in M0, in real terminals. It's still possible to reconsider D2 at that point. |
| Sandbox friction leads to it being disabled | Medium | High | Good defaults, simple per-project profiles, spike early |
| Prompt injection can't be fully solved | Certain | High | Layered defences, and honest documentation of limits |
| Scope: the spec describes years of work | High | High | Milestone discipline. M1 is the product. Others are additions. |
| Terminal differences (Konsole, JediTerm) | High | Medium | Test matrix, no reliance on modern protocols |
| Ollama API or behaviour changes | Medium | Medium | Version-keyed fixtures, adapter isolated |
| Provider policies change | Medium | Medium | Check primary sources at implementation time |
| Shared Ollama contention with Open WebUI | Medium | Low | Show it in the UI |
| Upgrade breaks stored data | Low | High | Migration tests from all previous versions |

## Open architectural questions

Beyond decisions D1–D8 in [ARCHITECTURE](ARCHITECTURE.md#decisions-needed):

1. How much context can each candidate model hold on this GPU before spilling to CPU? (M0 spike)
2. Which installed models can complete a multi-step coding task, and can dialect fallback rescue the failures? (M0 spike)
3. How is the actual memory split affected by `OLLAMA_KV_CACHE_TYPE=q8_0`? Which free-VRAM figure predicts loading? (M3)
4. How can a running JetBrains IDE's MCP port be discovered reliably? (M6 spike)
5. Does ACP in JetBrains handle edit review and permissions well enough? (M6 spike)
6. How should dependency downloads work inside the sandbox: a network rule, or a separate step? (M1)
7. What subscription and auth rules apply to OpenAI and Google? Not yet verified. (M4)
8. What does session sharing look like if the TUI and an ACP instance use the same session? (M6)
9. Should project memory files be committed to the repo, shareable with a team, or kept private? (M2)
10. What should the policy be for a model catalogue: bundled data, or a signed remote update? Remote updates conflict with "nothing phones home" unless they are opt-in. (M3)
