# Publishing to Vellum (for reManager / `vellum add` installs)

This is the step-by-step process to get **Calendar Notes** into the Vellum
package repository, so anyone can install it from the **reManager** app (or
`vellum add`) instead of copying a ZIP by hand.

Two important facts up front:

- **Vellum is community-run and maintainer-gated.** There is no
  self-service publish. You prepare a package recipe and open a pull
  request against [`vellum-dev/vellum`](https://github.com/vellum-dev/vellum);
  a Vellum maintainer reviews it, merges it, and then chooses to publish it
  to the **testing** repo (and later **stable**). You cannot skip that.
- Everything below is already scripted in this repo. You mainly run two
  scripts and open one PR.

> **⚠️ Vellum requires the PR to be opened *by a person*, not an agent.**
> Vellum's `README` states: *"Pull requests and issues must be opened by a
> person, not by an agent acting on its own. If a pull request description
> is written by an LLM, we will assume that the pull request was opened by
> an agent, and it will be closed."* It also requires that *"commits must be
> authored and co-authored by people"* — **do not** add
> `Co-Authored-By:` trailers naming an assistant/bot, and do not commit from
> a bot account. So: **you** write the PR description in your own words, and
> the fork commit is authored as **you** (the `publish-vellum-testing.sh`
> commit deliberately adds no agent trailers). This is the one part that
> must not be automated.

The repo already ships two ready recipes under `vellum/packages/`:
`remarkable-calendar-notes` (the app) and
`remarkable-calendar-notes-sidebar` (the optional xochitl sidebar icon).

---

## One-time prerequisites

1. A GitHub account, and a **fork** of `vellum-dev/vellum`
   (click *Fork* on that repo).
2. `git` and the GitHub CLI `gh`, signed in: `gh auth login`.
3. **Docker or Podman** — Vellum's own lint/build tooling runs in a
   container. (Only needed to validate the recipe before the PR.)
4. This repository cloned locally, on a machine with `bash`
   (Git Bash on Windows is fine).

---

## Step 1 — Cut a GitHub release of the app

Vellum downloads the app from a *published* GitHub release, so that must
exist first.

1. Bump the version in `Cargo.toml`, `external.manifest.json`, and both
   `vellum/packages/*/VELBUILD` `pkgver` fields (an `xtask` test enforces
   they match). Reset each VELBUILD `sha512sums` block to the
   `PLACEHOLDER-...` markers.
2. Merge to `main`, then tag and push:
   ```sh
   git tag -a vX.Y.Z -m "Release vX.Y.Z"
   git push origin vX.Y.Z
   ```
3. The `release.yml` GitHub Action builds the static armv7 binary and
   publishes a single asset to the release:
   `remarkable-calendar-notes-<ver>.zip` (plus its `.sha256`/`.sha512`).

Wait for the release to appear on the repo's **Releases** page before
continuing.

## Step 2 — Fill in the recipe checksums from that release

```sh
scripts/update-vellum-checksums.sh
```

This downloads the published zip and writes the real `sha512sums` into both
VELBUILD files. Then sanity-check the packaging metadata:

```sh
cargo test -p xtask
```

Commit the updated VELBUILDs to `main` (a normal PR in this repo).

## Step 3 — Prepare the Vellum pull request

```sh
VELLUM_FORK_URL=git@github.com:<your-user>/vellum.git \
  scripts/publish-vellum-testing.sh
```

The script:

- refuses to proceed if any checksum is still a placeholder;
- clones/updates your fork and copies both package folders into it;
- prints the exact Vellum lint/build commands to run in your fork
  (these need Docker/Podman):
  ```sh
  cd dist/vellum-fork
  ./scripts/lint-packages.sh remarkable-calendar-notes --apkbuild-lint
  ./scripts/build-package.sh remarkable-calendar-notes armv7
  ./scripts/lint-packages.sh remarkable-calendar-notes-sidebar --apkbuild-lint
  ./scripts/build-package.sh remarkable-calendar-notes-sidebar noarch
  ```
- after you confirm, commits and pushes the branch to your fork.

## Step 4 — Open the PR to `vellum-dev/vellum`

Open the compare link the script prints (or use `gh pr create` from the
fork) and submit. **Write the description yourself, in your own words** —
per Vellum's rules an LLM-written description gets the PR auto-closed (see
the warning at the top). Keep it factual: what the app is (a read-only
calendar with per-day handwritten notes for reMarkable 2, via AppLoad),
that it's armv7/RM2-only, and that the optional `-sidebar` package adds an
xochitl sidebar icon on OS 3.27. A Vellum maintainer reviews it; when they
merge and publish it to the **testing** channel, it becomes installable.

## Step 5 — Install on the device (reManager or CLI)

Once it's in the testing repo:

- **reManager app:** refresh/update the package list, search for
  **Calendar Notes**, and install. Install
  *Calendar Notes Sidebar* too if you want the xochitl sidebar icon
  (reMarkable OS 3.27 only).
- **Command line:**
  ```sh
  vellum add remarkable-calendar-notes@testing
  vellum add remarkable-calendar-notes-sidebar@testing   # optional sidebar
  ```

## Step 6 — Promotion to stable

After the package has been validated in testing, a Vellum maintainer may
promote it to the **stable** channel, at which point `@testing` is no
longer needed. This is maintainer-driven; there is nothing for you to run.

---

### Updating to a new version later

Repeat Steps 1–4 with the new version number. Vellum keys packages by
`pkgver`, so bump it, re-run `update-vellum-checksums.sh`, and open a fresh
PR (or push to the same branch). Users then get the update through
reManager / `vellum upgrade`.
