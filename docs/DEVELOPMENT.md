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

## Regenerating the icon

```sh
cargo run -p xtask -- icon
```

Deterministically regenerates `assets/icon.png` from code (see
`xtask/src/main.rs`) — no image editor or external asset needed.

## Releasing

1. Bump `version` in the workspace `Cargo.toml`, `external.manifest.json`,
   and `pkgver` in `vellum/packages/remarkable-calendar-notes/VELBUILD`
   (an `xtask` test fails if these drift apart).
2. Tag `vX.Y.Z` and push. `.github/workflows/release.yml` cross-builds
   the armv7 binary inside `ghcr.io/toltec-dev/rust:v4.0`, packages an
   AppLoad bundle zip, attaches it (with SHA-256/512 checksums) to a
   private-source GitHub Release, and mirrors the artifacts to the public
   `remarkable-calendar-notes-releases` repository.
3. Fill in the Vellum recipe's checksums from that release with
   `scripts/update-vellum-checksums.sh`.
4. Do not submit the Vellum recipe while this source repository is
   private. Vellum requires maintainers to review and build the source,
   and requires the repository owner—not an automation operator—to open
   and describe the contribution. If the source is made public later,
   review/clean the VELBUILD, run Vellum's own lint/build tools, and then
   use `scripts/publish-vellum-testing.sh`.
