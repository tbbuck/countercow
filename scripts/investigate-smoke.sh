#!/bin/bash
# Verify the investigation round trip against a live process: attach, press 'i' to open the
# runtime session, wait for events, press 'i' again to close it, then 'q'.
#
# The point is the session lifecycle — that a second EventPipe session opens on demand and is
# closed again — which the render tests cannot cover.
#
#   scripts/investigate-smoke.sh <pid>
set -u

pid="${1:?usage: investigate-smoke.sh <pid>}"
capture="${2:-/tmp/countercow-investigate-smoke.txt}"

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
bin="$root/target/debug/countercow"

if [ ! -x "$bin" ]; then
    echo "build first: cargo build" >&2
    exit 1
fi

( sleep 3; printf 'i'; sleep 5; printf 'i'; sleep 2; printf 'q' ) \
    | script -q "$capture" "$bin" --pid "$pid" >/dev/null 2>&1
status=$?

echo "exit status: $status"

if grep -q $'\033\[?1049l' "$capture" && grep -q $'\033\[?25h' "$capture"; then
    echo "terminal restored: yes"
else
    echo "terminal restored: NO" >&2
    exit 1
fi

# The runtime session must not outlive the screen: a leaked one would keep costing the target
# process CPU after countercow has exited. The target always holds its own listening socket, so
# one entry is the expected baseline and anything more is a leak.
open=$(lsof -p "$pid" 2>/dev/null | grep -c 'dotnet-diagnostic' || true)
echo "diagnostic sockets on target: $open (1 = the runtime's own listener)"
if [ "$open" -gt 1 ]; then
    echo "a session was leaked" >&2
    exit 1
fi

exit $status
