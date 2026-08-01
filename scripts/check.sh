#!/usr/bin/env bash
# Runs the same checks CI runs: formatting, clippy (warnings as errors), and
# the full test suite for every workspace crate that builds on this host.
#
# Usage: scripts/check.sh
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

echo "==> cargo fmt --check"
cargo fmt --all -- --check

echo "==> cargo clippy (workspace, all targets, warnings as errors)"
cargo clippy --workspace --all-targets -- -D warnings

echo "==> cargo test (workspace)"
cargo test --workspace

echo "==> cargo clippy (calnotes-device, armv7 target — the real reMarkable 2 ABI)"
if rustup target list --installed | grep -q armv7-unknown-linux-gnueabihf; then
  # Only calnotes-device is checked here. calnotes-core (and therefore
  # calnotes-app) pull in `ring` via ureq/rustls, whose build script needs
  # an arm-linux-gnueabihf C cross compiler; without one, even `cargo
  # check` fails before reaching this project's own code. calnotes-device
  # has no C dependency, and it is the crate whose Unix-only code
  # (`qtfb.rs`) is never compiled on a Windows/macOS-hosted check at all —
  # so this pass is the one that actually adds coverage locally.
  #
  # The full workspace, including calnotes-app's Unix-only device event
  # loop, is clippy-checked for armv7 in CI's `armv7-check` job, which
  # installs gcc-arm-linux-gnueabihf first (.github/workflows/ci.yml).
  cargo clippy -p calnotes-device --all-targets --target armv7-unknown-linux-gnueabihf -- -D warnings
  if command -v arm-linux-gnueabihf-gcc >/dev/null 2>&1; then
    echo "==> cargo clippy (whole workspace, armv7 — cross C compiler found)"
    cargo clippy --workspace --target armv7-unknown-linux-gnueabihf -- -D warnings
  else
    echo "    (workspace armv7 pass skipped: install gcc-arm-linux-gnueabihf to enable)"
  fi
else
  echo "    (skipped: run 'rustup target add armv7-unknown-linux-gnueabihf' to enable)"
fi

echo "All checks passed."
