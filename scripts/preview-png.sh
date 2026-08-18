#!/usr/bin/env bash
# Screenshot a dashboard preview, so the graph gradients can be reviewed without attaching to a
# live process. Plain-text previews prove the layout; only a picture proves the colours.
#
#   scripts/preview-png.sh [fixture] [width] [height] [repeat] [out.png]
set -euo pipefail

fixture="${1:-loaded}"
width="${2:-120}"
height="${3:-40}"
repeat="${4:-60}"
out="${5:-preview.png}"

root="$(cd "$(dirname "$0")/.." && pwd)"
page="$(mktemp -t countercow-preview).html"
trap 'rm -f "$page"' EXIT

cargo run --quiet --manifest-path "$root/Cargo.toml" --example preview -- \
  "$fixture" "$width" "$height" --html --repeat "$repeat" > "$page"

# Any headless Chromium will do; the page is self-contained and needs no network.
for candidate in \
  "${CHROME:-}" \
  "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
  "/Applications/Chromium.app/Contents/MacOS/Chromium" \
  "$(command -v google-chrome || true)" \
  "$(command -v chromium || true)"
do
  if [ -n "$candidate" ] && [ -x "$candidate" ]; then
    chrome="$candidate"
    break
  fi
done

if [ -z "${chrome:-}" ]; then
  echo "No Chrome or Chromium found. Set CHROME to one, or open $page yourself." >&2
  trap - EXIT
  exit 1
fi

# The page lays cells out at 9.6x19 px, so size the window to the grid plus its padding.
"$chrome" --headless --disable-gpu --hide-scrollbars \
  --screenshot="$out" \
  --window-size=$((width * 10 + 60)),$((height * 19 + 60)) \
  "file://$page" 2>/dev/null

echo "$out"
