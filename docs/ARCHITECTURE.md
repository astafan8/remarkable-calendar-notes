# Architecture

## Crate layout

```
crates/
  calnotes-core/    Platform-independent logic. No device dependency —
                     builds and tests run on Windows/Linux/macOS.
      model.rs       Event, CalendarSource, AppConfig, ViewMode
      ics.rs         RFC 5545 line unfolding, escaping, DATE/DATE-TIME parsing
      recurrence.rs  Bounded RRULE expansion (DAILY/WEEKLY/MONTHLY/YEARLY)
      ink.rs         Normalized handwritten strokes, undo/clear-day
      view.rs        Pure Day/Week/WorkWeek/TwoWeeks/Month grid geometry
      render.rs      RGB565 software framebuffer + tiny bitmap font
      vkb.rs         AppLoad virtual-keyboard key decoding + text fields
      persistence.rs Atomic JSON read/write, data-dir resolution
      config.rs      AppState (config + ink) load/save, secret masking
      timeutil.rs    Fixed UTC-offset wall-clock handling
      sources/       local_ics, https_ics, google (OAuth device flow),
                     caldav (iCloud), cache (offline fallback)

  calnotes-device/  QTFB protocol client.
      protocol.rs    Pure wire encoding/decoding (message layout, the
                      QTFB_KEY framebuffer key, input-event fields) —
                      no platform dependency, tested on every host
      qtfb.rs        `cfg(unix)`-gated SOCK_SEQPACKET client and
                      shared-memory framebuffer mapping

  calnotes-app/     The `remarkable-calendar-notes` binary.
      app.rs         Screen/navigation state, source editor, background
                      refresh + Google login workers, rendering —
                      built only on calnotes-core, fully unit-testable
      display.rs     What to publish to the screen and which rectangle to
                      refresh (full redraw vs incremental pen segment),
                      behind a `FrameSink` trait so it is tested with an
                      in-memory sink on any platform
      main.rs        CLI (`run` / `preview`) + the Unix-only device event
                      loop that wires calnotes-device into app.rs

xtask/              `cargo run -p xtask -- icon` regenerates
                    assets/icon.png; its tests also check the packaging
                    metadata's shape (Vellum recipe, release workflow)
```

## Why this split

`calnotes-core` has zero reMarkable/QTFB dependency on purpose: every
piece of logic worth testing (ICS parsing, recurrence, view layout, ink
normalization, source serialization) is testable on a normal development
machine and in CI without any device or emulator. `calnotes-device` is
the only crate that touches `/tmp/qtfb.sock`, and it's small enough that
its correctness can be reasoned about directly against the protocol
(see below) rather than through end-to-end testing.

## QTFB protocol

AppLoad's "QTFB" IPC is a fixed, documented protocol: a `SOCK_SEQPACKET`
Unix socket at `/tmp/qtfb.sock`, plus a POSIX shared-memory framebuffer.
`calnotes-device` implements it from scratch against that protocol
description (message layout, byte offsets, and numeric constants are
factual/necessary details, not creative expression copied from any
particular client). Message encoding/decoding lives in `protocol.rs`,
which has no platform dependency and is unit-tested on every host; only
the socket/mmap plumbing in `qtfb.rs` is Unix-only.

Three details worth highlighting for anyone extending this:

- **The framebuffer key is not a constant.** AppLoad passes it to each
  launched external app in the `QTFB_KEY` environment variable, and the
  host's initialize reply reports the key it actually bound the
  connection to (`shmKeyDefined`). That reported key — not a guessed or
  hardcoded one — names the `/dev/shm/qtfb_<key>` object this client
  maps.
- **The reMarkable 2 is a 32-bit `armv7` device**, where C's `size_t` is
  4 bytes. A client built by copying byte offsets from a reMarkable Paper
  Pro (aarch64, 64-bit) client will silently misread the server's reply.
  `protocol.rs` computes the 32-bit offsets explicitly and documents why.
- **Input field meanings depend on the event kind.** A virtual-keyboard
  key code arrives in the event's `x` field (not `d`), and pen pressure
  arrives in `d` as a `0..=100` percentage. `InputEvent::vkb_key_code`
  and `InputEvent::pen_pressure` exist so those two easy-to-swap
  conventions are stated once, in one place.

Input events (touch, pen, and AppLoad's virtual keyboard) all arrive over
the same socket, already tagged by kind (`INPUT_TOUCH_*` vs `INPUT_PEN_*`
vs `INPUT_VKB_*`). That tagging — not any raw `/dev/input` grabbing — is
what keeps pen input from interfering with touch navigation: `app.rs`
routes each kind to a different handler and never reads raw evdev itself
(this app is a normal windowed AppLoad app, not a full-screen "takeover"
app, so it deliberately doesn't grab input devices).

## Drawing: one framebuffer, incremental pen updates

The device loop owns exactly one `FrameBuffer` for the whole run.

- **Full re-render** (`App::render_into` + `request_full_update`) happens
  only when the whole screen can change: startup, navigation, view/UI
  changes, and completed background refreshes.
- **Pen samples do not go through it.** `App::pen_move` records the point
  in the ink store and returns a single `PenSegment` in absolute canvas
  pixels; `display::draw_segment` draws just that line into the
  framebuffer that already holds the screen, copies only the pixels
  inside the segment's dirty rectangle into the sink
  (`FrameBuffer::write_rect_rgb565_into`), and requests a partial refresh
  of that rectangle. No per-sample allocation, no full-frame copy, no
  full re-render.

Both paths sit behind the `FrameSink` trait, so the device supplies QTFB
shared memory and tests supply a byte buffer that records exactly which
updates were requested.

Because the incremental segments are computed from the same
normalize/denormalize round trip the full renderer uses, live ink is
pixel-identical to what a later full re-render of the persisted strokes
produces — there is a test asserting exactly that.

## Background work

Network I/O never runs on the event loop. `App::start_refresh` clones the
enabled sources onto a worker thread and returns immediately;
`App::poll_background` applies the result (statuses, events, cached
window) on the next tick via a non-blocking `try_recv`. The Google OAuth
device flow works the same way: a worker requests the device code and
polls until the user approves, streaming phase updates back to the UI so
the verification URL/code can be displayed while input stays responsive.

Events are fetched for a window 45 days wider than the visible one on
each side, so page-at-a-time navigation is served from memory rather than
leaving a moved view blank until the network answers.

## Rendering

`calnotes-core::render::FrameBuffer` is a grayscale-only RGB565 pixel
buffer with basic primitives (rects, lines, a 3x5 bitmap font). The exact
same drawing code:

- renders to QTFB's shared memory on-device (`calnotes-device` + the
  `run` subcommand), and
- renders to a deterministic `.ppm` file via the `preview` subcommand, on
  any platform, for development/CI without a device.

Ink strokes are stored normalized `[0,1]` within whichever date-cell
they were drawn on (see `ink.rs` / `view::normalize_within` /
`view::denormalize_within`), so the same stored points are correctly
redrawn regardless of which view (and therefore which pixel-size cell)
is currently displaying that date.

## Persistence

All state — `config.json` (sources, view mode, UTC offset), `ink.json`
(handwritten notes), and `cache/<source-id>.json` (offline event cache)
— lives under `~/.local/share/remarkable-calendar-notes`, or
`$REMARKABLE_CALENDAR_NOTES_DATA_DIR` if set. Every write is atomic
(write to a sibling temp file, `fsync`, `rename`) — see
`persistence.rs`.
