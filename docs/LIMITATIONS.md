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

Even with all of that, expect visibly more latency than xochitl's own
handwriting, especially during fast strokes. This is a platform ceiling,
not a bug in this app.

## No dedicated xochitl sidebar icon

The app is launched from **AppLoad**, which gives it an entry (with its
icon) in AppLoad's own launcher. It does *not* add an entry to xochitl's
library sidebar, and this repository ships nothing that claims to.

Adding one would mean patching xochitl's *compiled* QML resources at
runtime (the `qt-resource-rebuilder` / QMD approach). Such a patch is
addressed by internal numeric resource IDs that exist only inside one
specific firmware build, are undocumented, and change between releases —
including patch releases. They cannot be derived without decompiling a
particular firmware's `resources.rcc` on a real device and validating the
result there, and a wrong patch can silently no-op or destabilize
xochitl's UI at startup. There is no way for this project's automated
tooling to produce or verify one, so no QMD package is shipped or
proposed. This is a platform limitation, not an oversight; AppLoad's
launcher entry is the supported way in.

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

## Font: a small built-in bitmap font, not an embedded typeface

Grid chrome (day numbers, view/action button labels, event summaries) is
rendered with a compact, hand-authored 3x5 pixel bitmap font covering
uppercase letters, digits, and basic punctuation — see
`calnotes_core::render`. This keeps rendering deterministic and
dependency-/license-free, at the cost of a smaller character set than a
real typeface; lowercase text is folded to uppercase. Full-fidelity
free-form text is still captured via handwritten ink, which has no such
limitation.

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

## Vellum publication requires a human maintainer

`scripts/publish-vellum-testing.sh` prepares a PR against
`vellum-dev/vellum`; it cannot publish a package to Vellum's testing
repository on its own — that step is exclusively performed by Vellum's
own maintainers after PR review, by Vellum's design. Until that happens
the app installs from the release bundle only (see `docs/INSTALL.md`),
and the recipe's checksums stay as explicit placeholders
(`scripts/update-vellum-checksums.sh` replaces them once a release
exists).

## Network refreshes are best-effort and asynchronous

Fetching runs on a worker thread and the UI never blocks on it, which
means a refresh started by navigation may land a second or two after the
new view is already on screen. Events are fetched for a window 45 days
wider than the visible one on each side, so ordinary page-at-a-time
navigation is served from data already in memory; jumping much further
does need a fresh fetch, and the view shows what is cached until it
arrives.
