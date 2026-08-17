#!/bin/bash
# Report the minimum supported Rust version implied by the dependency tree.
#
# Cargo picks the newest semver-compatible version of every crate, and those bring their own
# `rust-version` floors — which is usually what pushes an MSRV up, not the project's own code.
#
#   scripts/msrv-report.sh [top-n]
set -u

top="${1:-15}"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

echo "Dependencies declaring the highest rust-version:"
cargo metadata --manifest-path "$root/Cargo.toml" --format-version 1 --locked \
    | jq -r '.packages[] | select(.rust_version != null) | "\(.rust_version)\t\(.name) \(.version)"' \
    | sort -Vr \
    | head -n "$top"

echo
echo "Highest floor in the tree:"
cargo metadata --manifest-path "$root/Cargo.toml" --format-version 1 --locked \
    | jq -r '[.packages[] | select(.rust_version != null) | .rust_version] | max'

echo
echo "Dependencies declaring no rust-version at all (unconstrained, but unverified):"
cargo metadata --manifest-path "$root/Cargo.toml" --format-version 1 --locked \
    | jq -r '[.packages[] | select(.rust_version == null)] | length'
