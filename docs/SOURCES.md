# Configuring calendar sources (in-app)

There are no config files to hand-edit. Everything below is done from the
in-app **Settings** screen (tap **SET** in the toolbar), using AppLoad's
virtual keyboard for any text field.

## Settings screen

- **BACK** — return to the calendar.
- **REFRESH** — start a fetch from every enabled source now. It runs on a
  background thread (the app stays usable and a status line reports
  progress) and also happens automatically every 15 minutes while the app
  is open, and whenever you navigate outside the already-fetched date
  range.
- Each configured source shows its label, **TEST**, an **ON/OFF** toggle,
  and **DEL**. **TEST** makes a real connection/fetch attempt and updates
  that row with `OK ... EVENTS` or the returned error. Tapping the large
  source field opens it for editing. Google sources additionally show a
  **LOG IN** button.
- Four **+ ...** buttons add a new source of each kind.
- While editing, tap a large bordered field to focus it; the double border
  and `|` cursor show exactly what is being edited. **Tab** also moves
  focus to the next field. **SAVE** commits, **CANCEL** discards. Saving
  an edit keeps the source's
  enabled/disabled state, its last sync status, and — for Google — a
  refresh token from a previous login.

## Source kinds

### Local `.ics` file

Enter a filesystem path. Useful if you sync an `.ics` file onto the
device yourself (e.g. via `scp`/`rsync` to a directory your own workflow
controls).

### HTTPS `.ics` URL

Any URL serving a plain `.ics` document over HTTPS (HTTP is rejected —
see `docs/SECURITY.md`). Many calendar providers (including Google and
iCloud) can also publish a plain "secret address" `.ics` URL if you don't
want to use their dedicated integration below.

### Google Calendar

Uses OAuth 2.0's **device authorization grant** (RFC 8628) — no password
ever touches this app:

1. In the [Google Cloud Console](https://console.cloud.google.com/),
   create an OAuth client of type **TV and Limited Input devices** (any
   Google account can do this; it's free). Note the **Client ID** and
   **Client Secret**.
2. In the app, add a Google Calendar source, enter that Client
   ID/Secret and the calendar ID (`primary` for your main calendar, or a
   specific calendar's ID from Google Calendar's settings), and **SAVE**.
3. Tap **LOG IN** on that source's row. The app shows a verification URL
   (`google.com/device`) and a short user code, and keeps polling in the
   background — the screen stays responsive while you complete the next
   step, and you can leave it open.
4. Open that URL on *any other* device (phone, laptop), sign in, and
   enter the code.
5. Once approved, a refresh token is stored (never displayed, plaintext
   on disk — see `docs/SECURITY.md`) and the source is refreshed
   immediately. This only needs to happen once; editing the source later
   does not discard the token. If the login fails or times out, the
   failure is shown on the settings screen and **LOG IN** can be tapped
   again.

### iCloud (CalDAV)

Apple requires an **app-specific password** for third-party CalDAV
clients — your normal Apple ID password will not work here.

1. Generate one at [appleid.apple.com](https://appleid.apple.com) →
   Sign-In and Security → App-Specific Passwords.
2. You'll also need your calendar's CalDAV collection URL. iCloud doesn't
   expose this in a simple settings page; it's typically found via your
   CalDAV client's account discovery, or by using iCloud's public
   `https://caldav.icloud.com/<uid>/calendars/<calendar-id>/` pattern
   once you know your account's DSID — consult your existing CalDAV
   client's account details if you have one set up already.
3. Enter your Apple ID, the app-specific password, and that URL.

## Time zone

All times are shown using a single, user-configurable **fixed UTC
offset** (no daylight-saving-time transitions, no per-event timezone
database) — set it once for wherever you are. Timestamps that carry an
explicit UTC marker (ICS `...Z` values, Google RFC 3339 offsets) are
converted into that offset; floating/`TZID` values are shown as-is. See
`docs/LIMITATIONS.md` for the full rules and why.
