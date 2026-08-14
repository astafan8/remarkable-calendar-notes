# Development

## Toolchain

The pinned toolchain is in [`rust-toolchain.toml`](../rust-toolchain.toml)
(installed automatically by `rustup` on first `cargo` invocation in this
repo). It includes `rustfmt`, `clippy`, and the
`armv7-unknown-linux-gnueabihf` target.

## Building

```sh
scripts/build.sh              # debug host build (for `preview`)
scripts/build.sh --release    # release host build
scripts/build.sh --armv7      # cross-build the release reMarkable 2 binary
```

The `--armv7` build needs an `arm-linux-gnueabihf` C cross compiler on
`PATH` (for `ring`, a TLS dependency) — e.g. on Debian/Ubuntu:
`sudo apt-get install gcc-arm-linux-gnueabihf`. CI instead builds inside
[`ghcr.io/toltec-dev/rust:v4.0`](https://github.com/toltec-dev/toolchain),
which already has the right cross toolchain — see
`.github/workflows/release.yml`.

## Testing

```sh
scripts/check.sh
```

runs `cargo fmt --check`, `cargo clippy --workspace --all-targets -D
warnings`, `cargo test --workspace`, and (if the armv7 target is
installed) a clippy pass of `calnotes-device` against the real armv7 ABI —
which is the only way its Unix-only `qtfb.rs` gets compiled at all when
you develop on Windows or macOS.

`calnotes-core` and `calnotes-app` are only added to that armv7 pass if an
`arm-linux-gnueabihf-gcc` is on `PATH`: they pull in `ring` via
ureq/rustls, whose build script compiles C and fails without a cross
compiler. CI's separate `armv7-check` job installs one and covers the
whole workspace — including `calnotes-app`'s Unix-only device event loop,
which no Windows/macOS-hosted check can compile.

## Desktop preview (no device required)

```sh
cargo run -p calnotes-app -- preview --view month --out preview.ppm
```

Renders the current screen to a binary PPM using nothing but
`calnotes-core`'s software rasterizer — works identically on Windows,
Linux, and macOS. Useful options:

```
--view <day|week|workweek|twoweeks|month>
--refresh     # fetch fresh events from enabled sources first
--out <path>
```

State (config, ink, cache) is read from/written to the same data
directory the real device uses; override it for a clean sandbox:

```sh
REMARKABLE_CALENDAR_NOTES_DATA_DIR=./scratch-data cargo run -p calnotes-app -- preview
```

## Device-mode QTFB screenshot (Linux)

CI also launches the real host-compiled binary's `run` subcommand against a
minimal, protocol-faithful AppLoad QTFB host and captures the shared RGB565
framebuffer:

```sh
cargo build --release -p calnotes-app
python3 scripts/qtfb_ci_harness.py \
  --binary target/release/remarkable-calendar-notes \
  --screenshot target/qtfb-calendar-notes.ppm
```

This exercises the Unix socket, shared-memory, device-loop, and update
signaling paths without proprietary firmware. It is not an OS/xochitl/XOVI
emulator; see [CI_DEVICE_EMULATION.md](CI_DEVICE_EMULATION.md) for the research,
coverage boundaries, and pinned upstream sources.

## Regenerating the icon

```sh
cargo run -p xtask -- icon
```

Deterministically regenerates `assets/icon.png` from code (see
`xtask/src/main.rs`) — no image editor or external asset needed.

## Releasing

1. Bump `version` in the workspace `Cargo.toml`, `external.manifest.json`,
   and `pkgver` in both `vellum/packages/*/VELBUILD` files (an `xtask`
   test fails if these drift apart).
2. Tag `vX.Y.Z` and push. `.github/workflows/release.yml` cross-builds
   the static musl armv7 binary, packages the single all-in-one
   `remarkable-calendar-notes-<ver>.zip` (app + optional sidebar +
   host-side installers/collectors), and attaches it — with SHA-256/512
   checksums — to a GitHub Release in the public source repository. That
   one zip is the only published asset; both Vellum recipes source it.
3. Fill in both Vellum recipes' checksums from that release with
   `scripts/update-vellum-checksums.sh`.
4. Review and clean the VELBUILD, run Vellum's own lint/build tools, and
   use `scripts/publish-vellum-testing.sh` to prepare a fork branch.
   Vellum requires the repository owner—not an automation operator—to
   inspect that branch and personally open and describe the pull request.

The sidebar QMD is derived from rm-appload's GPL-3.0-only patch, lives
under `sidebar/`, and is separately GPL-3.0-only. It must be re-authored
and tested against a real device-generated QMLDiff hashtab before widening
its firmware range.

## Device diagnostics

Every run appends to
`~/.local/share/remarkable-calendar-notes/calendar-notes.log` and rotates
it at 1 MiB. The device loop records state-loading, QTFB connection,
render completion time, non-white pixel count, shared-memory publication,
full-update acknowledgement, background refresh status, and panic
details. It deliberately omits source configuration values, credentials,
tokens, and event text.

The collectors run on the computer connected to the tablet and stream
one archive over one SSH session. Collect diagnostics from Windows with:

```powershell
.\scripts\collect-device-log.ps1
```

Or from Linux/macOS with:

```sh
./scripts/collect-device-log.sh
```

Both produce a single shareable archive, locate logs outside the expected
directory, and collect launch/permission evidence even if the app never
created a log. See
[`DIAGNOSTICS.md`](DIAGNOSTICS.md).
