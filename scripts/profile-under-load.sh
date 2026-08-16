#!/bin/bash
# Profile a process while load is definitely running.
#
# Starting the load in one background call and profiling in another risks the load finishing
# before the profile begins (a cargo rebuild in between is enough). This starts the load, waits
# for it to warm up, then profiles inside the load window.
#
#   scripts/profile-under-load.sh <pid> [profile-seconds] [url]
set -u

pid="${1:?usage: profile-under-load.sh <pid> [seconds] [url] [cpu|mixed]}"
seconds="${2:-5}"
url="${3:-http://localhost:5199}"
kind="${4:-mixed}"

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# `cpu` drives managed compute, which is what a hot-method list should attribute; `mixed` drives
# the allocation-heavy endpoints, whose real work happens in the native GC.
if [ "$kind" = "cpu" ]; then
    driver="$root/scripts/drive-cpu.sh"
else
    driver="$root/scripts/drive-load.sh"
fi

# Load runs comfortably longer than the profile at both ends.
"$driver" "$url" $(( seconds + 8 )) >/dev/null 2>&1 &
load_pid=$!

sleep 2
"$root/target/debug/examples/profile_cli" "$pid" "$seconds"
status=$?

wait $load_pid 2>/dev/null
exit $status
