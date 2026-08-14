#!/usr/bin/env bash
# Assembles the single all-in-one AppLoad bundle, mirroring what the release
# workflow publishes: one zip
#
#   remarkable-calendar-notes-<ver>/
#     remarkable-calendar-notes/    (app: binary, icon.png, external.manifest.json)
#     sidebar/                      (calendarNotesSidebar.qmd + .rcc + README)
#     diagnostics/                  (host-side installers + collectors)
#     INSTALL.md
#
# The app subfolder matches the on-device layout AppLoad expects under
# /home/root/xovi/exthome/appload/<app>/ — see docs/ARCHITECTURE.md. Both
# Vellum recipes source this one zip; the installer picks the app out of it.
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
if ! command -v rcc >/dev/null 2>&1 && [ ! -x /usr/lib/qt5/bin/rcc ]; then
  echo "error: Qt 5 rcc not found — install qtbase5-dev-tools" >&2
  exit 1
fi
RCC="$(command -v rcc || echo /usr/lib/qt5/bin/rcc)"

VERSION="$(grep -m1 '"version"' external.manifest.json | sed -E 's/.*"version"\s*:\s*"([^"]+)".*/\1/')"
COMBINED="dist/stage-combined/remarkable-calendar-notes-${VERSION}"
OUT="dist/remarkable-calendar-notes-${VERSION}.zip"

rm -rf dist/stage-combined
mkdir -p "$COMBINED/remarkable-calendar-notes" "$COMBINED/sidebar" "$COMBINED/diagnostics"

cp "$BIN" "$COMBINED/remarkable-calendar-notes/remarkable-calendar-notes"
cp assets/icon.png "$COMBINED/remarkable-calendar-notes/icon.png"
cp external.manifest.json "$COMBINED/remarkable-calendar-notes/external.manifest.json"
chmod 755 "$COMBINED/remarkable-calendar-notes/remarkable-calendar-notes"

cp sidebar/3.27/calendarNotesSidebar.qmd "$COMBINED/sidebar/"
"$RCC" --binary \
  -o "$COMBINED/sidebar/calendarNotesSidebar.rcc" \
  sidebar/3.27/calendarNotesSidebar.qrc
cp sidebar/README.md "$COMBINED/sidebar/README.md"

for helper in collect-device-log.ps1 collect-device-log.sh device-diagnostics-remote.sh \
  diagnose-on-device.sh install-device.ps1 install-device.sh; do
  cp "scripts/$helper" "$COMBINED/diagnostics/$helper"
done
cp docs/DIAGNOSTICS.md "$COMBINED/diagnostics/README.md"
cp docs/INSTALL.md "$COMBINED/INSTALL.md"

mkdir -p dist
rm -f "$OUT"
(cd dist/stage-combined && zip -r -X "../../$(basename "$OUT")" "remarkable-calendar-notes-${VERSION}") >/dev/null
rm -rf dist/stage-combined

sha256sum "$OUT" > "$OUT.sha256"
sha512sum "$OUT" > "$OUT.sha512"
echo "Wrote $OUT"
cat "$OUT.sha256"
