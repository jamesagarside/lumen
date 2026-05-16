#!/usr/bin/env bash
# Run the daemon and the UI dev server together.
# Output from each is prefixed; Ctrl-C kills both. Either crashing
# kills the other.

set -uo pipefail
cd "$(dirname "$0")/.."

cleanup() {
    local code=$?
    echo
    echo "[dev] stopping (exit=$code)..."
    # Kill the whole process group so child processes (cargo, vite,
    # node, etc.) die too — `kill 0` targets the group leader.
    kill 0 2>/dev/null || true
    wait 2>/dev/null || true
    exit $code
}
trap cleanup INT TERM EXIT

# Daemon: prefer cargo-watch when available for auto-reload.
if command -v cargo-watch >/dev/null 2>&1; then
    DAEMON_CMD="cargo watch -q -x 'run --bin lumen'"
else
    DAEMON_CMD="cargo run --bin lumen"
fi

echo "[dev] daemon : http://localhost:3000  (NetFlow v5 on UDP :2055)"
echo "[dev] ui     : http://localhost:5173"
echo "[dev] press Ctrl-C to stop both"
echo

# Run each in its own subshell so we can prefix output without
# affecting the other. `2>&1` interleaves stderr; `sed -u` keeps
# line-buffering so prefixes appear promptly.
(
    eval "$DAEMON_CMD" 2>&1 | sed -u 's/^/[daemon] /'
    # If daemon exits, signal the parent group so the UI dies too.
    kill -TERM 0
) &

(
    cd ui && npm run dev 2>&1 | sed -u 's/^/[ui]     /'
    kill -TERM 0
) &

wait
