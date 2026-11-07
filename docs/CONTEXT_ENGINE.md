# Context Engine

Status: **largely implemented (2026-11-07).** Token budgets here are targets, not measurements. What exists:
the ledger behind `/context` (`compact::ledger`), per-model windows with an auto policy
(`recode_core::context`), compaction for local and cloud models, `/compactions` with originals kept in the
`compactions` table, calibration of the character-per-token ratio from reported usage, a truncation detector
for local runtimes, re-injection after a summary, a thrash guard, idle compaction. Not yet: the budget test in
CI, superseded-first elision, a per-request raw payload in the store (`/context raw` shows the next request).

The promise: **at any moment, re-code can show exactly what it sent and what each part cost.**

## Investigating the 100,000-token problem

The spec asks us not to assume MCP alone was responsible. Anything added automatically is a suspect:

| Source | How it inflates | Our control |
| --- | --- | --- |
| Tool and MCP schemas | Full schemas on every request, for every server | Lazy discovery. A compact index, with schemas loaded on use. |
| Project and user instructions | Long files, sometimes duplicated across several paths | Hash de-duplication, size warnings |
| Same content arriving by several routes | Auto-loaded, then pasted, then attached from an editor selection | Hash detection of large duplicates, with a notice |
| Tool results | Large output retained in every later request | Budgets and elision |
| Skills and agent lists | Long catalogues of things that might be useful | Short index, or on demand |
| Memory | Everything injected "in case" | Retrieve by relevance, with a budget |
| Environment snapshots | Repo trees, git status and logs up front | Only on request |
| Reasoning | Re-sent thinking blocks | Stripped unless required |
| Hidden background requests | Tokens spent outside the visible turn | Disallowed, or itemised in the ledger |

**Measured evidence, 2026-11-06:** RustRover's MCP server offers 83 tools, and sending all their schemas would cost roughly 73,500 tokens per request. That's estimated at three characters per token, which is deliberately pessimistic, so the true figure may be lower. Either way, a single IDE connection could account for most of the 100,000-token problem, and it would do so in an empty project. re-code shows this cost per server on its MCP tab.

**This very session is an example.** The master spec reached the model through at least three routes:
auto-loaded `CLAUDE.md`, a paste, and an IDE selection. The session's tool list also names several hundred
MCP tools. The harness defers most of their schemas, which is a good pattern to learn from.

## The ledger

Every request produces a ledger entry, persisted with the session:

```rust
pub struct RequestLedger {
    pub components: Vec<Component>, // kind, source, content hash, TokenCount
    pub budget: Budget,
    pub reported: Option<ReportedUsage>,
    pub raw_request: StoredBlobRef,  // exact payload, for inspection
}

pub enum TokenCount {
    Exact { tokenizer: String },
    Estimated { method: String, confidence: Confidence },
}
```

Component kinds: system, developer, project instructions, history, tool definitions, MCP schemas,
retrieved files, memory, tool results, agent state, provider metadata.

`/context` shows it:

```text
 Context  ~9,140 / 32,768 (estimated)     last reported: 8,902 in, 7,410 from cache
 ────────────────────────────────────────────────────────────────
 system prompt             ~620  ▏
 tool definitions          ~980  ▎
 project instructions    ~1,240  ▍  AGENTS.md
 conversation history    ~3,300  █
 tool results            ~3,000  █  largest: cargo test (result:7f3a)
 reserved for response    4,096
```

(Illustrative layout, not measured data.)

`/context raw` shows the exact last payload, with secrets redacted.

## Budget test

CI fails if a new session in an empty project exceeds its budget. This protects us from gradual growth,
and it's the single most direct answer to the problem:

| Component | Target maximum |
| --- | --- |
| System prompt | 800 tokens |
| Built-in tool definitions | 1,200 tokens |
| Everything else, empty project | 200 tokens |
| **Total** | **2,200 tokens** |

Increasing a budget means editing the test, so it shows up in code review.

## Token counting

| Situation | Method | Label |
| --- | --- | --- |
| Provider reported usage | Provider's numbers | Exact |
| Provider has a counting endpoint | Call it where cheap (e.g. Anthropic) | Exact |
| Tokenizer known and available | Local tokenizer | Exact, for that tokenizer |
| Otherwise | Calibrated chars-per-token ratio, updated per model from reported usage | `~` estimate |

Reports are checked against estimates. Measured on Ollama 0.40.14: `prompt_eval_count` is the whole prompt
including cached tokens, so it can be used directly for calibration, and uncached tokens are
`prompt_eval_count − prompt_eval_cached_count` ([spike report](spikes/2026-11-06-ollama.md)).

**Truncation detector:** if the reported prompt size is far below our estimate, warn the user that
content might have been truncated. It won't be a certainty, and it isn't presented as one.

## Budgeting

```text
input budget = num_ctx − reserved output − safety margin
```

- **Preflight is mandatory.** A request that doesn't fit is never sent. Measured: Ollama silently drops
  the oldest turns, and with too little room left it can return an empty reply with `done_reason: length`.
- **`num_ctx` is chosen per session,** from the model's limit, the hardware estimate, and any measurement.
  It's kept fixed during a session because changing it reloads the model and resets the cache (measured).
- **Reserved output** scales with context. A small context can't reserve the same absolute amount.

## Prompt cache discipline

On local runtimes this has a large effect. Measured on a 3.4k-token prompt: 1.28 s uncached and 0.01 s
cached. Changing only the system message dropped the cache from 3,445 tokens to 5.

- **Order:** system → tools → project instructions → history. Most stable first.
- **No volatile values in the prefix.** Timestamps, git status or token counts don't belong there.
- **History is append-only** between compactions. Rewriting early messages invalidates everything after.
- **Elision happens in batches,** not continuously, so each compaction causes one cache miss, not many.
- **Tool order is deterministic.**
- **Cache hit rate is displayed in diagnostics** using `prompt_eval_cached_count`, so regressions are visible.

This creates real tension with the goal of minimising tokens: removing old tool results saves context
but invalidates the cache. Hence the batching: tolerate some stale results until a threshold, then pay
for one cache miss to reclaim a lot.

## Compaction

Runs before overflow. It starts at a fraction of the input budget, so there's room for the work itself.

**Stage 1: elide (no model call).** Replace old tool results with references:

```text
[result:7f3a elided: cargo test, exit 101, 2,431 lines. Retrieve with read_result.]
```

Results that were superseded go first, such as a file read before later edits. This is deterministic,
instant and inspectable, and often enough on its own.

**Stage 2: summarise (a model call, shown in the UI).** Older turns are summarised into a fixed structure:

- Objective, in the user's original words
- Decisions and why
- Findings
- Unfinished work and task list
- Files touched, by path
- Errors still unresolved

The objective, task list and recent turns are retained verbatim rather than summarised.

**Stage 3: verify.** Each compaction records a report of what was retained, summarised and discarded, with
token counts before and after. `/compactions` shows it, and originals are never deleted from the store, so
the agent or user can retrieve them later.

If summarising fails, the engine keeps the elided version and tells the user. A failed summary must not
damage the session.

**Implemented 2026-11-06, extended 2026-11-07** in `crates/app/src/compact.rs`, for local and cloud models
(the window comes from `recode_core::context`: `num_ctx` capped at the model's limit, or the listing's
limit; unknown means no automatic compaction and a notice). Elision keeps the 4 most recent tool results and
shortens older ones anywhere, including inside the current turn, since one long agent turn is the usual way
a session overflows. Summaries are inserted as an assistant message after the objective; `/compact <focus>`
and a project's "Compact instructions" section are appended to the summarising instructions. Each
compaction's report and the messages it replaced go to the `compactions` table; `/compactions` lists them
and Enter shows the originals. After a summary the task list, up to five recently touched project files
(re-read, ≤ 5k tokens each) and loaded skill bodies (≤ 5k each, 25k in all) come back as notes. A third
summary in one turn fails the turn with an explanation. Measured on `gpt-oss:20b`: ~3.8k → ~1k tokens
(estimated) for a 60-message history, with every heading filled correctly.

## Selective retrieval and repeated content

- Files are read by the agent as needed, not preloaded.
- A compact project overview is built on request, and has a budget.
- If an unchanged file is read again, the model gets a short reference to the earlier result, confirmed
  by content hash.

## Memory (Milestone 2)

**Storage:** SQLite with FTS5 keyword search. **Embeddings aren't needed at first.** Keyword search over a
modest set of notes is predictable and needs no model. It can be added if retrieval quality proves
inadequate, since an embedding model competes for memory on a machine that's already constrained.

| Kind | Example | Injected |
| --- | --- | --- |
| Project instructions | `AGENTS.md` | Always, measured |
| Decisions, architecture notes | "We use rusqlite, because…" | When relevant, within budget |
| Known issues | "Flaky test in X" | When relevant, within budget |
| User preferences | "Prefer small commits" | Short list, measured |

Rules:

- Memory is inspectable and editable in a TUI view, and every injection appears in the ledger.
- The agent can propose a memory. **Durable preferences require user approval,** so a compromised tool
  result can't plant a long-lived instruction.
- Each project is strictly isolated.

## Session recovery

The event log rebuilds a session after a crash or restart. Turns in flight are marked interrupted, and tool
calls of unknown outcome are flagged. They **aren't** automatically re-run, since a command may have partly
completed. The agent is told plainly and can inspect the state.
