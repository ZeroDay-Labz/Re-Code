# Agent Engine

Status: **proposed, awaiting agreement.**

The model is the reasoning engine. It can only ask for things. The engine validates, checks permission,
executes, and reports back.

## Why local agents usually fail

These are the design inputs. Each maps to a mechanism below.

| Failure | Mechanism |
| --- | --- |
| Prompt silently truncated, losing instructions | Preflight budget check, explicit `num_ctx` |
| Long silences mistaken for hangs, or killed by timeouts | Phase-aware watchdog and visible states |
| Tool-call format not recognised, leaked as text | Dialect parsers and probing |
| Slow turns, because prompt cache keeps missing | Stable prefix, append-only history |
| Model repeats the same failing call forever | Loop detector |
| Exact-match edits fail on whitespace | Tiered edit matching, with useful errors |
| Tool output floods the context | Budgeted results, with full output retrievable |
| Output cut off mid tool call | `Length` handling |

## Session actor and turn loop

```text
                 ┌──────────── user message ───────────┐
                 ▼                                     │
          BuildContext ──over budget──► Compact        │
                 │                         │           │
                 ▼◄────────────────────────┘           │
             Preflight ──cannot fit──► Fail, explain   │
                 ▼                                     │
              Request ◄───────────────┐                │
                 ▼                    │                │
          Stream + parse              │                │
                 ▼                    │                │
        ┌── tool calls? ──no──► Done ─┼────────────────┘
        ▼ yes                         │
     Validate ──invalid──► error result ─┐
        ▼                                │
     Permission ─deny─► denied result ───┤
        ▼ allow                          │
     Checkpoint and execute ─────────────┤
        ▼                                │
     Record structured result ◄──────────┘
        └──► loop detector ──ok──────────┘
                    └──tripped──► stop, ask the user
```

Each transition is an event in the durable log. An interrupt can arrive in any state. It cancels the
HTTP stream, kills the tool's process group, records what finished and what didn't, and returns to idle.
The next model request is given accurate results for any tool calls that were cut off, since an
unanswered tool call is itself a common cause of provider errors.

## Robustness

| Situation | Behaviour |
| --- | --- |
| Malformed JSON arguments | Conservative repair only (e.g. trailing fence). Otherwise return the parse error to the model, up to a retry limit. |
| Unknown tool or schema mismatch | Structured error listing valid tools or the field problem. Never executed. |
| `finish_reason = length` during a tool call | Discard the incomplete call, and retry once with a larger output allowance or a request for a smaller step. Never execute a truncated call. |
| Empty response | One retry, then fail visibly |
| Connection drop mid-stream | Partial text is kept and marked. Retry with backoff when it is safe, and never re-run tools. |
| Silence | Phase-aware watchdog: long allowance before first byte, as prompt evaluation legitimately takes a while, and shorter between bytes. It asks the user and doesn't kill by default. |
| Reasoning output | Stored and shown, but not re-sent by default, except where a provider requires it (as with signed thinking blocks) |
| Repeated identical failing calls | Loop detector stops and asks |
| Turn limit | Configurable step limit, with a prompt to continue |

## Native tools

**Fewer tools with clear schemas.** Each schema costs tokens on each request and adds a decision for
weak models. First set:

| Tool | Purpose | Default permission |
| --- | --- | --- |
| `read` | Read a file or range, with line numbers | Allow in workspace |
| `list` | List a directory, honouring ignore files | Allow in workspace |
| `grep` | Search content, using ripgrep libraries | Allow in workspace |
| `edit` | Replace text in a file | Ask |
| `write` | Create or overwrite a file | Ask |
| `delete` | Remove a file (its own permission category) | Ask, always |
| `shell` | Run a command in the sandbox | Ask |
| `git` | Read-only status, diff, log, show | Allow in workspace |
| `todo` | Maintain the task list | Allow |

Git writes go through `shell`, so they're subject to its approval and to rules such as "always ask for
`git push`".

### Edit matching

Edits are where weak models fail most, so matching is tiered and each tier demands a **unique** match:

1. Exact string.
2. Line-ending and trailing-whitespace tolerant.
3. Indentation tolerant, re-indenting the replacement.

A broader match is displayed in the diff preview, so nothing gets applied that the user can't see. On
failure, the error is written to help the model: it reports the closest candidate lines, or how many
locations matched. Files changed since the model read them are rejected, to prevent stale overwrites.

### Tool results

Results are structured: status, content, truncation details, and a handle. The full output goes to the
store, and the model receives a budgeted view:

```text
exit code 101. Showing last 80 of 2,431 lines (full output: result:7f3a).
```

Compiler and test output prioritises the end and error lines. The model can retrieve more by handle.

## Checkpoints

Before each file modification, the file's prior contents are saved, grouped by turn. `/undo` restores a
turn and shows what it's changing. It stores content itself and never depends on git, nor does it alter
the user's git state. Effects of arbitrary shell commands can't be fully captured, and the UI will say so.

## Project instructions

Loads `AGENTS.md`, `CLAUDE.md` and `.re-code/instructions.md`, de-duplicated by content hash. Each is
measured and has a size limit. Above the limit, the user sees a warning with its token cost instead of
silent truncation.

**This repository is the example.** Its `CLAUDE.md` is about 7,500 tokens by a chars/4 estimate, close to
a quarter of a 32k context before any work begins. Instructions are also project content, so an untrusted
project's file gets treated accordingly (see [SECURITY](SECURITY.md)).

## Task state

The `todo` tool gives a persistent task list, shown in the UI and preserved across compaction. It's the
anchor for "what was I doing?" after recovery.

## Subagents: sequential, one level

The `task` tool (`crates/app/src/subagent.rs`, run by `Chat::run_task`) gives the model a helper: a fresh
`Chat` on the same model and window, with the system prompt and instruction files but none of the history,
the reading tools by default (`read`, `list`, `grep`, `git`, `skill`) or the tools the parent names, at most
twelve steps. The helper shares the parent's permissions, approvals and checkpoints, so the user's rules and
undo cover it; its approvals arrive on the same card. The parent receives only the answer (cut at 8k
characters) and a list of the helper's tool calls, inside the task's own tool row. Tasks don't nest and run
one at a time, on purpose: with `OLLAMA_NUM_PARALLEL=1` a second concurrent request queues behind the first,
and switching context evicts the prompt cache, so parallel helpers would only slow a local model down. The
tool is offered when the window is at least 16k, since a helper means a second full prompt. The same
reasoning keeps **hidden background calls** to the active model out, such as for generating session titles.

## Permission flow

See [SECURITY](SECURITY.md) for the policy. Inside the engine:

1. Normalise the action into `PermissionRequest` (category, target path or command, risk notes).
2. Evaluate rules, narrowest scope first: session → project → user → defaults.
3. On "ask": emit `ApprovalRequired` and suspend that tool only. The UI stays fully usable.
4. Record the decision and its source in the execution history.

The engine, not the model, has the final decision. A model's claim that an action is safe carries no weight.
