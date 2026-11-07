# Contributing to re-code

Thanks for your interest. re-code is a terminal-native coding agent for Fedora Linux that puts local
models first. The working rules in `CLAUDE.md` apply to every change, by anyone:

- Never replace, recreate or reconfigure a user's existing containers or Ollama. Use their APIs only, and
  ask before starting anything.
- No placeholder functionality that appears to work. Don't claim a provider, runtime or model feature is
  supported until it has been tested for real; label estimates as estimates.
- Don't silently fall back to another provider or model.
- The interface must fit and look good at any terminal size (`docs/TUI_DESIGN.md`).
- Explain significant architectural decisions and trade-offs honestly, in `docs/ARCHITECTURE.md`.

## Building and checking

A stable Rust toolchain (1.90 or newer) is all that's needed:

```bash
cargo run                    # the app, needs Ollama
cargo run -- demo            # the interface with a recording, no model needed
cargo run -- doctor          # what re-code sees on this machine
./scripts/check.sh           # formatting, clippy (warnings are errors), tests, cargo-deny
```

`./scripts/check.sh` must pass before a change is proposed. Tests that reach a real Ollama, a network
service or an API key are `#[ignore]`d and run with `cargo test -- --ignored`; they must not be required
for the default suite.

## Making changes

- Keep commits focused and describe *why* in the message. Plain messages, no trailers.
- Interface changes need a scene in `crates/tui/tests/any_size.rs` so the any-size sweep covers them, and
  snapshot changes reviewed, not just accepted.
- Anything that touches security (sandbox, permissions, credentials, project trust, installs) updates
  `docs/SECURITY.md` in the same change.
- New dependencies: prefer well-maintained crates with permissive licences; `cargo deny` enforces the
  allow-list in `deny.toml`.
- Terminal commands in docs and code are for Fedora Linux.

## Reporting problems

Open an issue with the output of `re-code doctor` (it contains no secrets or prompts), your terminal, and
what you expected. For security problems see `docs/SECURITY.md`.

## Licence

By contributing you agree that your contributions are licensed under the Apache License, Version 2.0, the
project's licence (see `LICENSE`).
