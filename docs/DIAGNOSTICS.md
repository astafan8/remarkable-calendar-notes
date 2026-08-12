# Collecting device diagnostics

## What the app shows and records on failure

Calendar Notes is designed so that a failure never leaves a silently blank
window:

- **On-screen error.** Any startup or runtime failure (including an
  internal panic) paints a full-screen error page with a short description
  **and the full path to the log file**, so you can find and share it.
- **Persistent log.** Every run appends to a log file (path shown on that
  error page, and reported by the collector below). The log records state
  loading, QTFB connection, render results, background refresh, and panic
  details — but never calendar text, source URLs, or credentials.
- **Self-healing stored settings.** A corrupt or incompatible
  `config.json`/`ink.json` (for example from an interrupted write or an
  older/newer version) is **automatically moved aside** to
  `<name>.corrupt-<timestamp>` and reset to defaults instead of blocking
  startup. The app opens normally and notes the reset in the log. Your
  moved-aside file is preserved next to it if you want to inspect it.

If the app opens but behaves as if freshly installed, a stored settings
file was likely reset — check the log for a `state warning:` line.

## If the screen is blank and there is NO log at all

The app writes its log *before* it connects to the display, so **a blank
screen with no log file means the binary never started** — either the
device could not execute it (for example after a firmware update changed
the system C library / dynamic loader, which also wipes XOVI/AppLoad), or
AppLoad never launched it. The regular log collectors cannot help here
because there is nothing to collect.

Run the on-device diagnostic instead. It executes the binary directly and
prints a full report to your terminal — copy the output and share it:

```sh
ssh root@10.11.99.1 'sh -s' < diagnose-on-device.sh
```

`diagnose-on-device.sh` ships in the diagnostics bundle (and lives in
`scripts/`). The key line is the direct `--help` run: if that prints usage
the binary works and the problem is the AppLoad/QTFB launch; if it crashes
or reports a missing loader/library, the binary cannot run on this device.

Since v0.1.10 the release binary is **fully statically linked** (musl), so
it has no dependency on the device's C library or dynamic loader and keeps
running across firmware updates — the most common cause of this failure.

## Collecting the log

The collector runs on the **computer connected to the reMarkable**, not
on the tablet. Connect the tablet over USB first. The default USB address
is `10.11.99.1`; use its Wi-Fi address instead if that is how you normally
SSH to it.

The collector uses one SSH session, so the tablet password is requested
only once. It fetches:

- the current Calendar Notes log, wherever AppLoad's environment placed it;
- the previous rotated log, when present;
- binary presence and executable permissions;
- the device OS, installed app version, and QTFB socket status;
- filtered AppLoad/xochitl launch errors.

It creates one `tar.gz` archive that is easy to attach to a GitHub issue.
If no application log exists, collection still succeeds and explains the
pre-start failure in `device-info.txt` and `appload-xochitl.log`.
Calendar Notes deliberately excludes calendar text, source URLs,
credentials, and tokens from its own log.

## Windows computer

Open PowerShell in the extracted diagnostics bundle or repository:

```powershell
.\collect-device-log.ps1
```

When running from the repository, use:

```powershell
.\scripts\collect-device-log.ps1
```

For a Wi-Fi address:

```powershell
.\collect-device-log.ps1 -Device 192.168.1.50
```

## Linux or macOS computer

Open a terminal in the extracted diagnostics bundle or repository:

```sh
chmod +x collect-device-log.sh
./collect-device-log.sh
```

When running from the repository, use:

```sh
./scripts/collect-device-log.sh
```

For a Wi-Fi address:

```sh
./collect-device-log.sh --device 192.168.1.50
```

## No script

The essential log can always be copied directly from the computer:

```sh
scp root@10.11.99.1:/home/root/.local/share/remarkable-calendar-notes/calendar-notes.log .
```

If the app reports `LOG UNAVAILABLE`, try its temporary fallback:

```sh
scp root@10.11.99.1:/tmp/calendar-notes.log .
```

Both collectors accept an optional system-log flag
(`-IncludeSystemLog` or `--include-system-log`). That adds the last ten
minutes of xochitl logs. Those logs may contain unrelated device details,
so review them before sharing.

## Repairing installation permissions

The diagnostics bundle also contains a one-password installer. It sends
the official release ZIP directly to the tablet, installs it in the
correct AppLoad/XOVI paths, and explicitly restores executable permission:

```powershell
.\install-device.ps1 -Bundle .\remarkable-calendar-notes-<version>-armv7.zip
```

```sh
./install-device.sh --bundle remarkable-calendar-notes-<version>-armv7.zip
```

The same installer accepts the `-xovi-sidebar.zip` bundle and installs
both the QMD launcher and its Qt icon resource before rebuilding the
device hashtable.
