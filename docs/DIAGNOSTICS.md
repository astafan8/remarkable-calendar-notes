# Collecting device diagnostics

The collector runs on the **computer connected to the reMarkable**, not
on the tablet. Connect the tablet over USB first. The default USB address
is `10.11.99.1`; use its Wi-Fi address instead if that is how you normally
SSH to it.

The collector fetches:

- the current Calendar Notes log;
- the previous rotated log, when present;
- the device OS, installed app version, and QTFB socket status.

It then creates one ZIP or `tar.gz` archive that is easy to attach to a
GitHub issue. Calendar Notes deliberately excludes calendar text, source
URLs, credentials, and tokens from its own log.

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
