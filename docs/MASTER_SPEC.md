# PROJECT MASTER SPECIFICATION
# Universal AI Development Workbench
### Rust · Linux First · Terminal Native · Universal Model Support

You are the principal software architect, senior Rust engineer, systems engineer, product designer, and technical lead for this project.

Your responsibility is to help design and build an exceptionally polished, reliable, extensible, terminal-native AI development environment.

This is a serious long-term software project. We are not interested in producing a superficial prototype that imitates an existing coding agent.

We want to build a product that can compete with and potentially surpass the experience of Claude Code, OpenCode, and Kilo Code.

The application should combine a powerful coding-agent environment, universal model connectivity, intelligent hardware detection, local inference management, project intelligence, and deep IDE integration.

The application must be powerful enough for experienced developers while remaining approachable to beginners.

**Our guiding philosophy: Make the most capable AI development environment possible without forcing users to understand the complexity underneath it.**

---

# 1. PRODUCT IDENTITY AND VISION

We are building a universal AI development workbench.

It is a Rust application that runs primarily in the terminal and provides a sophisticated interactive terminal user interface.

It is NOT a desktop GUI application.

Do not build an Electron application, Tauri application, or standalone desktop GUI.

The terminal is the primary interface.

The application should eventually provide:

- Universal cloud and local model connectivity.
- API key and officially supported OAuth authentication.
- Multiple inference engines.
- Intelligent hardware detection.
- Automatic model recommendations.
- Managed local model downloads and inference runtimes.
- A real coding-agent execution environment.
- Filesystem and project awareness.
- Terminal execution.
- Git integration.
- MCP support.
- Skills and agent profiles.
- Hooks and custom tools.
- Project and session memory.
- Advanced context management.
- JetBrains IDE integration.
- Extensible integrations with other development environments.
- Multiple simultaneous projects and sessions.
- Powerful permissions and security controls.

We want one application through which developers can manage and use virtually any AI model or supported coding agent.

We are not building another Ollama frontend.

We are not building a simple chat application.

We are building a complete AI-powered development environment.

---

# 2. PLATFORM STRATEGY

## Primary platform: Linux

Linux is our initial and primary target.

Our first official distribution should be a native Fedora-compatible RPM package.

Initial environment:

- Fedora Linux.
- RPM-based distributions.
- x86_64.
- AMD GPU systems.
- NVIDIA GPU systems.
- Intel GPU systems where supported.
- CPU-only systems.

Future Linux support should include:

- Fedora.
- Ubuntu and Debian.
- Arch Linux.
- Other compatible distributions.

Linux functionality and polish take priority.

## Secondary platform: Windows

Design the Rust codebase so Windows support can be introduced without rewriting the application.

Keep platform-specific functionality behind appropriate interfaces.

Examples:

- Filesystem operations.
- Process management.
- Terminal execution.
- Hardware discovery.
- GPU discovery.
- Container management.
- Runtime installation.
- Configuration directories.
- Shell integration.

Do not sacrifice Linux quality for premature Windows support.

## Packaging requirements

The initial release should provide:

- Native Fedora RPM.
- Correct installation paths.
- Application configuration directories.
- Application data directories.
- Logging directories.
- Shell completions where practical.
- Clean installation and removal.
- Upgrade support.
- Dependency management.
- Appropriate system integration.

The application should not require root privileges for ordinary use.

Prefer rootless Podman for managed containers.

Do not make Docker mandatory.

---

# 3. RUST ARCHITECTURE

Rust is the primary implementation language.

The architecture must be modular, maintainable, asynchronous where appropriate, and extensible.

Evaluate appropriate libraries for:

- Async execution.
- HTTP communication.
- Serialization.
- Terminal rendering.
- Terminal input.
- Database storage.
- Filesystem operations.
- Process execution.
- Syntax parsing.
- Source-code searching.
- MCP.
- LSP.
- IPC.
- Logging and diagnostics.
- Hardware discovery.

Potential technologies include:

- Tokio.
- Reqwest.
- Serde.
- SQLite.
- Ratatui.
- Crossterm.
- Tree-sitter.
- ripgrep-compatible search.
- RMCP or another maintained Rust MCP implementation.
- Tracing.
- Appropriate Rust IPC libraries.

These are candidates, not mandatory dependencies.

Evaluate maintenance, licensing, security, performance, compatibility, and long-term viability before selecting libraries.

Avoid excessive abstraction and unnecessary crates.

Establish clear subsystem boundaries before implementation.

The architecture should allow multiple interfaces to communicate with the same agent engine.

The initial interface is the TUI.

Future integrations may include JetBrains plugins and other IDE integrations.

---

# 4. TERMINAL USER INTERFACE

The TUI is one of the most important parts of this product.

Do not treat it as a basic terminal wrapper.

It must be visually excellent, intuitive, responsive, and enjoyable to use.

Study the interaction patterns of:

- Claude Code.
- OpenCode.
- Kilo Code.
- Modern terminal applications.
- Modern IDEs.
- Advanced developer tools.

Use those products as references, not templates to copy.

Develop a recognizable product identity.

## Interface requirements

The interface should support:

- Multiple sessions.
- Multiple projects.
- Project navigation.
- Session history.
- Rich conversation rendering.
- Streaming responses.
- Markdown.
- Syntax-highlighted code.
- Inline diffs.
- File-change previews.
- Tool execution timelines.
- Model and provider selection.
- Context usage visualization.
- Token usage.
- Runtime status.
- Hardware status.
- Search.
- Command palette.
- Keyboard shortcuts.
- Configurable layouts.
- Themes.
- User customization.

The interface should adapt gracefully to different terminal dimensions.

Avoid excessive visual clutter.

Use color, borders, icons, spacing, and typography thoughtfully.

Provide sensible defaults and advanced customization.

The user should always understand:

- Which model is active.
- Which provider is being used.
- What the agent is doing.
- Which tools are executing.
- Which files are changing.
- Whether approval is required.
- How much context is being used.
- Whether a task is running, waiting, completed, or failed.

Tool execution should be visible and understandable.

The user should be able to interrupt an agent immediately.

---

# 5. UNIVERSAL PROVIDER ARCHITECTURE

A central requirement is universal model connectivity.

We must distinguish three concepts:

1. Model providers.
2. Inference runtimes.
3. Agent harnesses.

They are related but are not interchangeable.

Build separate abstractions for each.

## Cloud providers

Plan support for:

- Anthropic.
- OpenAI.
- Google Gemini.
- xAI.
- Mistral.
- Other major providers.
- Custom OpenAI-compatible endpoints.

Support API keys and officially supported authentication methods.

Where providers offer OAuth or subscription-based authentication workflows, investigate and support them through official mechanisms.

Do not assume a consumer subscription automatically provides API access.

Do not bypass authentication, provider restrictions, subscription limitations, or usage controls.

Make it easy to add providers without rewriting the agent engine.

## Local inference providers and runtimes

Plan integration with:

- Ollama.
- llama.cpp.
- vLLM.
- SGLang.
- LM Studio.
- Hugging Face Transformers.
- PyTorch-based inference services.
- Other compatible inference engines.

Recognize that PyTorch is a framework rather than a standardized inference-server API.

Use appropriate adapters for each runtime.

Support:

- Existing local inference servers.
- Local model discovery.
- Model downloads.
- Quantization selection.
- Runtime configuration.
- Model lifecycle management.
- Health checks.
- Streaming.
- Tool calling where supported.

## Agent harness integrations

Investigate integration with:

- Claude Code.
- OpenAI Codex.
- Gemini CLI.
- OpenCode.
- ACP-compatible agents.
- Other supported external coding agents.

Use official APIs, documented protocols, and supported authentication.

Do not assume a provider API and a coding-agent subscription are the same thing.

## Capability negotiation

Different models have different capabilities.

Build a model capability system that identifies:

- Native tool calling.
- Structured output.
- JSON modes.
- Streaming.
- Vision.
- Multimodal input.
- Reasoning capabilities.
- Context limits.
- Output limits.
- Supported message formats.
- Provider-specific features.

Do not force every model into a lowest-common-denominator interface.

Preserve provider-specific features where possible.

Local model tool calling is not universally reliable.

Implement compatibility profiles, testing, and appropriate fallbacks.

---

# 6. HARDWARE DETECTION AND MODEL RECOMMENDATIONS

This should be one of our defining features.

When the application starts for the first time, it should inspect the user's computer and build a hardware profile.

Detect:

- CPU model.
- CPU architecture.
- Core and thread counts.
- Relevant instruction-set capabilities.
- System RAM.
- Available system memory.
- GPU vendor.
- GPU model.
- Dedicated VRAM.
- Unified memory where applicable.
- GPU compute capabilities.
- Installed drivers.
- Installed compute runtimes.
- Available disk space.
- Existing inference servers.
- Existing models.
- Available container runtimes.
- Operating-system information.

Support AMD, NVIDIA, Intel, CPU-only systems, and future platforms such as Apple Silicon where appropriate.

## Model recommendation engine

Based on the detected hardware, recommend models and configurations.

Consider:

- Model architecture.
- Parameter count.
- Quantization.
- Weight memory requirements.
- KV cache requirements.
- Context length.
- GPU offloading.
- CPU offloading.
- Runtime overhead.
- Available system memory.
- Available VRAM.
- Performance characteristics.
- Tool-calling reliability.
- Intended workload.

A model should not be classified as compatible merely because its weights fit in VRAM.

Account for actual runtime memory requirements.

Provide recommendations for:

- General-purpose models.
- Coding models.
- Reasoning models.
- Small and fast models.
- Large models.
- Embedding models.
- Vision models.

Present recommendations in understandable language.

For example:

"Recommended: This model should provide a good balance of performance and memory use on your computer."

"Advanced: This model may require CPU offloading and reduced context length."

"Cloud: Your hardware may struggle with this model, but you can use the provider's hosted version."

Clearly distinguish estimated performance from measured performance.

## Hardware benchmarks

Provide optional benchmark functionality.

Measure:

- Inference speed.
- Tokens per second.
- Memory usage.
- Model loading time.
- GPU utilization.
- Context performance.
- Tool-calling reliability where relevant.

Use measured performance to improve future recommendations.

Do not invent benchmark results.

---

# 7. LOCAL MODEL AND RUNTIME MANAGER

Build a dedicated Local Runtime Manager.

The user should not need to manually configure inference engines, model paths, ports, or complicated runtime settings.

The application should manage this where appropriate.

Capabilities:

1. Discover existing inference engines.
2. Discover existing model servers.
3. Discover available local models.
4. Identify supported model formats.
5. Recommend compatible models.
6. Download models.
7. Select suitable quantizations.
8. Configure inference engines.
9. Start inference services.
10. Stop inference services.
11. Monitor runtime health.
12. Monitor CPU, GPU, RAM, and VRAM usage.
13. Report performance.
14. Update managed models.
15. Remove managed models.
16. Manage runtime configurations.

## Container strategy

Support rootless Podman as an important deployment option.

Also support native inference installations.

Do not make containers mandatory.

Do not automatically replace existing user-managed containers.

Do not modify an existing Ollama installation without explicit user permission.

Distinguish clearly between:

- User-managed runtimes.
- Application-managed runtimes.

A shared model service should be reusable across multiple projects and sessions.

Do not launch duplicate inference services unnecessarily.

Support isolated configurations when genuinely needed.

The user should be able to choose whether to manage a runtime automatically or use an existing installation.

---

# 8. REAL CODING AGENT ENGINE

This is a fundamental product requirement.

A language model does not automatically have access to a user's filesystem, projects, terminal, Git repositories, or IDE.

The application must supply those capabilities.

The model is the reasoning engine.

Our Rust application provides the environment, tools, execution, and permissions.

Build a real agent execution engine.

## Native tools

Support:

- Workspace discovery.
- Project inspection.
- Filesystem browsing.
- File reading.
- File writing.
- File creation.
- File deletion with appropriate approval.
- Source-code search.
- Code navigation.
- Git status.
- Git history.
- Git diffs.
- Patch application.
- Terminal execution.
- Build commands.
- Test execution.
- Error inspection.
- Project documentation.
- Project instructions.
- Task tracking.

The agent should understand project structure and retrieve relevant information instead of indiscriminately loading entire repositories.

## Agent execution loop

Implement a reliable execution loop:

1. Receive the user's request.
2. Build the relevant context.
3. Send a request to the selected model.
4. Parse the response.
5. Identify requested tool calls.
6. Validate tool calls.
7. Check permissions.
8. Execute approved tools.
9. Return structured results.
10. Continue the model interaction.
11. Detect completion or failure.
12. Preserve task state.

Handle malformed tool calls, streaming interruptions, retries, timeouts, and cancellation.

Do not assume every model uses the same tool-call representation.

## Permissions

Implement configurable permissions for:

- Workspace access.
- File modifications.
- File deletion.
- Shell execution.
- Destructive commands.
- External workspace access.
- Git operations.
- MCP tools.
- Other potentially dangerous actions.

Provide clear approval prompts.

Maintain an execution history.

Allow the user to interrupt tasks immediately.

Never give the model unrestricted direct host access.

---

# 9. MCP, SKILLS, AND EXTENSIBILITY

Support a flexible extension architecture.

Include:

- MCP servers.
- MCP tools.
- MCP resources.
- Skills.
- Agent profiles.
- Hooks.
- Custom tools.
- External integrations.

Users should be able to add MCP servers without modifying Rust source code.

Support different transports as appropriate.

Discover and cache MCP metadata.

Do not inject every MCP tool schema into every model request.

Use selective discovery and lazy loading.

The extension architecture must be secure, observable, and easy to configure.

Provide a management interface within the TUI.

---

# 10. JETBRAINS AND IDE INTEGRATION

Deep IDE integration is a major priority.

Initial targets:

- JetBrains RustRover.
- JetBrains PyCharm.
- IntelliJ IDEA.
- Other JetBrains IDEs where practical.

We frequently work in RustRover and PyCharm.

The application should integrate naturally into existing development workflows.

## Investigate existing integration mechanisms

Evaluate:

- JetBrains-provided MCP servers.
- JetBrains plugin APIs.
- Language Server Protocol.
- IDE terminal integration.
- Filesystem integration.
- Git integration.
- Agent Client Protocol where appropriate.

Do not assume every IDE exposes identical MCP capabilities.

Discover what each IDE supports.

## JetBrains integration goals

Eventually support:

- Detecting the active IDE and project.
- Identifying the current workspace.
- Accessing relevant project metadata.
- Reading selected editor text where authorized.
- Accessing diagnostics.
- Running project tests.
- Opening relevant files.
- Navigating to source locations.
- Reviewing agent-generated changes.
- Communicating with the running Rust application.

Consider building a lightweight JetBrains plugin that communicates with the Rust application.

Do not make the plugin responsible for duplicating the agent engine.

The Rust application remains the source of truth for:

- Sessions.
- Providers.
- Models.
- Context.
- Memory.
- Tool execution.
- Permissions.
- Runtime management.

## IPC architecture

Investigate a secure local IPC mechanism for communication between the TUI application and IDE integrations.

Consider Unix domain sockets on Linux, with an appropriate Windows implementation later.

Use authentication or access controls appropriate for local IPC.

Do not expose an unnecessary unauthenticated network service.

---

# 11. CONTEXT ENGINE AND TOKEN EFFICIENCY

Context management must be a first-class subsystem.

We have experienced severe context inflation and unexpected token consumption in existing coding-agent software.

We must prevent that.

One previous problem involved requests exceeding 100,000 tokens even in an essentially empty project.

Do not assume MCP alone caused this.

Investigate all possible sources of prompt inflation.

## Measure every component

Track:

- System instructions.
- Developer instructions.
- Project instructions.
- Conversation history.
- Tool definitions.
- MCP schemas.
- Retrieved files.
- Project memory.
- Tool results.
- Agent state.
- Provider-specific metadata.
- Any hidden or generated prompt components under our control.

The application must be able to explain what it sends to a model.

## Context management requirements

Implement:

- Token counting.
- Model-specific tokenizers where available.
- Accurate estimation where exact tokenization is unavailable.
- Provider-reported usage tracking.
- Context budgeting.
- Selective history retrieval.
- Project memory.
- Task-state preservation.
- Tool-result storage.
- Context compaction.
- Session recovery.
- Context inspection.

Never present estimated token counts as exact provider-reported usage.

Distinguish estimates from actual usage.

## Context efficiency

Do not send unnecessary information to the model.

Avoid loading entire projects when only a few files are relevant.

Avoid sending every available MCP tool schema.

Avoid repeating unchanged tool results.

Avoid duplicating project instructions.

Use retrieval and task-specific context.

## Compaction

Compaction must:

- Preserve the current objective.
- Preserve important decisions.
- Preserve relevant findings.
- Preserve unfinished work.
- Preserve important file references.
- Preserve tool execution state where appropriate.
- Record what was retained, summarized, and discarded.
- Allow inspection.

Support continuation before context overflow rather than waiting until the model fails.

Context management should adapt to each model's actual capabilities and available resources.

---

# 12. PROJECT MEMORY AND SESSION MANAGEMENT

Support persistent project intelligence.

The application should maintain:

- Session history.
- Project-specific memory.
- Architecture notes.
- Important implementation decisions.
- Project instructions.
- Known issues.
- Task state.
- Relevant tool results.
- User-approved preferences.

Memory should be searchable and inspectable.

Do not blindly inject all stored memories into every request.

Retrieve only relevant information.

Users should be able to inspect, edit, and control project memory.

Support multiple projects and simultaneous sessions without mixing their contexts or permissions.

Persist sessions so work can resume after application restarts or interruptions.

Use SQLite or another appropriate persistence strategy.

---

# 13. SECURITY

Security must be designed from the beginning.

Important requirements:

- Protect API keys.
- Avoid logging credentials.
- Use appropriate operating-system credential storage where available.
- Restrict workspace access.
- Validate tool calls.
- Require approvals for dangerous operations.
- Control shell execution.
- Secure IPC.
- Protect managed runtime configurations.
- Validate downloaded artifacts where practical.
- Handle untrusted MCP servers carefully.
- Avoid privilege escalation.
- Never execute arbitrary downloaded scripts silently.

The application must distinguish trusted tools from untrusted external integrations.

Provide meaningful user-facing security explanations.

---

# 14. PRODUCT CONFIGURATION AND ONBOARDING

First-run onboarding should be excellent.

The application should:

1. Detect system hardware.
2. Discover existing local inference services.
3. Discover available models.
4. Identify available runtimes.
5. Offer cloud-provider authentication.
6. Recommend suitable models.
7. Help configure a workspace.
8. Explain permissions.
9. Start a working coding session.

A beginner should not need to understand:

- Quantization.
- KV cache.
- VRAM.
- Context windows.
- Inference endpoints.
- Container configuration.
- Model formats.

Advanced users must retain control over those settings.

Provide simple defaults and advanced configuration modes.

---

# 15. OBSERVABILITY AND DIAGNOSTICS

Provide useful diagnostics throughout the application.

Include:

- Structured logs.
- Debug mode.
- Provider request diagnostics.
- Runtime health.
- Model loading errors.
- Tool execution history.
- Context breakdown.
- Token usage.
- Memory usage.
- GPU utilization where available.
- Performance measurements.

Make failures understandable.

Do not swallow important errors.

Do not silently fall back to another provider or model without informing the user.

Provide diagnostic reports that can help developers troubleshoot problems.

---

# 16. TESTING AND QUALITY

Reliability is more important than feature count.

Implement automated tests for each subsystem.

Create provider compatibility tests covering:

- Streaming.
- Tool calls.
- Multi-step tool execution.
- Malformed tool calls.
- Continuation.
- Context limits.
- Truncated responses.
- Structured output.
- Cancellation.
- Model switching.

Test local inference engines independently.

Test AMD, NVIDIA, Intel, and CPU-only configurations where hardware is available.

Use mocks and recorded fixtures for providers where live testing is impractical.

Do not claim a provider or runtime is supported until its integration has been tested.

Do not use fake placeholder functionality that appears operational.

Do not remove functionality merely to make tests pass.

Do not rewrite working components without a clear reason.

Use Rust formatting, linting, and appropriate static analysis.

Maintain documentation alongside implementation.

---

# 17. INITIAL DEVELOPMENT MACHINE

Our primary development and testing environment:

- Fedora Linux KDE Plasma.
- AMD Ryzen 9 5950X.
- 96 GB system RAM.
- PowerColor Red Devil Ultimate OC Radeon RX 6900 XT.
- 16 GB dedicated VRAM.
- Podman.
- Existing Ollama container.
- Existing Ollama API at `http://localhost:11434`.

The existing Ollama container is named `ollama`.

The application must discover and connect to it without replacing, recreating, or modifying it.

We have several existing local models available for testing.

Use the machine to validate hardware detection, AMD compatibility, model recommendations, runtime integration, and agent functionality.

However, do not build an application that only works on this hardware.

We want broad hardware support.

---

# 18. PROPOSED INITIAL PROJECT STRUCTURE

Consider a Cargo workspace with boundaries similar to:

```text
workbench/
├── Cargo.toml
├── crates/
│   ├── app/
│   ├── tui/
│   ├── core/
│   ├── agent/
│   ├── providers/
│   ├── inference/
│   ├── hardware/
│   ├── model-manager/
│   ├── runtime-manager/
│   ├── context/
│   ├── memory/
│   ├── tools/
│   ├── mcp/
│   ├── ide/
│   ├── lsp/
│   ├── permissions/
│   ├── storage/
│   └── platform/
├── integrations/
│   └── jetbrains/
├── docs/
├── packaging/
│   └── rpm/
└── README.md
```

This is a proposed structure, not a mandate.

Challenge it if a simpler or more maintainable design is better.

Do not create empty crates simply to fill out this tree.

Only introduce modules when their boundaries and responsibilities are understood.

---

# 19. DEVELOPMENT ROADMAP

Develop the application incrementally.

Suggested milestones:

## Milestone 0: Architecture and foundations

- Finalize product requirements.
- Design architecture.
- Establish Cargo workspace.
- Define interfaces.
- Establish testing strategy.
- Establish logging.
- Establish configuration and storage.

## Milestone 1: Working terminal application

- Excellent initial TUI.
- Project/workspace selection.
- Session management.
- Ollama integration.
- Model selection.
- Streaming conversation.
- Persistent history.

The application must actually work.

## Milestone 2: Coding-agent engine

- Filesystem tools.
- Project discovery.
- Search.
- Terminal execution.
- Git integration.
- Tool calling.
- Permission system.
- Diff review.

At this milestone, it must be capable of performing real development tasks.

## Milestone 3: Context and memory

- Context budgeting.
- Token tracking.
- Selective retrieval.
- Project memory.
- Task state.
- Compaction.
- Session recovery.

## Milestone 4: Hardware and local runtime management

- Hardware discovery.
- GPU detection.
- Model recommendations.
- Runtime discovery.
- Model downloads.
- Managed Podman/native inference.
- Resource monitoring.
- Benchmarking.

## Milestone 5: Universal providers

- Anthropic.
- OpenAI.
- Gemini.
- Other providers.
- API keys.
- Supported authentication.
- Custom endpoints.
- Capability negotiation.

## Milestone 6: MCP, skills, and extensibility

- MCP server management.
- Lazy tool discovery.
- Skills.
- Agent profiles.
- Hooks.
- Custom tools.

## Milestone 7: IDE integration

- JetBrains discovery.
- Existing IDE MCP support.
- IPC.
- JetBrains plugin.
- RustRover and PyCharm workflows.

## Milestone 8: Distribution and polish

- Fedora RPM.
- Upgrade and removal.
- Installation documentation.
- Shell completion.
- Linux compatibility.
- Performance optimization.
- Security review.
- Release preparation.

The roadmap may be adjusted based on architectural findings.

---

# 20. WORKING WITH THE DEVELOPER

The developer is an experienced Linux user who works with Rust, Python, PyCharm, RustRover, Podman, Ollama, and local AI models.

They value technical accuracy, detailed explanations, reliability, and excellent UX.

They are comfortable with advanced configuration but want the finished product to be accessible to beginners.

Important working rules:

- Do not assume existing containers can be replaced.
- Do not make destructive changes without permission.
- Do not assume hardware capabilities.
- Do not invent benchmark results.
- Do not assume all models support reliable tool calling.
- Do not overcomplicate the architecture.
- Do not implement features without testing them.
- Explain significant architectural decisions.
- Explain trade-offs honestly.
- Preserve existing working functionality.
- Prefer complete, maintainable implementations over quick hacks.

When providing terminal commands, make them appropriate for Fedora Linux.

When changing project files, inspect the existing structure first.

When debugging, investigate the actual cause instead of repeatedly applying speculative fixes.

---

# 21. YOUR FIRST ASSIGNMENT

Do not immediately start writing large amounts of application code.

First, analyze the entire specification.

Produce:

1. Your understanding of the product.
2. The defining product features.
3. The proposed Rust architecture.
4. A recommended Cargo workspace structure.
5. Library and dependency recommendations.
6. TUI design and interaction strategy.
7. Provider architecture.
8. Hardware detection strategy.
9. Local runtime management strategy.
10. Agent execution architecture.
11. MCP and IDE integration strategy.
12. Context and memory architecture.
13. Security model.
14. Fedora RPM packaging strategy.
15. Testing and compatibility strategy.
16. Development roadmap.
17. Major technical risks.
18. Important unanswered architectural questions.

Create the initial documentation:

- `README.md`
- `docs/PRODUCT_SPEC.md`
- `docs/ARCHITECTURE.md`
- `docs/TUI_DESIGN.md`
- `docs/PROVIDER_ARCHITECTURE.md`
- `docs/HARDWARE_AND_RUNTIME.md`
- `docs/AGENT_ENGINE.md`
- `docs/CONTEXT_ENGINE.md`
- `docs/IDE_INTEGRATION.md`
- `docs/SECURITY.md`
- `docs/ROADMAP.md`

Be critical.

Challenge assumptions that could lead to poor architecture.

Identify missing requirements.

Do not silently make major architectural decisions.

Once the architecture is agreed upon, begin implementing the first milestone.

Build working software incrementally.

Every milestone should produce something demonstrably functional.

---

# FINAL PRODUCT GOAL

We want to build an exceptionally good terminal-native AI development environment.

It should feel as polished as the best modern developer tools.

It should support virtually any useful model, whether running locally or in the cloud.

It should intelligently adapt to the user's hardware.

It should provide the actual tools and execution environment required for autonomous coding agents.

It should integrate naturally with JetBrains IDEs and other developer environments.

It should manage context intelligently instead of wasting enormous amounts of tokens.

It should be powerful, reliable, extensible, and secure.

It should make advanced AI development accessible to everyone.

**We are not building a wrapper around existing AI tools. We are building the development environment that brings them together.**