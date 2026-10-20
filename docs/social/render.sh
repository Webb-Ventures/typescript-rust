#!/usr/bin/env bash
# Render docs/social/*.html to 3200x1800 PNGs (1600x900 at 2x) with headless Chrome.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
chrome="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
profile=$(mktemp -d)
trap 'rm -rf "$profile"' EXIT
node "$here/build.mjs"
for f in "$here"/*.html; do
  "$root/scripts/limit.sh" --mem-mb 2048 --time-s 60 -- "$chrome" --headless=new --disable-gpu --hide-scrollbars \
    --user-data-dir="$profile" --window-size=1600,900 --force-device-scale-factor=2 \
    --screenshot="${f%.html}.png" "file://$f" 2>/dev/null
  echo "${f%.html}.png"
done
