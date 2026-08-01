# Firmware & device compatibility

## Target device

**reMarkable 2 only.** This app is built exclusively for RM2's native
QTFB resolution (1404x1872, RGB565) and its 32-bit `armv7` userspace (see
`docs/ARCHITECTURE.md` for why the 32- vs 64-bit distinction matters for
the QTFB protocol). It is not built for, and has not been tested on, the
reMarkable 1, Paper Pro, Paper Pro Move, or Paper Pure.

## Firmware (reMarkable OS) versions

| Component | Supported range |
|---|---|
| Main app (`remarkable-calendar-notes`) | `remarkable-os >= 3.26, < 3.28` |

The main app's range matches its only hard dependency, `appload >= 0.5.3`
— any reMarkable OS version AppLoad itself supports in that range should
work. The `<3.28` upper bound is precautionary: reMarkable OS updates can
change QTFB's behavior or AppLoad's compatibility without notice, and
this project cannot pre-verify a version that doesn't exist yet.

No firmware-pinned QML/QMD resource patch is shipped; there is nothing
here that only works on one exact patch release. See
[LIMITATIONS.md](LIMITATIONS.md#no-dedicated-xochitl-sidebar-icon).

## What "supported" means here

- **Main app, within the stated range:** expected to work; please file an
  issue with your exact `remarkable-os` version if it doesn't.
- **Anything else:** untested. It may work, but isn't a support target.

## Bumping the supported range

When a new reMarkable OS version is confirmed to work, update:

1. `vellum/packages/remarkable-calendar-notes/VELBUILD`'s `depends=`.
2. This file.
3. If AppLoad itself has bumped its minimum required version, the
   `appload>=` bound too.
