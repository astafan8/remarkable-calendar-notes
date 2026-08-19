# Known limitations

Honest documentation of trade-offs this app makes, and why.

## Pen latency: QTFB is not xochitl's native pipeline

xochitl (reMarkable's own note-taking app) draws ink through a dedicated,
hardware-accelerated pipeline with direct e-ink waveform control. A
windowed AppLoad "external QTFB" app — which is what this app is — has no
access to that pipeline. It receives pen samples over a socket, draws
into a shared-memory RGB565 buffer in software, and asks the QTFB host to
refresh a region of the physical display. Each of those steps costs time
xochitl's native pipeline doesn't pay.

**What this app does to reduce that gap, without pretending to close
it:**

- Each pen sample draws **only the newest stroke segment** into a
  framebuffer the event loop keeps alive across frames — the whole screen
  is never re-rendered, re-allocated, or re-copied for a pen sample (see
  `App::pen_move`, which returns just that segment, and
  `display::draw_segment`).
- Only the pixels that segment touched are copied into QTFB's shared
  memory, and only that rectangle is refreshed
  (`display::draw_segment` → `FrameBuffer::write_rect_rgb565_into` +
  `QtfbClient::request_partial_update`). A typical segment dirties a few
  hundred pixels instead of 1404x1872 — asserted by a test, which fails
  if any byte outside the published rectangle changes.
- Full re-renders happen only when the whole screen can change: pen
  release is not one of them — navigation, view/UI changes, and completed
  background refreshes are.
- The software rasterizer (`calnotes_core::render`) is deliberately
  simple — a Bresenham line with a square brush, no anti-aliasing, no
  blending — specifically to keep the per-sample cost low and
  predictable.
- A pen sample does **zero** per-sample layout work: the cell it writes
  in is resolved once at pen-down and cached on the active gesture, so
  `App::pen_move` never rebuilds the grid or searches cells — it only
  normalizes the point, records it, and returns one segment.
- The device event loop polls the QTFB socket **adaptively**: roughly
  every 2 ms while a pen or finger is on the glass (or a burst of events
  just arrived), falling back to ~60 Hz when idle to spare the battery.

**Why the first arc of a fast letter used to look straight.** QTFB does
not forward every digitizer sample; it coalesces (and can drop) pen moves
between reads, more so under load. If the socket is only drained every
~16 ms, the first move after contact can already be a long way from where
the pen touched down, so the first drawn segment is one long straight line
before the curve catches up. Draining the socket every ~2 ms while writing
gives QTFB far less time to coalesce, so many more of the pen's
high-frequency points survive and curves keep their shape from the very
start. We cannot exceed what QTFB delivers — an AppLoad app has no direct
access to the Wacom `/dev/input` digitizer — but polling aggressively
captures as much of it as the platform allows.

Even with all of that, expect visibly more latency than xochitl's own
handwriting, especially during fast strokes. This is a platform ceiling,
not a bug in this app.

## The optional sidebar launcher patches xochitl

The standard app is launched from **AppLoad** and does not modify
xochitl. An optional OS 3.27-only QMLDiff companion adds a real Calendar
Notes sidebar icon and calls AppLoad's public launcher API.

That convenience has a larger failure surface: QMLDiff runs inside
xochitl, and a future firmware can change the sidebar's QML structure.
The numeric tokens in a hashed QMD are stable hashes resolved through a
device-generated hashtab; the fragile part is the surrounding QML
structure, not arbitrary per-build IDs. The companion is therefore pinned
to `remarkable-os >=3.27,<3.28` and must be revalidated for every new OS
minor. Re-run `xovi/rebuild_hashtable` after OS or
qt-resource-rebuilder updates.

If a bad or stale patch prevents the interface from loading, remove
`calendarNotesSidebar.qmd` and `calendarNotesSidebar.rcc` over SSH and
rebuild the hashtab. The sidebar
button still launches the same external QTFB process, so it does not
improve pen latency or provide native xochitl drawing tools.

## Timezones: a single fixed UTC offset, not a timezone database

The app asks once for a fixed UTC offset (e.g. `-05:00`) and uses it for
all "what date/time is this" math — see `calnotes_core::timeutil`. It
does **not** ship or consult an IANA timezone database, and does **not**
automatically follow daylight-saving-time transitions. If your region's
UTC offset changes (DST), update the setting manually.

This is a deliberate simplification: a full tzdata port adds real
maintenance burden (databases need periodic updates as governments change
DST rules) for a personal calendar app on a single-purpose device where
the user already knows their own offset.

Values that carry an **explicit** UTC marker *are* converted with that
offset, and the conversion can legitimately move an event onto a
different calendar date:

- ICS `DATE-TIME` values ending in `Z` (`recurrence::build_events` +
  `UtcOffset::utc_naive_to_local`).
- Google Calendar RFC 3339 `dateTime` values with a `Z` or `±HH:MM`
  suffix (`sources::google::parse_rfc3339_to_local`).

Values with **no** offset marker — floating times, and times carrying
only a `TZID` parameter — are taken as already being local wall-clock
time, because resolving a `TZID` would require the timezone database this
app deliberately does not ship. An event in a timezone other than your
configured offset, published with `TZID` rather than UTC, will therefore
display at its originating wall-clock time.

## ICS parsing: pragmatic, not exhaustive RFC 5545 coverage

Supported: line folding/unfolding, `TEXT` escaping, `DATE`/`DATE-TIME`
values (including `Z`-suffixed UTC and floating/`TZID` local times,
treated as local per the timezone note above), and `RRULE` recurrence
(`DAILY`/`WEEKLY`/`MONTHLY`/`YEARLY` with `INTERVAL`/`COUNT`/`UNTIL`/
`BYDAY`, including ordinal `BYDAY` like `-1FR`) plus `EXDATE`.

Not supported: `RDATE`, `EXRULE`, `VTIMEZONE` resolution against a real
tzdata, `VTODO`/`VJOURNAL`/`VALARM` components, and `BYMONTH`/`BYSETPOS`/
`BYWEEKNO`/etc. `RRULE` parts. Malformed individual `VEVENT`s are skipped
(recorded, not silently dropped) rather than failing the whole calendar.

## Recurrence expansion is bounded by design

An `RRULE` with no `COUNT`/`UNTIL` (or one far in the future) is capped
at 2000 generated instances and always bounded to the current view's
display window (see `recurrence::MAX_INSTANCES`,
`view::window_for`) — this is intentional, not a bug: an unbounded rule
combined with an old `DTSTART` must never be able to hang the UI or
exhaust memory on a resource-constrained e-reader.

## Fonts: a built-in bitmap font, plus JetBrains Mono in the settings menu

The calendar chrome (day numbers, view/action button labels, month names,
event summaries) is rendered with a compact, hand-authored 3x5 pixel
bitmap font — see `calnotes_core::render` — for a crisp, deterministic
look; that font is uppercase-only and covers digits and basic
punctuation. The settings ("SET") menu instead uses an embedded Latin
subset of **JetBrains Mono** (SIL OFL), rasterized with the pure-Rust
`ab_glyph` crate, so emails, Apple IDs and passwords render in real
mixed-case, monospaced glyphs (including `@`, `.`, `/`, `_`, …). Neither
path depends on Qt or a system font, so the binary still links statically.
Full-fidelity free-form notes are captured via handwritten ink, which has
no character-set limitation.

## AppLoad cannot let an external app open its keyboard

AppLoad 0.5.3 owns the virtual keyboard and exposes its key events to
QTFB applications, but exposes no application-to-host command for opening
it. Calendar Notes focuses a field immediately when tapped and makes the
active field/cursor prominent, but AppLoad's keyboard button must still
be tapped once in the window chrome. The app deliberately does not draw a
second, incompatible keyboard of its own. Backspace uses the actual
`0x80` code emitted by AppLoad's bundled keyboard layout; Delete uses
`0x7f`.

## Device-only code paths cannot be tested in CI

Everything above the socket is tested on ordinary machines: the QTFB
message encoding/decoding (`calnotes-device::protocol`), the decision of
what to publish and refresh (`calnotes-app::display`, exercised through an
in-memory frame sink), and all calendar/ink/view logic.

What no automated test here can cover is the parts that need real
hardware: connecting to `/tmp/qtfb.sock`, mapping `/dev/shm/qtfb_<key>`,
and the actual e-ink refresh behavior and latency of a partial update.
CI compiles those paths for the real armv7 target (`.github/workflows/ci.yml`),
which catches type/ABI mistakes but not runtime ones. Treat on-device
behavior as verified by running it, not by a green CI badge.

## Vellum review is still pending

The source and release artifacts are public, but that does not
automatically place the app in Vellum. Its maintainers require the
repository owner—not an automation operator—to review the VELBUILD,
personally open and describe the contribution, and respond to review.

Until that contribution is accepted and published to Vellum's testing
feed, install the public release bundle manually as described in
`docs/INSTALL.md`.

## Network refreshes are best-effort and asynchronous

Fetching runs on a worker thread and the UI never blocks on it, which
means a refresh started by navigation may land a second or two after the
new view is already on screen. Events are fetched for a window 45 days
wider than the visible one on each side, so ordinary page-at-a-time
navigation is served from data already in memory; jumping much further
does need a fresh fetch, and the view shows what is cached until it
arrives.
