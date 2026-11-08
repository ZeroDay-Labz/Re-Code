#!/usr/bin/env bash
# Drives re-code in tmux with an isolated configuration, captures each screen with its colours
# (tmux capture-pane -e) into scripts/screenshots/*.ans, and renders them to assets/screenshots/*.png
# with render.py. Needs tmux, Pillow, a running Ollama with the model named below, and a GitHub
# connection for the install previews.
#
#   scripts/screenshots/capture.sh [model]
set -euo pipefail
cd "$(dirname "$0")/../.."
MODEL="${1:-gpt-oss:20b}"
BIN="$PWD/target/debug/re-code"
OUT="$PWD/scripts/screenshots"
PNG="$PWD/assets/screenshots"
WORK="${TMPDIR:-/tmp}/re-code-shots"
rm -rf "$WORK"
mkdir -p "$PNG" "$WORK/xdg/config/re-code" "$WORK/proj/src"
cargo build -q
# A small real project for the agent to work in.
cat > "$WORK/proj/Cargo.toml" <<'T'
[package]
name = "demo"
version = "0.1.0"
edition = "2024"
T
cat > "$WORK/proj/src/main.rs" <<'T'
fn main() {
    let retries = 3;
    for attempt in 0..retries {
        println!("attempt {attempt}");
    }
}
T
cat > "$WORK/run.sh" <<T
#!/bin/sh
export XDG_CONFIG_HOME=$WORK/xdg/config XDG_DATA_HOME=$WORK/xdg/data XDG_CACHE_HOME=$WORK/xdg/cache XDG_STATE_HOME=$WORK/xdg/state
cd $WORK/proj
exec $BIN "\$@"
T
chmod +x "$WORK/run.sh"
S=shots
tmux kill-session -t $S 2>/dev/null || true
tmux new-session -d -s $S -x 132 -y 38 "$WORK/run.sh"
shot() { sleep "${2:-0.6}"; tmux capture-pane -e -p -t $S > "$OUT/$1.ans"; echo "captured $1"; }
key() { tmux send-keys -t $S "$@"; }
type() { tmux send-keys -t $S -l "$1"; }

sleep 4
# The home screen opens on Home when sessions exist, else on Models: find tabs by their panel title.
goto() {
    for _ in $(seq 1 10); do
        if tmux capture-pane -p -t $S | grep -qE "$1"; then return 0; fi
        key Tab; sleep 0.25
    done
    echo "couldn't reach $1" >&2
}
goto "╭ Home "; shot home 0.3
goto "Ollama .* of .* models|╭ Models"; shot models
goto "Runtimes and containers"; shot runtimes
goto "╭ MCP servers"; shot mcp
goto "╭ Skills "; shot skills
goto "Plugins and marketplaces"; shot plugins
goto "╭ Settings"; shot settings
# Waits until the screen shows a pattern (or gives up after N seconds).
until_shown() {
    for _ in $(seq 1 "${2:-30}"); do
        if tmux capture-pane -p -t $S | grep -qE "$1"; then return 0; fi
        sleep 1
    done
    echo "never saw: $1" >&2
}
# Install a skill from GitHub: the Hermes-style preview.
goto "╭ Skills "
key i; type "anthropics/skills/skills/pdf"; key Enter; until_shown "Enable now\?" 40; shot skill_install 0.3
key n; sleep 1
# Follow Anthropic's marketplace and install a plugin from it.
goto "Plugins and marketplaces"
key m; type "anthropics/claude-plugins-official"; key Enter; until_shown "Following claude-plugins-official" 40; sleep 0.5
key i; type "context7@claude-plugins-official"; key Enter; until_shown "Enable now\?" 40; shot plugin_install 0.3
key y; until_shown "Installed plugin context7" 20; shot plugins 0.5
# A conversation: pick the model and ask for a real edit.
goto "Ollama .* of .* models|╭ Models"
type "$MODEL"; sleep 0.3; key Enter; sleep 3
type "Read src/main.rs and add a doc comment to main explaining the retry loop. Keep it short."; key Enter
until_shown "y approve" 120; shot approval 0.3
key y
until_shown "── .* tools? · .* out ──" 150; shot conversation 0.5
# The context ledger, the command palette and the theme picker.
type "/context"; sleep 0.3; key Escape; sleep 0.3; key Enter; until_shown "Reserved for the reply" 10; shot context 0.3
key Escape; sleep 1.5
key C-k; until_shown "Copy the last code block" 10; shot palette 0.3
key Escape; sleep 1.5
type "/theme"; sleep 0.3; key Escape; sleep 0.3; key Enter; until_shown "Catppuccin" 10; key Down; sleep 0.4; key Down; sleep 0.8; shot themes 0.3
key Escape; sleep 1
tmux kill-session -t $S
for ans in "$OUT"/*.ans; do
    name=$(basename "$ans" .ans)
    python3 "$OUT/render.py" "$PNG/$name.png" "$ans" --title "re-code · $name"
done
ls -la "$PNG"
