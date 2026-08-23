# Pen input on reMarkable 2: research & recommendations

This is the "do the thorough research" write-up you asked for: what
approaches the reMarkable community has used to get responsive pen/ink,
which of them apply to an **AppLoad external app** like this one, what we
already do, and what is left to try (ranked). It is deliberately short —
pick the options you want and I'll implement them.

## 1. The pipeline (why latency exists at all)

Drawing ink on an rM2 goes through several layers. Latency is the sum of
all of them:

```
Wacom digitizer  →  /dev/input/event*  →  [app reads samples]
        →  app rasterizes into a framebuffer
        →  framebuffer handed to the display stack
        →  EPDC (e-ink controller) picks a waveform and refreshes pixels
```

xochitl (reMarkable's own app) owns this whole stack and talks to the
EPDC directly with tuned waveforms. **Any** third-party app gives up some
of that. An AppLoad app gives up more, because it does not touch the
framebuffer hardware at all — it renders into a shared-memory image and
asks the AppLoad/QTFB host to push it to the panel.

## 2. Approaches surveyed

Sources: `reHackable/awesome-reMarkable`, `rmkit-dev/rmkit` (harmony),
`canselcik/libremarkable`, `ddvk/remarkable2-framebuffer` (rm2fb),
`asivery/rm-appload` (QTFB).

| # | Approach | What it gives | Applies to us? |
|---|----------|---------------|----------------|
| A | **Read `/dev/input` digitizer directly** (harmony, libremarkable) | Every hardware sample, no compositor coalescing — fixes dropped/short strokes | **Yes — already done** (`calnotes_device::wacom`) |
| B | **Partial EPDC refresh with a fast waveform** (libremarkable `mxcfb`, rm2fb `MODE_*`) | Low-latency mono updates for ink vs. slow high-quality refresh | **Indirect** — an AppLoad app cannot call `mxcfb` ioctls; the QTFB host chooses the waveform. We can only pick *what* and *how often* to send. |
| C | **Batch/coalesce our own updates** (harmony) | Avoid flooding the display stack; publish one dirty rect per burst | **Yes — done**, now also **rate-limited** (`pen_refresh_ms`) |
| D | **Defer heavy redraws while drawing** | A full-screen redraw never blocks the stroke loop | **Yes — done** this release |
| E | **Direct rm2fb client** (`ddvk/remarkable2-framebuffer`) | Bypass QTFB, talk to rm2fb's shared framebuffer + its refresh modes, including a fast A2/DU-like mono mode | **Possible but large** — would mean running as an rm2fb client instead of/alongside AppLoad QTFB; see recommendations |
| F | **Predict/interpolate the pen path** (many sketch apps) | Perceived latency drops by drawing slightly ahead of the last confirmed sample | **Yes, app-side** — no platform access needed |
| G | **Reduce per-sample work** | Less CPU per sample = lower latency | **Yes — done**: no per-sample layout, single Bresenham segment, no AA |

## 3. What this app already does

- **A** raw digitizer read (never grabbed; auto-falls back to QTFB pen).
- **G** zero per-sample layout; one small segment blitted per sample.
- **C** one partial update per poll-cycle burst, **now throttled** to
  `pen_refresh_ms` (default 12 ms) so the display host is never flooded.
- **D** background/auto/startup redraws deferred while the pen is down.
- Adaptive 2 ms/16 ms poll so we drain the digitizer fast while writing.

## 4. Recommendations, ranked

**R1 — Tune `pen_refresh_ms` on the device (no code needed).**
The throttle is exposed in **Settings → Display → PEN**. Try 8–16 ms.
Lower feels more immediate but risks the flooding stall again; higher
batches more. This tells us the real rate the QTFB host on your firmware
can sustain, which informs everything below.

**R2 — Pen-path prediction (F).** Cheap, app-only, no platform risk.
Draw a short predicted segment past the last confirmed sample and correct
it when the next real sample lands. Typically the single biggest
*perceived* latency win after the pipeline itself. Low risk; opt-in
setting. **Recommended next step.**

**R3 — A dedicated "mono/fast" publish path (B, within QTFB limits).**
Investigate whether the QTFB protocol exposes a faster refresh
mode/waveform hint for ink rectangles vs. the general UI. If it does, use
it only for pen dirty-rects. If it does not, R3 is a no-op and we skip it.
Medium effort, medium payoff, no risk if it's just a flag.

**R4 — Evaluate an rm2fb-client build (E).** The largest change: talk to
`ddvk/remarkable2-framebuffer` directly, which supports fast mono refresh
modes designed for exactly this. Upside: closest a third-party app can
get to xochitl-like ink latency. Downside: a second display backend to
build, package, and support; only makes sense if R1–R3 aren't enough.
**Only if you want to push further after trying R1–R3.**

## 5. Honest ceiling

Even with all of the above, an AppLoad app will not match xochitl's
handwriting feel — xochitl owns the EPDC and its waveforms and we never
will from user space. The goal here is "as good as a well-written
third-party sketch app (harmony-class)", not parity with the built-in
notebook. R1 + R2 are the realistic path to that; R3/R4 are there if you
want to chase the last bit.
