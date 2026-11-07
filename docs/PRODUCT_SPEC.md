# Product Specification

Status: **proposed, awaiting agreement.** This distils the master specification in `CLAUDE.md` and
challenges it where the evidence points elsewhere.

## Understanding of the product

re-code is one terminal application in which a developer can use almost any AI model to do real
work on a real codebase. The model does the reasoning. The application supplies everything else:
files, shell, git, permissions, context management, and the runtime that serves the model.

The immediate goal is narrower, and it decides priorities:

> **A Fedora terminal coding agent that actually works with local models, with a UX people enjoy.**

The universal-provider vision stays in the architecture, but it must not delay that goal. A product
that connects to 20 providers and fumbles a multi-file edit on a local model has failed.

## Defining features

These are the features that justify the product existing. Everything else is table stakes.

1. **Local models that really work.** Explicit context control, tool-dialect handling, and failure
   recovery built for models that are slower and less reliable than frontier cloud models.
2. **Radical context honesty.** Itemised accounting of every request, with a tested overhead budget.
   This directly answers the 100,000-token problem.
3. **Measured model capability.** A conformance probe answers "can this model do agent work on this
   machine?" with a tested result, not a marketing claim.
4. **Hardware-aware recommendations.** They count KV cache and real free VRAM, not just weight size.
5. **A terminal interface with its own identity** that always shows what the agent is doing.
6. **A respectful guest on your machine.** Clear separation of user-managed and app-managed runtimes.
7. **Natural JetBrains integration**, using what the IDEs already expose before building a plugin.

## Non-goals

- A desktop GUI (Electron, Tauri or otherwise).
- Bypassing provider authentication, subscription limits or usage controls.
- Telemetry by default (see missing requirements).
- Parity with any other tool's feature list. We take interaction ideas, not templates.

## Where this document challenges the master specification

| Spec position | Challenge | Evidence |
| --- | --- | --- |
| Ollama is an existing container at `localhost:11434` | It is a **Quadlet user service that is currently stopped**. A stopped `--rm` container doesn't appear in `podman ps -a`. Discovery must read systemd units as well. | Observed 2026-11-06, see [HARDWARE_AND_RUNTIME](HARDWARE_AND_RUNTIME.md#what-was-found-on-the-development-machine) |
| 19 crates | Seven at first. The others have no boundary that has been proven by a second implementation. | [ARCHITECTURE](ARCHITECTURE.md#workspace-structure) |
| Milestone 1 is a chat client, agent tools come in Milestone 2 | Merge them. A chat client doesn't test the main risk, and the TUI's hardest states only exist with tools. | [ROADMAP](ROADMAP.md) |
| Context work is Milestone 3 | The ledger and preflight check must exist from the first request. Without them, local agents silently lose their system prompt. | [CONTEXT_ENGINE](CONTEXT_ENGINE.md) |
| RPM is Milestone 8 | Build a basic RPM early. Packaging surprises are cheap early and expensive late. | [ARCHITECTURE](ARCHITECTURE.md#fedora-rpm-packaging) |
| A JetBrains plugin is likely | JetBrains IDEs already host ACP agents and expose MCP. A plugin may be needed only for gaps, if at all. | [IDE_INTEGRATION](IDE_INTEGRATION.md) |
| OAuth and subscription support for cloud providers | For Claude this isn't available to us. Anthropic doesn't permit third-party apps to offer Claude.ai login. An unmodified Claude Code binary, signed in by the user, is the permitted route. | [PROVIDER_ARCHITECTURE](PROVIDER_ARCHITECTURE.md#authentication) |

## Missing requirements

The specification doesn't address the following. Proposed answers are given. Those that need a
decision are repeated in [ARCHITECTURE](ARCHITECTURE.md#decisions-needed).

| Gap | Why it matters | Proposal |
| --- | --- | --- |
| **Undo and checkpoints** | The most important safety net for an agent that edits files, and a large UX win. Git alone isn't enough: changes may be uncommitted and the project may not use git. | Snapshot every file before an agent edit. `/undo` restores per turn. **Add to the first milestone.** |
| **Headless mode** | Needed for scripting and CI, and it is how we test the engine without a TUI. | `re-code run "prompt"` with text or JSON event output. |
| **Project trust** | A cloned repo could carry hooks, MCP servers or instructions that attack the user. | Ask on first entry. Repo-supplied executable configuration is inert until trusted. |
| **Licence** | Affects dependencies and contribution, and it is awkward to change later. | **Decision needed.** |
| **Telemetry and privacy** | Users of local models often choose them for privacy. | No telemetry. Diagnostic reports are created locally and only on request. |
| **Retention** | SQLite will hold source code and command output indefinitely. | Inspectable size, per-session deletion, optional age limit. |
| **Schema evolution** | Config and database formats must survive upgrades. | Versioned migrations, tested from day one. |
| **Accessibility** | Heavy TUIs can exclude screen-reader and low-vision users. | `NO_COLOR`, reduced motion, high-contrast theme, no meaning carried by colour alone, plain headless mode. |
| **Shared-runtime contention** | Your Ollama runs `OLLAMA_NUM_PARALLEL=1`, and Open WebUI uses the same pod. Sessions can evict each other's prompt cache or queue behind one another. | Show queueing and cache misses in the UI. Warn on contention. |
| **Concurrent instances** | Two terminals, one project, one database. | WAL mode and a per-session write lease. A session has one active writer. |
| **Offline operation** | Local-first must function without a network. | No network needed for local use. Nothing phones home. |
| **Model licences** | Downloads carry licence terms, some restrictive. | Show the licence before a managed download. |
| **Updates** | RPM users update through dnf, but others won't. | Package manager only at first. No self-updater. |

## Users

- **Experienced developer** (the initial user): wants control, wants to see the raw truth, and will
  notice a dropped frame.
- **Beginner:** should never need to know what `num_ctx` is. Advanced settings exist, but they are
  never required to reach a working session.

Default language is plain ("This model will run partly on your CPU, so it will be slower"), with
the technical detail one keypress away.
