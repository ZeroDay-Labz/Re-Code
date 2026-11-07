# Provider Architecture

Status: **proposed, awaiting agreement.** No provider is supported yet.

## Three separate concepts

| Concept | Question it answers | Examples | Abstraction |
| --- | --- | --- | --- |
| **Provider** | How do I talk to a model? | Ollama API, Anthropic Messages, OpenAI Chat | `Provider` |
| **Runtime** | How does a model server get running? | Ollama in Podman, `llama-server` as a process | `Runtime` |
| **Harness** | How do I delegate to another complete agent? | Claude Code, Codex, Gemini CLI | `Harness` |

A runtime usually exposes a provider, but they have separate lifecycles. A remote Ollama server is a
provider with no local runtime. A harness brings its own loop, tools and permissions, so it isn't a
kind of provider: our engine does not run its loop when a harness is driving.

## Provider interface

```rust
#[async_trait]
pub trait Provider: Send + Sync {
    fn id(&self) -> &ProviderId;
    async fn health(&self) -> Result<Health, ProviderError>;
    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError>;
    async fn capabilities(&self, model: &ModelId) -> Result<ModelCapabilities, ProviderError>;
    fn stream(
        &self,
        request: ChatRequest,
        cancel: CancellationToken,
    ) -> BoxStream<'static, Result<StreamEvent, ProviderError>>;
}

pub enum StreamEvent {
    TextDelta(String),
    ReasoningDelta(String),
    ToolCallDelta { index: usize, id: Option<String>, name: Option<String>, args: String },
    Usage(ReportedUsage),
    Finished(FinishReason),
    ProviderSpecific(serde_json::Value),
}
```

Three things stop this becoming a lowest common denominator:

- `ChatRequest` carries a typed `provider_options` value (Ollama's `num_ctx` and `keep_alive`,
  Anthropic's cache breakpoints).
- `ProviderSpecific` passes unmodelled data through to diagnostics instead of dropping it.
- `FinishReason` keeps `Length` distinct from `Stop`, since a truncated tool call needs its own
  recovery path.

`ReportedUsage` is used only for numbers that came from the provider. Estimates are a different type,
so the compiler stops one being displayed as the other.

## Capabilities have a provenance

Saying "this model supports tools" is too vague to build on. Every capability records where the claim
came from:

```rust
pub struct Capability<T> { value: T, source: Source }
pub enum Source {
    Declared,                     // the server or template says so
    Profile { id: String },       // from our shipped compatibility data
    Probed { at: DateTime<Utc>, runtime_version: String },
    Measured { samples: u32 },    // accumulated in real use
}
```

Priority runs `Measured` → `Probed` → `Profile` → `Declared`.

**Why this matters, measured on this machine:** all 8 models tested declare `tools`, and **3 of the 8 failed
every tool-call run** ([spike report](spikes/2026-11-06-ollama.md)). The reverse error also exists: two of the models that passed have no `.Tools` in
their template because they use built-in renderers, so a template check would wrongly reject them.

## Model conformance probe

`re-code probe <model>` runs real requests and stores the results:

| Level | Test |
| --- | --- |
| L0 | Streams text and terminates correctly |
| L1 | Makes a single tool call with valid arguments |
| L2 | Uses a tool result correctly and continues |
| L3 | Chains several steps, including parallel calls if offered |
| L4 | Recovers after being told a tool call failed |
| L5 | Completes a tiny real task: find a bug, edit the file, run a test |

Results are scored over repeated runs because local models are non-deterministic, and they are stored
per model, quantisation, runtime version and machine. Changing any of these invalidates the result.
The UI then speaks plainly: "Works well for agent tasks (passed 5/5 levels, tested today)".

## Tool-call dialects

Tool calls don't arrive in a single format, so dialect handling is separate from transport:

| Dialect | Handling |
| --- | --- |
| Native (structured fields) | Used when probing shows it works |
| Hermes-style and similar tagged text | Parsed from text output by a streaming parser |
| JSON in text or code fences | Conservative fallback, only when a profile enables it |
| Prompted (no native support) | We describe tools in the prompt and parse the reply |

The parsers are streaming so tool-call markup never flashes up as visible text. Which dialect a model
uses is stored in its compatibility profile, which is TOML data so new models don't need a Rust change.

**Never guess silently.** If a dialect fallback activates, the user can see it.

## Ollama adapter (first provider)

**Use the native `/api/chat`, not the OpenAI-compatible endpoint.**

- `options.num_ctx` works on the native API. **Measured:** the OpenAI-compatible endpoint ignored it and
  reloaded the model at the server default ([spike report](spikes/2026-11-06-ollama.md)). Context control is non-negotiable, so this settles it.
- It reports `prompt_eval_count`, `prompt_eval_cached_count`, `eval_count`, `load_duration` and
  durations, which feed the ledger and status display directly.
- `think` and `keep_alive` are available.

Ollama-specific points to handle:

- **`num_ctx` must be sent on every request, and over-budget prompts must never be sent.** Measured:
  Ollama truncates silently, returning HTTP 200 with no error. In one test it kept the system prompt and
  dropped the oldest turns.
- **Keep `num_ctx` stable within a session.** Measured: changing it reloads the model and resets the cache.
- **Streaming tool calls** are documented only as "partial tool calls, if any". They arrived in one chunk
  in every passing run on 0.40.14. The adapter still has to tolerate any split, and we'll record fixtures.
- **Don't send `think: false` by default.** Measured: on `qwen3:4b` it put reasoning into `content` and no
  tool call followed within 400 tokens. Thinking settings belong in the compatibility profile.
- **Cold start.** Requests produce no bytes during loading, so timeouts must be phase-aware.

## Other local runtimes

Listed honestly. **None of these is claimed as supported until tested.**

| Runtime | Approach | Notes |
| --- | --- | --- |
| llama.cpp `llama-server` | OpenAI-compat adapter plus its native extras | Likely second target. It has a tokenize endpoint for exact counts (to verify). |
| LM Studio | OpenAI-compat profile | Thin |
| vLLM, SGLang | OpenAI-compat profile, with tool parser configured server-side | Primarily CUDA-oriented. ROCm viability on an RX 6900 XT is **unverified**. |
| HF Transformers | Only via a serving layer | No stable agent-grade API to target directly. Deferred. |
| PyTorch | Not a provider | It's a framework with no server API. A user's custom service would connect as a custom OpenAI-compatible endpoint. |

## Cloud providers

**Anthropic** and **Gemini** get native adapters, because their features (prompt caching, thinking
blocks, multimodal input) would be lost through a compatibility layer. **OpenAI** gets a native adapter
too. **Mistral, xAI** and custom endpoints use a configurable OpenAI-compatible adapter unless testing
shows a need for more.

## Current status (2026-11-06)

Implemented and tested: the **Ollama**, **OpenAI-compatible** (OpenAI, Gemini via its OpenAI endpoint, OpenRouter including free `:free` models, and any custom server), and **Anthropic** (native Messages API) adapters, each with model listing and SSE streaming. The OpenAI-compatible streaming path is verified against a live server. Cloud providers are added on the home screen's **Providers** tab, API keys are stored in the OS keyring (never on disk), and `https://` works now that TLS (rustls) is built in. Still chat-only: tool calling isn't sent yet. OAuth is not implemented, per the authentication notes below.

## Authentication

**Claude, per Anthropic's [legal and compliance page](https://code.claude.com/docs/en/legal-and-compliance):**

- Developers building products, including with the Agent SDK, should use API keys.
- Third-party apps may **not** offer Claude.ai login or route requests through consumer plan credentials.
- A user **may** sign in to the **unmodified** Claude Code binary with their own subscription.

So re-code will support:

1. **Anthropic API key:** a direct provider.
2. **Claude Code as a harness:** the user's own installed binary, signed in through Anthropic's own
   flow. We never read, store or forward its tokens, and we must not use Claude Code branding in a way
   that implies endorsement.

**OpenAI, Google and others:** their policies have **not been verified** in this research. Each must be
checked against primary documentation before implementation (M4). A subscription is never assumed to
grant API access.

Credentials are stored in the OS keyring. See [SECURITY](SECURITY.md).

## Harness interface

```rust
pub trait Harness: Send + Sync {
    fn id(&self) -> &HarnessId;
    async fn detect(&self) -> Result<HarnessStatus, HarnessError>; // installed? version? signed in?
    async fn start(&self, spec: SessionSpec) -> Result<Box<dyn HarnessSession>, HarnessError>;
}
```

Sessions translate their events into our `Event` type, so the TUI displays them with the same timeline,
marked clearly as running under the external agent's own permissions. Candidates: Claude Code's headless
stream-JSON mode, and any ACP-compatible agent through one generic adapter. ACP is the priority there,
since one adapter covers several agents.

**Caveat:** a harness's tool calls are outside our permission engine, and the UI must say so.

## Failure behaviour

- **No silent fallback** to another model or provider, ever. Offer it, and let the user choose.
- Errors say what happened and what to do: "Ollama isn't running. Its service `ollama.service` is
  installed but stopped. Start it?"
- Provider request diagnostics are available in debug mode, with credentials redacted.
