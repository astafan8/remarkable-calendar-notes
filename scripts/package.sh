#!/usr/bin/env bash
# Assembles an AppLoad bundle: a zip containing
#   remarkable-calendar-notes/
#     remarkable-calendar-notes   (the armv7 release binary)
#     icon.png
#     external.manifest.json
#
# This matches the on-device layout AppLoad expects under
# /home/root/xovi/exthome/appload/<app>/ — see docs/ARCHITECTURE.md.
#
# Usage: scripts/package.sh
# Requires: an armv7 release binary already built (scripts/build.sh --armv7).
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

BIN="target/armv7-unknown-linux-gnueabihf/release/remarkable-calendar-notes"
if [ ! -f "$BIN" ]; then
  echo "error: $BIN not found — run scripts/build.sh --armv7 first" >&2
  exit 1
fi

if [ ! -f "assets/icon.png" ]; then
  echo "error: assets/icon.png not found — run 'cargo run -p xtask -- icon'" >&2
  exit 1
fi

VERSION="$(grep -m1 '"version"' external.manifest.json | sed -E 's/.*"version"\s*:\s*"([^"]+)".*/\1/')"
STAGE="dist/stage/remarkable-calendar-notes"
OUT="dist/remarkable-calendar-notes-${VERSION}-armv7.zip"

rm -rf dist/stage
mkdir -p "$STAGE"
cp "$BIN" "$STAGE/remarkable-calendar-notes"
cp assets/icon.png "$STAGE/icon.png"
cp external.manifest.json "$STAGE/external.manifest.json"
chmod 755 "$STAGE/remarkable-calendar-notes"

mkdir -p dist
rm -f "$OUT"
(cd dist/stage && zip -r -X "../../$OUT" remarkable-calendar-notes) >/dev/null
rm -rf dist/stage

sha256sum "$OUT" > "$OUT.sha256"
echo "Wrote $OUT"
echo "Wrote $OUT.sha256"
cat "$OUT.sha256"
