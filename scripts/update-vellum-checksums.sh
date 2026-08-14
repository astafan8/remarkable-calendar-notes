#!/usr/bin/env bash
# Fills in both Calendar Notes VELBUILD sha512sums from published sources.
#
# The VELBUILD ships explicit `PLACEHOLDER-...` markers rather than
# zeroed-out or invented digests, so nothing can mistake an unreleased
# recipe for a verified one. This script is the only supported way to turn
# those markers into real checksums, and it does so from exactly the URLs
# the recipe's `source=` lists.
#
# Usage:
#   scripts/update-vellum-checksums.sh [version]
#
# `version` defaults to the pkgver in the VELBUILD. Requires curl,
# sha512sum (coreutils), and awk — no Python or Docker.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

VELBUILD="vellum/packages/remarkable-calendar-notes/VELBUILD"
SIDEBAR_VELBUILD="vellum/packages/remarkable-calendar-notes-sidebar/VELBUILD"
PKGVER="${1:-$(sed -n 's/^pkgver=//p' "$VELBUILD")}"
UPSTREAM="$(sed -n 's/^upstream_author=//p' "$VELBUILD" | tr -d '"')"
DIST_REPO="remarkable-calendar-notes"
BASE="https://github.com/${UPSTREAM}/${DIST_REPO}"

ZIP="remarkable-calendar-notes-${PKGVER}.zip"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

echo "==> Downloading release assets for v${PKGVER}"
curl -fsSL -o "$WORK/$ZIP" "${BASE}/releases/download/v${PKGVER}/${ZIP}"
curl -fsSL -o "$WORK/LICENSE" "https://raw.githubusercontent.com/${UPSTREAM}/${DIST_REPO}/v${PKGVER}/LICENSE"

ZIP_SUM="$(sha512sum "$WORK/$ZIP" | cut -d' ' -f1)"
LICENSE_SUM="$(sha512sum "$WORK/LICENSE" | cut -d' ' -f1)"

echo "==> Rewriting $VELBUILD sha512sums"
awk -v zip_name="$ZIP" -v zip_sum="$ZIP_SUM" -v lic_sum="$LICENSE_SUM" '
  /^sha512sums="/ {
    print "sha512sums=\"";
    print zip_sum "  " zip_name;
    print lic_sum "  LICENSE";
    in_block = 1;
    next
  }
  in_block && /^"$/ { print "\""; in_block = 0; next }
  in_block { next }
  { print }
' "$VELBUILD" > "$VELBUILD.tmp"

if grep -q 'PLACEHOLDER-' "$VELBUILD.tmp"; then
  rm -f "$VELBUILD.tmp"
  echo "error: failed to replace the sha512sums block; $VELBUILD left unchanged" >&2
  exit 1
fi
mv "$VELBUILD.tmp" "$VELBUILD"

# The sidebar package sources the very same all-in-one zip (its qmd + rcc
# live inside it) plus rm-appload's GPL LICENSE.
curl -fsSL -o "$WORK/GPL-LICENSE" \
  "https://raw.githubusercontent.com/asivery/rm-appload/v0.5.3/LICENSE"
GPL_SUM="$(sha512sum "$WORK/GPL-LICENSE" | cut -d' ' -f1)"

echo "==> Rewriting $SIDEBAR_VELBUILD sha512sums"
awk -v zip_name="$ZIP" -v zip_sum="$ZIP_SUM" -v lic_sum="$GPL_SUM" '
  /^sha512sums="/ {
    print "sha512sums=\"";
    print zip_sum "  " zip_name;
    print lic_sum "  LICENSE";
    in_block = 1;
    next
  }
  in_block && /^"$/ { print "\""; in_block = 0; next }
  in_block { next }
  { print }
' "$SIDEBAR_VELBUILD" > "$SIDEBAR_VELBUILD.tmp"

if grep -q 'PLACEHOLDER-' "$SIDEBAR_VELBUILD.tmp"; then
  rm -f "$SIDEBAR_VELBUILD.tmp"
  echo "error: failed to replace $SIDEBAR_VELBUILD checksums" >&2
  exit 1
fi
mv "$SIDEBAR_VELBUILD.tmp" "$SIDEBAR_VELBUILD"

echo "==> Done. Verify with:"
echo "    grep -A3 '^sha512sums=' $VELBUILD"
echo "    grep -A3 '^sha512sums=' $SIDEBAR_VELBUILD"
echo "Only after this step is the recipe buildable by Vellum's own tooling."
