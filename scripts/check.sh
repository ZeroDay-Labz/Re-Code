#!/usr/bin/env bash
# The quality gates. Live tests against a real Ollama are separate:
#   cargo test -p recode-providers -- --ignored
set -euo pipefail
cd "$(dirname "$0")/.."
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
if command -v cargo-deny >/dev/null; then
    cargo deny check
else
    echo "note: cargo-deny isn't installed, so licences and advisories weren't checked" >&2
fi
