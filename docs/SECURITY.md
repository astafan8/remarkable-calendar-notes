# Security

## Threat model summary

This app runs entirely on-device (plus outbound HTTPS/CalDAV calls you
configure yourself) as a normal, non-privileged AppLoad external app.
There is no server component, no telemetry, and no third-party analytics.

## Plaintext-at-rest credentials (accepted limitation)

Google OAuth **refresh tokens**, Google OAuth **client secrets**, and
iCloud **app-specific passwords** are stored in plaintext JSON at
`~/.local/share/remarkable-calendar-notes/config.json` (or
`$REMARKABLE_CALENDAR_NOTES_DATA_DIR/config.json`).

This is a deliberate, documented trade-off, not an oversight:

- reMarkable OS has no supported per-app secret store or keychain
  equivalent available to a third-party AppLoad app.
- Encrypting the file with a key stored *on the same device* would not
  meaningfully raise the bar against anyone with filesystem access to the
  device (the same access needed to read the plaintext file today).

**Mitigations that are implemented:**

- Secrets are always **masked in the UI** (`config::mask_secret` — shows
  only the first/last character) so they can't be shoulder-surfed or
  accidentally screenshotted in full. The Google refresh token is
  stronger still: it is never rendered at all, and the source editor has
  no field for it.
- Secrets are **never logged**, in any build configuration. Access tokens
  exist only in memory for the duration of one fetch, are sent solely as
  an `Authorization: Bearer` header, and are never persisted or printed.
- Only what's strictly necessary is stored: a Google **refresh token**
  (not the password, which the app never sees at all — see the OAuth
  device flow in `docs/SOURCES.md`), and an iCloud **app-specific
  password** (not the Apple ID password itself — Apple explicitly
  designed app-specific passwords to be revocable independently).

**What you can do:** revoke a Google refresh token any time from your
[Google Account security settings](https://myaccount.google.com/permissions);
revoke an iCloud app-specific password from
[appleid.apple.com](https://appleid.apple.com). Both take effect
immediately and don't require reinstalling the app.

## Network

- All calendar network I/O uses **pure-Rust TLS** (`rustls`, via `ureq`'s
  `tls` feature) with **bundled root certificates** (`webpki-roots`) —
  no OpenSSL, no reliance on the device's system certificate store. This
  also keeps cross-compiling to armv7 simple (no C TLS library to cross
  build).
- HTTPS `.ics` URLs and iCloud CalDAV URLs are validated to start with
  `https://` before any request is made; plain `http://` URLs are
  rejected outright (see `sources::https_ics`/`sources::caldav`).
- There is no certificate pinning — standard WebPKI validation only.

## Data handled

| Data | Where | Sensitivity |
|---|---|---|
| Calendar event summaries/locations/times | `cache/<source>.json` | Whatever your calendar contains |
| Handwritten notes | `ink.json` | Whatever you write |
| Source configuration (URLs, IDs, secrets) | `config.json` | See "Plaintext-at-rest" above |

None of the above ever leaves the device except the outbound
requests you configure (to the HTTPS URL, Google's API, or your iCloud
CalDAV server) — there is no other network destination.

## Reporting a vulnerability

See [`SECURITY.md`](../SECURITY.md) at the repository root.
