#!/bin/bash
# Verify the CPU profile screen end to end in the real TUI: attach, press 'c' to profile, wait
# for the window plus resolution, then leave and quit.
#
#   scripts/profile-smoke.sh <pid> [url]
set -u

pid="${1:?usage: profile-smoke.sh <pid> [url]}"
url="${2:-http://localhost:5199}"
capture="${3:-/tmp/countercow-profile-smoke.txt}"

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
bin="$root/target/debug/countercow"

if [ ! -x "$bin" ]; then
    echo "build first: cargo build" >&2
    exit 1
fi

# Keep the target busy in managed code for the whole run, so the profile has something to find.
"$root/scripts/drive-cpu.sh" "$url" 22 4 >/dev/null 2>&1 &
load_pid=$!
sleep 2

# 'c' profiles (5s window + resolve), then Esc back to the dashboard, then 'q'.
( sleep 2; printf 'c'; sleep 12; printf '\033'; sleep 2; printf 'q' ) \
    | script -q "$capture" "$bin" --pid "$pid" >/dev/null 2>&1
status=$?

wait $load_pid 2>/dev/null

echo "exit status: $status"

if grep -q $'\033\[?1049l' "$capture" && grep -q $'\033\[?25h' "$capture"; then
    echo "terminal restored: yes"
else
    echo "terminal restored: NO" >&2
    exit 1
fi

open=$(lsof -p "$pid" 2>/dev/null | grep -c 'dotnet-diagnostic' || true)
echo "diagnostic sockets on target: $open (1 = the runtime's own listener)"
if [ "$open" -gt 1 ]; then
    echo "a session was leaked" >&2
    exit 1
fi

exit $status
