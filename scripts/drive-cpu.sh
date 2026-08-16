#!/bin/bash
# Drive pure managed CPU work at the sample app, for testing the profiler.
#
# Unlike drive-load.sh (which mostly exercises allocation and the GC — native work), this keeps
# the process busy inside managed code, which is what a hot-method list should attribute.
#
#   scripts/drive-cpu.sh [base-url] [seconds] [concurrency]
set -u

base="${1:-http://localhost:5199}"
seconds="${2:-15}"
concurrency="${3:-4}"

deadline=$(( SECONDS + seconds ))

for _ in $(seq 1 "$concurrency"); do
    (
        while [ $SECONDS -lt $deadline ]; do
            curl -s -o /dev/null "$base/compute?iterations=8000000" || true
        done
    ) &
done

wait
echo "drove $concurrency concurrent compute loops for ${seconds}s"
