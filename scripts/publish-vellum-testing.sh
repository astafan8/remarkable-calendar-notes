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
#   - This script only gets a correctly-formed PR in front of a maintainer.
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
#   VELLUM_FORK_URL=git@github.com:<you>/vellum.git scripts/publish-vellum-testing.sh
#
# Optional:
#   VELLUM_WORKDIR   local clone location (default: dist/vellum-fork)
#   VELLUM_BRANCH    branch name (default: add-remarkable-calendar-notes)
#   OPEN_PR=1        also run `gh pr create` (requires `gh auth login` first)
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

: "${VELLUM_FORK_URL:?Set VELLUM_FORK_URL to your fork of vellum-dev/vellum, e.g. git@github.com:you/vellum.git}"
WORKDIR="${VELLUM_WORKDIR:-dist/vellum-fork}"
BRANCH="${VELLUM_BRANCH:-add-remarkable-calendar-notes}"
VELBUILD="vellum/packages/remarkable-calendar-notes/VELBUILD"

if grep -q 'PLACEHOLDER-' "$VELBUILD"; then
  echo "error: $VELBUILD still contains checksum placeholders." >&2
  echo "       Publish a GitHub Release, then run:" >&2
  echo "         scripts/update-vellum-checksums.sh" >&2
  exit 1
fi

if [ ! -d "$WORKDIR/.git" ]; then
  echo "==> Cloning $VELLUM_FORK_URL into $WORKDIR"
  git clone "$VELLUM_FORK_URL" "$WORKDIR"
else
  echo "==> Updating existing clone at $WORKDIR"
  git -C "$WORKDIR" fetch origin
fi

git -C "$WORKDIR" checkout -B "$BRANCH"

echo "==> Copying package definition"
mkdir -p "$WORKDIR/packages"
rm -rf "$WORKDIR/packages/remarkable-calendar-notes"
cp -r vellum/packages/remarkable-calendar-notes "$WORKDIR/packages/"

echo
echo "==> Files staged in $WORKDIR. Before committing, you must:"
echo "    1. Run vellum's own lint: (cd $WORKDIR && ./scripts/lint-packages.sh remarkable-calendar-notes --apkbuild-lint)"
echo "    2. Run vellum's own build: (cd $WORKDIR && ./scripts/build-package.sh remarkable-calendar-notes armv7)"
echo "    Both require Docker or Podman; neither is invoked by this script."
echo

read -r -p "Have you completed the steps above and want to commit + push now? [y/N] " confirm
if [ "$confirm" != "y" ] && [ "$confirm" != "Y" ]; then
  echo "Stopping. Re-run this script once you're ready to commit."
  exit 0
fi

git -C "$WORKDIR" add packages/remarkable-calendar-notes
git -C "$WORKDIR" commit -m "Add remarkable-calendar-notes AppLoad package"
git -C "$WORKDIR" push -u origin "$BRANCH"

echo
if [ "${OPEN_PR:-0}" = "1" ] && command -v gh >/dev/null 2>&1; then
  echo "==> Opening PR via gh CLI (requires prior 'gh auth login')"
  (cd "$WORKDIR" && gh pr create \
    --repo vellum-dev/vellum \
    --title "Add remarkable-calendar-notes" \
    --body "Adds the remarkable-calendar-notes AppLoad calendar/notes app (reMarkable 2 only). See https://github.com/astafan8/remarkable-calendar-notes for source, docs, and CI. Requesting maintainer review before any testing-repo publication.")
else
  echo "==> Branch pushed. Open a PR manually at:"
  echo "    https://github.com/vellum-dev/vellum/compare/main...$(basename "$(dirname "$VELLUM_FORK_URL")")-fork:$BRANCH?expand=1"
  echo "    (or set OPEN_PR=1 with the gh CLI authenticated to do this automatically)"
fi

echo
echo "A Vellum maintainer must review and merge this PR, then choose to"
echo "publish it to the testing repository, before it is installable via"
echo "'vellum add remarkable-calendar-notes@testing'. This is not automatic."
