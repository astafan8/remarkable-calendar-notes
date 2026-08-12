# CI device-emulation research

Research date: **2026-08-12**.

## Decision

Booting the supported reMarkable OS 3.26–3.27 stack, installing XOVI and
AppLoad into it, launching this app, and taking a trustworthy screenshot is
not currently a practical public-CI test without putting proprietary
reMarkable software into the build/cache. The strongest reproducible
substitute is a protocol-faithful fake AppLoad QTFB host. It launches the real
host-compiled `remarkable-calendar-notes run` binary, negotiates the same
`SOCK_SEQPACKET` and shared-memory ABI, waits for a full update, checks that
the frame is nonblank, and converts the actual shared RGB565 pixels to PPM.

This test covers the AppLoad manifest contract, device-mode startup, QTFB
initialization, the Unix socket/mmap plumbing, rendering, shared-memory
publication, update signaling, clean host-disconnect handling, and screenshot
capture. The separate armv7 CI job still compiles the complete program for the
real RM2 ABI. It does **not** claim to test xochitl/XOVI injection, AppLoad's
QML UI, firmware compatibility, e-ink waveforms, or hardware input.

## Projects evaluated

### XOVI and AppLoad

XOVI is an `LD_PRELOAD`-style hook/dynamic-link framework for a running Linux
application. Its current README says arm32 remains less tested than aarch64
and documents runtime hooking rather than a standalone xochitl replacement.
AppLoad is a XOVI extension. Its desktop build is useful for developing QML
applications, but this project is an **external QTFB application**, not an
AppLoad QML frontend. AppLoad's published protocol confirms:

- `/tmp/qtfb.sock` is an `AF_UNIX` `SOCK_SEQPACKET` socket;
- RM2 is 1404x1872 RGB565;
- initialization returns a POSIX shared-memory key and size;
- external apps receive the framebuffer ID through `QTFB_KEY`; and
- full/partial updates and pen/touch/keyboard input are fixed-size messages.

Therefore running all of AppLoad in CI adds Qt/XOVI complexity but does not
improve coverage of this external app beyond faithfully hosting that protocol.

### reManager

reManager is a desktop Vellum/SSH management client. Its README and connection
implementation operate against a reachable tablet over SSH; it is not a
device, xochitl, AppLoad, framebuffer, or QEMU emulator. It can install the
bundle on real hardware but cannot make a CI runner behave like that hardware.

### rM-docker, run-in-reMarkable-action, QEMU, and rm2fb

`timower/rM-docker` is the closest full-system option. It boots an ARM Linux
kernel under `qemu-system-arm`, downloads a selected official reMarkable
firmware, extracts its root filesystem into the image, and can start the
proprietary `/usr/bin/xochitl` with an `rm2fb` preload shim. Its X11/SDL viewer
is an rm2fb emulator, not QTFB/AppLoad itself. The image defaults to old
firmware, while this app supports 3.26–3.27; compatibility of those releases,
XOVI, AppLoad, the patched xochitl, and the emulated i.MX7 machine would all
remain additional moving parts.

`Eeems-Org/run-in-remarkable-action` productizes the same approach and pins
rM-docker commit `4b6a612...`. Its Docker build downloads/extracts firmware and
GitHub Actions caching can retain the resulting layers. That is unsuitable as
the default public workflow here: it puts reMarkable OS/xochitl artifacts into
the build and potentially its cache, contrary to this task's artifact-free
requirement. This document does not offer a legal opinion about private
firmware use. A future opt-in workflow could require a repository owner to
supply an appropriately obtained firmware artifact and explicitly disable
shared caches, but it would still need per-firmware XOVI/AppLoad validation.

`ddvk/remarkable2-framebuffer` and `timower/rM2-stuff` expose or emulate the
lower rm2fb/SWTCON path by preloading xochitl and forwarding framebuffer
updates. They are valuable for native framebuffer apps, but Calendar Notes
uses AppLoad QTFB, one layer above them. Neither supplies an open xochitl,
AppLoad, or reMarkable OS replacement.

QEMU user-mode for the release ARM binary was also considered. The binary is
dynamically linked and needs a compatible armv7 userspace/runtime. Supplying
that from supported reMarkable firmware returns to the proprietary-artifact
problem. A generic armhf sysroot would test instruction/loader compatibility
but not AppLoad or reMarkable OS, while making CI slower and less reproducible
than the host binary plus the existing real-target cross build.

## Implemented harness

`scripts/qtfb_ci_harness.py` uses only the Python standard library. On Linux it:

1. validates that `external.manifest.json` names the binary, enables QTFB, and
   selects the original RM2 aspect ratio;
2. binds the real AppLoad socket path and launches the real device-mode binary
   with a unique `QTFB_KEY`;
3. validates the initialize packet and creates `/dev/shm/qtfb_<key>`;
4. replies with the RM2 **32-bit** `ServerMessage` layout expected on device;
5. waits for the application's full-update packet;
6. rejects an all-white frame and writes its shared RGB565 pixels as PPM; and
7. closes the host socket and verifies that the application exits cleanly.

The normal CI job runs unit tests for packet/manifest/pixel conversion, stages
the binary, manifest, and icon with the same layout AppLoad installs, runs the
end-to-end harness after the release host build, and uploads the screenshot and
diagnostic logs. Locally on Linux:

```sh
cargo build --release -p calnotes-app
python3 -m unittest scripts/test_qtfb_ci_harness.py
python3 scripts/qtfb_ci_harness.py \
  --binary target/release/remarkable-calendar-notes \
  --screenshot target/qtfb-calendar-notes.ppm
```

## Recommended next steps

1. Keep the fake-QTFB screenshot test and armv7 compile as the required,
   artifact-free CI baseline.
2. Add scripted QTFB input events if specific navigation/ink regressions need
   end-to-end coverage; the existing in-process tests already cover those
   paths more precisely today.
3. Run release candidates on a physical RM2 for xochitl/XOVI/AppLoad,
   touch/pen, e-ink, and firmware validation.
4. Only add full-system QEMU as an opt-in, non-public-artifact workflow after
   legal review and a demonstrated 3.26/3.27 image that installs current XOVI
   and AppLoad reproducibly.

## Authoritative sources

Pinned links are used so the findings remain auditable:

- [XOVI README at `2b99649`](https://github.com/asivery/xovi/blob/2b99649f5e4fd6288be7792a8570bd16418adb70/README.MD)
- [AppLoad README at `123c29e`](https://github.com/asivery/rm-appload/blob/123c29eb2fa6d1025cb3fa1b47bece6cee0a74f6/README.MD)
- [AppLoad QTFB ABI constants/structs](https://github.com/asivery/rm-appload/blob/123c29eb2fa6d1025cb3fa1b47bece6cee0a74f6/src/qtfb/common.h)
- [AppLoad QTFB client handshake](https://github.com/asivery/rm-appload/blob/123c29eb2fa6d1025cb3fa1b47bece6cee0a74f6/backends/qtfb-clients/cpp/qtfb-client.cpp)
- [AppLoad QTFB paint/input controller](https://github.com/asivery/rm-appload/blob/123c29eb2fa6d1025cb3fa1b47bece6cee0a74f6/src/qtfb/FBController.cpp)
- [reManager README at `04e652b`](https://github.com/rmitchellscott/reManager/blob/04e652b5b033c868d71200aec20e4185aaf0e665/README.md)
- [reManager SSH connection implementation](https://github.com/rmitchellscott/reManager/blob/04e652b5b033c868d71200aec20e4185aaf0e665/app_connection.go)
- [rM-docker README at `4b6a612`](https://github.com/timower/rM-docker/blob/4b6a612941cc29adc7ca23c1da38e641655d2ed2/Readme.md)
- [rM-docker Dockerfile: firmware extraction, QEMU, xochitl/rm2fb](https://github.com/timower/rM-docker/blob/4b6a612941cc29adc7ca23c1da38e641655d2ed2/Dockerfile)
- [rM-docker QEMU command](https://github.com/timower/rM-docker/blob/4b6a612941cc29adc7ca23c1da38e641655d2ed2/bin/run_vm)
- [rM-docker xochitl/X11 framebuffer launch](https://github.com/timower/rM-docker/blob/4b6a612941cc29adc7ca23c1da38e641655d2ed2/bin/run_xochitl)
- [run-in-reMarkable-action at `3ae4f02`](https://github.com/Eeems-Org/run-in-remarkable-action/blob/3ae4f0236186b0c113a320721b80edf52c03da64/action.yml)
- [remarkable2-framebuffer README at `3ce4f81`](https://github.com/ddvk/remarkable2-framebuffer/blob/3ce4f8109a146edf5602ac1e434f052659756441/README.md)
- [rm2fb emulator implementation](https://github.com/timower/rM2-stuff/blob/v0.1.2/tools/rm2fb-emu/rm2fb-emu.cpp)
