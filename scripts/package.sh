#!/usr/bin/env bash
# Builds the release binary, then the Fedora .rpm and Debian .deb into dist/.
# Needs cargo-generate-rpm and cargo-deb (`cargo install cargo-generate-rpm cargo-deb`).
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --release
mkdir -p dist
rm -f dist/*.rpm dist/*.deb
cargo generate-rpm -p crates/app -o dist/
cargo deb -p re-code --no-build -o dist/
sha256sum dist/*.rpm dist/*.deb > dist/SHA256SUMS
ls -la dist/
