# Install

## Current: release bundle (manual)

This app is **not published to Vellum yet**, so there is no `vellum add`
one-liner to run. Install the release bundle by hand:

1. Download the latest `remarkable-calendar-notes-<version>-armv7.zip`
   from this repository's [Releases](../../releases) page (built by
   `.github/workflows/release.yml`), and verify its checksum against the
   accompanying `.sha256`/`.sha512` file.
2. Extract it; you'll get a `remarkable-calendar-notes/` folder containing
   the binary, `icon.png`, and `external.manifest.json`.
3. Copy that folder to `/home/root/xovi/exthome/appload/` on the device
   (e.g. `scp -r remarkable-calendar-notes root@10.11.99.1:/home/root/xovi/exthome/appload/`),
   assuming AppLoad (via XOVI) is already installed.
4. Restart AppLoad (or the device). "Calendar Notes" appears in AppLoad's
   app list with the icon from step 2.

## Future: Vellum

`vellum/packages/remarkable-calendar-notes/VELBUILD` in this repository is
the package recipe intended for
[vellum-dev/vellum](https://github.com/vellum-dev/vellum). It is **not
installable yet**: its checksums are explicit placeholders until a release
is published (`scripts/update-vellum-checksums.sh` fills them in), and
Vellum publication additionally requires a Vellum maintainer to review and
merge a PR, then choose to publish to their testing repository. See
`scripts/publish-vellum-testing.sh` and
[DEVELOPMENT.md](DEVELOPMENT.md).

Once that has happened, the install becomes:

```sh
vellum add remarkable-calendar-notes    # only after upstream acceptance
```

with `vellum del` keeping your config/notes/cache and `vellum purge`
removing them too.

## Launching

Open **AppLoad** and pick "Calendar Notes". AppLoad is the only supported
launcher: a dedicated icon in xochitl's own library sidebar is not
offered, and cannot be supported robustly — see
[LIMITATIONS.md](LIMITATIONS.md#no-dedicated-xochitl-sidebar-icon).

## Requirements

- reMarkable 2, reMarkable OS 3.26–3.27.x (see
  [FIRMWARE_COMPATIBILITY.md](FIRMWARE_COMPATIBILITY.md)).
- [AppLoad](https://github.com/asivery/rm-appload) `>= 0.5.3` (itself
  distributed via Vellum or XOVI).
