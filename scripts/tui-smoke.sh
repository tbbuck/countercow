#!/bin/bash
# Smoke-test the TUI against a live .NET process under a pseudo-terminal.
#
# Attaches to $1 for a few seconds, sends 'q', and reports the exit status. The point is the
# plumbing the layout tests cannot cover: that the session starts, the threads shut down, and
# the terminal is restored rather than left in raw mode.
#
#   scripts/tui-smoke.sh <pid> [capture-file]
#
# Note: under a pty with no size (CI, a non-interactive shell) ratatui draws empty frames, so
# this checks behaviour, not appearance. Use `cargo run --example preview` for the visuals.
set -u

pid="${1:?usage: tui-smoke.sh <pid> [capture-file]}"
capture="${2:-/tmp/countercow-tui-smoke.txt}"

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
bin="$root/target/debug/countercow"

if [ ! -x "$bin" ]; then
    echo "build first: cargo build" >&2
    exit 1
fi

( sleep 5; printf 'q' ) | script -q "$capture" "$bin" --pid "$pid" >/dev/null 2>&1
status=$?

echo "exit status: $status"

# Leaving the alternate screen and restoring the cursor are the markers that matter: without
# them the user's terminal is left unusable.
if grep -q $'\033\[?1049l' "$capture" && grep -q $'\033\[?25h' "$capture"; then
    echo "terminal restored: yes"
else
    echo "terminal restored: NO — check the panic hook" >&2
    exit 1
fi

exit $status
