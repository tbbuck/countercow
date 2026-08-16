#!/bin/bash
# Verify the detach round trip: attach to $1, press 'd' to return to the picker, then 'q' twice
# to leave the picker and exit.
#
#   scripts/detach-smoke.sh <pid>
set -u

pid="${1:?usage: detach-smoke.sh <pid>}"
capture="${2:-/tmp/countercow-detach-smoke.txt}"

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
bin="$root/target/debug/countercow"

if [ ! -x "$bin" ]; then
    echo "build first: cargo build" >&2
    exit 1
fi

# 'd' leaves the dashboard for the picker; 'q' then leaves the picker.
( sleep 4; printf 'd'; sleep 3; printf 'q' ) | script -q "$capture" "$bin" --pid "$pid" >/dev/null 2>&1
status=$?

echo "exit status: $status"

if grep -q $'\033\[?1049l' "$capture" && grep -q $'\033\[?25h' "$capture"; then
    echo "terminal restored: yes"
else
    echo "terminal restored: NO" >&2
    exit 1
fi

# The alternate screen should be entered once and left once: the terminal must not be torn down
# and rebuilt when moving between screens.
enters=$(grep -o $'\033\[?1049h' "$capture" | wc -l | tr -d ' ')
echo "alternate screen entered: $enters time(s)"
if [ "$enters" != "1" ]; then
    echo "expected exactly 1 — detach should reuse the terminal, not flicker" >&2
    exit 1
fi

exit $status
