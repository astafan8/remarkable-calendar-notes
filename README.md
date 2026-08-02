# remarkable-calendar-notes

A read-only calendar app for the **reMarkable 2**, with persistent
handwritten notes attached to individual dates. Runs as an [AppLoad](https://github.com/asivery/rm-appload)
external QTFB app — no jailbreak/takeover mode required.

- **Views:** Day, Week, Work Week, Two Weeks, Month, with `PREV`/`TODAY`/
  `NEXT` navigation; `TODAY` always jumps to the real current date at your
  configured UTC offset.
- **Notes:** write on any date in any view with the pen; notes are stored
  normalized to that date's grid cell, so the same handwriting renders
  correctly in every view and survives navigation/restart. Each pen sample
  draws only its newest stroke segment and refreshes only the few pixels
  it touched. Undo and clear-day controls included.
- **Calendar sources**, all configured in-app (no config files to hand
  edit): a local `.ics` file, an arbitrary HTTPS `.ics` URL, Google
  Calendar (OAuth device flow, with an in-app `LOG IN` action), and iCloud
  (CalDAV + app-specific password). Fetching runs on a worker thread, so
  the UI stays responsive; each source caches its last successful fetch
  offline and shows its own status/errors without taking down the rest of
  the app.
- **ICS parsing** handles folded lines, text escaping, `DATE`/`DATE-TIME`
  values, and `RRULE` recurrence (`DAILY`/`WEEKLY`/`MONTHLY`/`YEARLY`,
  `INTERVAL`, `COUNT`, `UNTIL`, `BYDAY`) over a bounded display window.

Requires reMarkable OS **3.26–3.27.x** (see
[docs/FIRMWARE_COMPATIBILITY.md](docs/FIRMWARE_COMPATIBILITY.md)).

## Install

The current install is manual because the app is not in Vellum's package
feed. In short:

1. Connect the rM2 by USB and SSH to `root@10.11.99.1`.
2. Install Vellum, then run `vellum add appload tripletap` and
   `xovi/rebuild_hashtable`.
3. Download and extract the latest ARMv7 bundle from the public
   [release repository](https://github.com/astafan8/remarkable-calendar-notes-releases/releases).
4. From the computer, copy the extracted folder:

   ```sh
   scp -r remarkable-calendar-notes root@10.11.99.1:/home/root/xovi/exthome/appload/
   ```

5. Triple-press the tablet's power button to start XOVI, open
   **AppLoad**, tap **Reload**, then open **Calendar Notes**.

The complete first-time instructions—including how to find the SSH
password, install Vellum safely, verify the release checksum, and recover
after an OS update—are in [docs/INSTALL.md](docs/INSTALL.md).

The source is now public, but `vellum add remarkable-calendar-notes` is
still not available until the repository owner personally reviews and
submits the package and Vellum's maintainers accept it. Manual AppLoad
installation remains the current path. A dedicated xochitl sidebar icon
is not supported (see [docs/LIMITATIONS.md](docs/LIMITATIONS.md)).

## In-app setup

All configuration — calendar sources, the fixed UTC offset, the active
view — is done on-device via the settings screen and AppLoad's virtual
keyboard. See [docs/SOURCES.md](docs/SOURCES.md).

## Documentation

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — crate layout, QTFB
  protocol notes, rendering pipeline.
- [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) — building, testing, the
  desktop `preview` command, releasing.
- [docs/SOURCES.md](docs/SOURCES.md) — configuring calendar sources
  in-app.
- [docs/SECURITY.md](docs/SECURITY.md) — the plaintext-at-rest credential
  limitation and other honest trade-offs.
- [docs/FIRMWARE_COMPATIBILITY.md](docs/FIRMWARE_COMPATIBILITY.md) —
  supported OS/device versions.
- [docs/LIMITATIONS.md](docs/LIMITATIONS.md) — pen latency, timezone
  handling, and other known limitations.

## License

[MIT](LICENSE). See [SECURITY.md](SECURITY.md) for the security policy
and [CONTRIBUTING.md](CONTRIBUTING.md) for how to contribute.
