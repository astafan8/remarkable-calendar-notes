#!/usr/bin/env bash
# Prepares (and optionally opens) a pull request against vellum-dev/vellum
# adding the remarkable-calendar-notes package under vellum/packages/, so
# a Vellum maintainer can review it and — at their discretion — publish it
# to the vellum testing repository for on-device validation.
#
# IMPORTANT — this script does NOT and CANNOT publish the package to Vellum
# on its own:
#   - Vellum requires a maintainer to review every PR before it is merged.
#   - Only after merge do Vellum maintainers publish a package to their
#     *testing* repository; there is no automatic/self-service path, by
#     Vellum's own design.
#   - This script prepares and pushes a fork branch. It deliberately does
#     not open the PR: Vellum requires the repository owner to review the
#     contribution and personally write/open it.
#
# Requirements (all yours to provide — none of this is bundled or assumed):
#   - A personal GitHub fork of https://github.com/vellum-dev/vellum
#   - `git` and (optionally) the `gh` CLI, authenticated with a GitHub
#     Personal Access Token (PAT) that has `repo` scope for your fork. A PAT
#     (or an equivalent `gh auth login`) is unavoidable here: pushing a
#     branch and opening a PR both require authenticating as you.
#   - Docker or Podman, to run vellum-dev/vellum's own package lint/build
#     scripts before you open the PR — this script does not attempt to
#     reproduce Vellum's build tooling itself.
#   - A published GitHub Release of this repository, and real checksums
#     generated from it with scripts/update-vellum-checksums.sh. This
#     script refuses to commit a recipe that still has placeholders.
#
# Usage:
#   VELLUM_FORK_URL=git@github.com:<you>/vellum.git \
#   scripts/publish-vellum-testing.sh
#
# Optional:
#   VELLUM_WORKDIR   local clone location (default: dist/vellum-fork)
#   VELLUM_BRANCH    branch name (default: add-remarkable-calendar-notes)
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

: "${VELLUM_FORK_URL:?Set VELLUM_FORK_URL to your fork of vellum-dev/vellum, e.g. git@github.com:you/vellum.git}"
WORKDIR="${VELLUM_WORKDIR:-dist/vellum-fork}"
BRANCH="${VELLUM_BRANCH:-add-remarkable-calendar-notes}"
PACKAGES="remarkable-calendar-notes remarkable-calendar-notes-sidebar"

for package in $PACKAGES; do
  velbuild="vellum/packages/$package/VELBUILD"
  if grep -q 'PLACEHOLDER-' "$velbuild"; then
    echo "error: $velbuild still contains checksum placeholders." >&2
    echo "       Publish a GitHub Release, then run:" >&2
    echo "         scripts/update-vellum-checksums.sh" >&2
    exit 1
  fi
done

if [ ! -d "$WORKDIR/.git" ]; then
  echo "==> Cloning $VELLUM_FORK_URL into $WORKDIR"
  git clone "$VELLUM_FORK_URL" "$WORKDIR"
else
  echo "==> Updating existing clone at $WORKDIR"
  git -C "$WORKDIR" fetch origin
fi

git -C "$WORKDIR" checkout -B "$BRANCH"

echo "==> Copying package definitions"
mkdir -p "$WORKDIR/packages"
for package in $PACKAGES; do
  rm -rf "$WORKDIR/packages/$package"
  cp -r "vellum/packages/$package" "$WORKDIR/packages/"
done

echo
echo "==> Files staged in $WORKDIR. Before committing, you must:"
echo "    1. Run vellum's own lint: (cd $WORKDIR && ./scripts/lint-packages.sh remarkable-calendar-notes --apkbuild-lint)"
echo "    2. Run vellum's own build: (cd $WORKDIR && ./scripts/build-package.sh remarkable-calendar-notes armv7)"
echo "    3. Repeat lint/build for remarkable-calendar-notes-sidebar (noarch)."
echo "    Both require Docker or Podman; neither is invoked by this script."
echo

read -r -p "Have you completed the steps above and want to commit + push now? [y/N] " confirm
if [ "$confirm" != "y" ] && [ "$confirm" != "Y" ]; then
  echo "Stopping. Re-run this script once you're ready to commit."
  exit 0
fi

git -C "$WORKDIR" add packages/remarkable-calendar-notes packages/remarkable-calendar-notes-sidebar
git -C "$WORKDIR" commit -m "Add Calendar Notes AppLoad and sidebar packages"
git -C "$WORKDIR" push -u origin "$BRANCH"

echo
echo "==> Branch pushed. Review it, then open and describe the PR yourself:"
echo "    https://github.com/vellum-dev/vellum/compare/main...astafan8:$BRANCH?expand=1"

echo
echo "A Vellum maintainer must review and merge this PR, then choose to"
echo "publish it to the testing repository, before it is installable via"
echo "'vellum add remarkable-calendar-notes@testing'. The optional sidebar is"
echo "installed separately with 'vellum add remarkable-calendar-notes-sidebar@testing'."
