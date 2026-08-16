#!/bin/bash
# Drive traffic at the sample ASP.NET app so its request and GC counters move.
#
#   scripts/drive-load.sh [base-url] [seconds]
set -u

base="${1:-http://localhost:5199}"
seconds="${2:-15}"

deadline=$(( SECONDS + seconds ))
requests=0

while [ $SECONDS -lt $deadline ]; do
    curl -s -o /dev/null "$base/" || true
    curl -s -o /dev/null "$base/alloc?mb=4" || true
    curl -s -o /dev/null "$base/throw?count=25" || true
    curl -s -o /dev/null "$base/work?items=100" || true
    curl -s -o /dev/null "$base/fail" || true
    requests=$(( requests + 5 ))
done

echo "sent $requests requests over ${seconds}s"
