#!/bin/bash
# Smoke-test changing the refresh rate against a live .NET process.
#
# Each `-` or `+` closes the counter session and opens another, which the layout tests cannot
# cover: they never speak to a runtime. What matters here is that the restarts actually succeed,
# and that the dashboard ends up running at the rate that was asked for.
#
# Unlike the other smoke scripts this one sizes the pty, because the thing being checked is what
# reached the screen: at 0x0 ratatui draws empty frames and every assertion below would pass
# vacuously.
#
#   scripts/rate-smoke.sh <pid> [capture-file]
set -u

pid="${1:?usage: rate-smoke.sh <pid> [capture-file]}"
capture="${2:-/tmp/countercow-rate-smoke.txt}"

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
bin="$root/target/debug/countercow"

if [ ! -x "$bin" ]; then
    echo "build first: cargo build" >&2
    exit 1
fi

# Down a rung, down again, then back up: three restarts, from the 1s default.
(
    sleep 4
    printf -- '-'
    sleep 2
    printf -- '-'
    sleep 2
    printf '+'
    sleep 2
    printf 'q'
) | script -q "$capture" /bin/sh -c "stty rows 40 cols 120; exec '$bin' --pid '$pid'" >/dev/null 2>&1
status=$?

echo "exit status: $status"

if grep -q $'\033\[?1049l' "$capture" && grep -q $'\033\[?25h' "$capture"; then
    echo "terminal restored: yes"
else
    echo "terminal restored: NO — check the panic hook" >&2
    exit 1
fi

# A restart that fails puts its reason in the footer and leaves the old rate running. Without
# this the script would pass on a build that had quietly stopped changing anything.
if strings "$capture" | grep -qE 'could not (restart counters|close the counter session)'; then
    echo "rate change: FAILED — see $capture" >&2
    exit 1
fi

# The footer names the live rate. ratatui repaints only the cells that changed, so match from the
# first character that differs from the 1s it started at rather than expecting the whole hint.
if ! strings "$capture" | grep -q '0\.5s'; then
    echo "rate change: never reached 0.5s — see $capture" >&2
    exit 1
fi
if ! strings "$capture" | grep -q '25s'; then
    echo "rate change: never reached 0.25s — see $capture" >&2
    exit 1
fi
echo "rate change: 1s -> 0.5s -> 0.25s observed, no session errors"

exit $status
