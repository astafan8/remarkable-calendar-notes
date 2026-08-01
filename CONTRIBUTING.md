# Contributing

Thanks for considering a contribution to remarkable-calendar-notes.

## Getting set up

See [`docs/DEVELOPMENT.md`](docs/DEVELOPMENT.md) for toolchain setup,
building, testing, and the desktop `preview` workflow (no device needed).

## Before opening a PR

Run the same checks CI runs:

```sh
scripts/check.sh
```

This runs `cargo fmt --check`, `cargo clippy -D warnings` (workspace,
including the real armv7 reMarkable 2 target for `calnotes-core`/
`calnotes-device` if you have that target installed), and the full test
suite.

## Code organization

- `crates/calnotes-core` — all calendar/ICS/recurrence/ink/persistence
  logic. Platform-independent; this is where almost all logic and tests
  should live.
- `crates/calnotes-device` — QTFB protocol client, `cfg(unix)`-gated.
  Keep this crate minimal and free of business logic.
- `crates/calnotes-app` — the binary: screen/navigation state, the
  source editor, rendering, and (Unix-only) the real device event loop.
- `xtask` — small dev tools (currently just deterministic icon
  generation).
- `vellum/` — the Vellum package recipe (see `docs/INSTALL.md` for its
  current, not-yet-published status).

## Testing philosophy

Prefer adding tests to `calnotes-core` over `calnotes-app` where
possible — they run on every platform without a device. Please add
focused unit tests for:

- New ICS/RRULE parsing behavior.
- New view-layout or ink-normalization logic.
- New calendar source behavior (using canned fixtures, not live network
  calls — see `sources::caldav`'s XML-extraction tests for the pattern).

## Commit / PR conventions

- Keep PRs focused; one logical change per PR.
- Describe *why*, not just *what*, in the PR description for anything
  touching protocol details (QTFB, CalDAV, OAuth) or recurrence rules —
  these are easy to get subtly wrong and hard to review without context.
- Do not rewrite git history on shared branches.

## Reporting security issues

Please see [`SECURITY.md`](SECURITY.md) — do not open a public issue for
security-sensitive reports.
