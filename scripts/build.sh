#!/usr/bin/env bash
# Builds remarkable-calendar-notes.
#
# Usage:
#   scripts/build.sh              Build a debug host binary (for `preview`)
#   scripts/build.sh --release    Build a release host binary
#   scripts/build.sh --armv7      Cross-build the release armv7 binary for
#                                 the reMarkable 2 (requires the
#                                 armv7-unknown-linux-gnueabihf target and an
#                                 arm-linux-gnueabihf C cross compiler on
#                                 PATH — see docs/DEVELOPMENT.md; CI instead
#                                 builds inside ghcr.io/toltec-dev/rust:v4.0,
#                                 which ships that toolchain already).
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

mode="${1:-debug}"

case "$mode" in
  --release)
    cargo build --release -p calnotes-app
    ;;
  --armv7)
    rustup target add armv7-unknown-linux-gnueabihf >/dev/null 2>&1 || true
    cargo build --release -p calnotes-app --target armv7-unknown-linux-gnueabihf
    echo "Binary: target/armv7-unknown-linux-gnueabihf/release/remarkable-calendar-notes"
    ;;
  debug|"")
    cargo build -p calnotes-app
    ;;
  *)
    echo "unknown mode: $mode (expected --release, --armv7, or nothing)" >&2
    exit 1
    ;;
esac
