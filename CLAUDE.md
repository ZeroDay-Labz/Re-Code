# re-code

A Rust, terminal-native AI coding agent for Fedora Linux, local models first. The full requirements are in
`docs/MASTER_SPEC.md`. Read the relevant section when working on that area, rather than loading all of it.

Start with `docs/ROADMAP.md` for current status and `docs/ARCHITECTURE.md` for structure and decisions.

## Working rules

- Never replace, recreate or reconfigure the developer's existing containers or Ollama. The `local-ai` pod is
  theirs. Use its API only, and ask before starting or stopping it.
- Don't make destructive changes without permission.
- Don't assume hardware capabilities, and don't invent benchmark results.
- Don't assume models support reliable tool calling. Test it.
- Don't claim a provider or runtime is supported until its integration has been tested.
- No placeholder functionality that appears to work.
- Don't silently fall back to another provider or model.
- Never present estimated token counts as provider-reported usage.
- Explain significant architectural decisions and trade-offs honestly.
- Debug by finding the actual cause, not by repeating speculative fixes.
- Terminal commands must suit Fedora Linux.
- The TUI must fit and look good at any terminal size (`docs/TUI_DESIGN.md`).

## Checks

`./scripts/check.sh` runs formatting, clippy, tests and `cargo deny`.
`cargo test -p recode-providers -- --ignored` runs live tests against a real Ollama.
