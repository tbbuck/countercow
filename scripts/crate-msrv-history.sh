#!/bin/bash
# List a crate's recent versions and the rust-version each declares, newest first.
#
# Useful for answering "how far back can I go and still get MSRV X" without guessing.
#
#   scripts/crate-msrv-history.sh <crate> [count]
set -u

crate="${1:?usage: crate-msrv-history.sh <crate> [count]}"
count="${2:-25}"

curl -s "https://crates.io/api/v1/crates/$crate/versions" \
    -H "User-Agent: countercow-msrv-check" \
    | jq -r --argjson n "$count" '
        [.versions[] | select(.yanked == false)][:$n][]
        | "\(.num)\t\(.rust_version // "none")"
      '
