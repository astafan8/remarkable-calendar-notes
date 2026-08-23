//! `remarkable-calendar-notes` binary entry point.
//!
//! Two modes:
//! - `run` (default, Unix-only): connects to the AppLoad QTFB host and
//!   drives the real on-device event loop.
//! - `preview`: renders the current calendar screen to a `.ppm` file using
//!   nothing but `calnotes-core`, so it works identically on Windows,
//!   Linux, and macOS for development and CI screenshots — no device, no
//!   QTFB socket required.

mod app;
mod diagnostics;
mod display;

use app::App;
use std::env;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("preview") => run_preview(&args[1..]),
        Some("fetch-debug") => run_fetch_debug(&args[1..]),
        Some("run") | None => {
            #[cfg(unix)]
            diagnostics::write_start_marker();
            let log_path = diagnostics::init();
            diagnostics::log(format_args!(
                "diagnostic log: {}",
                log_path
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "unavailable".to_string())
            ));
            diagnostics::log(format_args!("arguments: {args:?}"));
            run_device()
        }
        Some("--help") | Some("-h") => {
            print_usage();
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("unknown subcommand: {other}");
            print_usage();
            ExitCode::FAILURE
        }
    }
}

fn print_usage() {
    eprintln!(
        "remarkable-calendar-notes\n\n\
         Usage:\n  \
         remarkable-calendar-notes run                 Connect to the AppLoad QTFB host (device only)\n  \
         remarkable-calendar-notes preview [OPTIONS]   Render the current screen to a .ppm file\n\n\
         preview options:\n  \
         --out <path>       Output file (default: preview.ppm)\n  \
         --view <mode>      day | week | workweek | twoweeks | month | twomonths\n  \
         --refresh          Fetch fresh events from enabled sources before rendering\n\n\
         remarkable-calendar-notes fetch-debug [URL]   Diagnose an HTTPS .ics fetch on this device\n  \
         (no URL)           Test every configured HTTPS .ics source using its stored URL"
    );
}

/// Diagnose an HTTPS `.ics` fetch directly on the device, using the exact
/// same HTTP/TLS code path the app uses. With a URL argument it tests that
/// URL; with none it tests every configured HTTPS source's stored URL, so a
/// corrupted/mistyped stored address is caught too.
fn run_fetch_debug(args: &[String]) -> ExitCode {
    if let Some(url) = args.iter().find(|a| !a.starts_with("--")) {
        let normalized = app::normalize_https_url(url);
        if &normalized != url {
            println!("normalized URL: {normalized}");
        }
        println!(
            "{}",
            calnotes_core::sources::https_ics::fetch_ics_report(&normalized)
        );
        return ExitCode::SUCCESS;
    }

    let app = match App::new() {
        Ok(app) => app,
        Err(e) => {
            eprintln!("could not load configuration: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut found = false;
    for source in &app.state.config.sources {
        if let calnotes_core::model::SourceKind::HttpsIcs { url } = &source.kind {
            found = true;
            println!("### source: {} (enabled={})", source.label, source.enabled);
            println!("stored URL: {url:?}");
            let normalized = app::normalize_https_url(url);
            if &normalized != url {
                println!("normalized URL: {normalized:?}");
            }
            println!(
                "{}",
                calnotes_core::sources::https_ics::fetch_ics_report(&normalized)
            );
        }
    }
    if !found {
        println!("No HTTPS .ics sources are configured. Pass a URL to test one:");
        println!("  remarkable-calendar-notes fetch-debug \"https://host/path.ics\"");
    }
    ExitCode::SUCCESS
}

fn run_preview(args: &[String]) -> ExitCode {
    let mut out_path = "preview.ppm".to_string();
    let mut view_mode: Option<calnotes_core::model::ViewMode> = None;
    let mut refresh = false;
    let mut settings = false;
    let mut settings_list = false;
    let mut demo_ink = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--out" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    out_path = v.clone();
                }
            }
            "--view" => {
                i += 1;
                view_mode = args.get(i).and_then(|v| parse_view_mode(v));
            }
            "--refresh" => refresh = true,
            "--settings" => settings = true,
            "--settings-list" => settings_list = true,
            "--demo-ink" => demo_ink = true,
            _ => {}
        }
        i += 1;
    }

    let mut app = match App::new() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("failed to load app state: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Some(mode) = view_mode {
        app.set_view_mode(mode);
    }
    if refresh {
        app.refresh_blocking();
    }
    if demo_ink {
        app.add_demo_scribbles();
    }
    if settings {
        app.show_settings_for_preview();
    }
    if settings_list {
        app.show_settings_list_for_preview();
    }
    let fb = app.render();
    if let Err(e) = std::fs::write(&out_path, fb.to_ppm()) {
        eprintln!("failed to write {out_path}: {e}");
        return ExitCode::FAILURE;
    }
    println!("wrote {out_path}");
    ExitCode::SUCCESS
}

fn parse_view_mode(s: &str) -> Option<calnotes_core::model::ViewMode> {
    use calnotes_core::model::ViewMode;
    match s.to_ascii_lowercase().as_str() {
        "day" => Some(ViewMode::Day),
        "week" => Some(ViewMode::Week),
        "workweek" | "work-week" | "work_week" => Some(ViewMode::WorkWeek),
        "twoweeks" | "two-weeks" | "two_weeks" => Some(ViewMode::TwoWeeks),
        "month" => Some(ViewMode::Month),
        "twomonths" | "two-months" | "two_months" => Some(ViewMode::TwoMonths),
        _ => None,
    }
}

#[cfg(unix)]
fn run_device() -> ExitCode {
    device_loop::run()
}

#[cfg(not(unix))]
fn run_device() -> ExitCode {
    eprintln!(
        "remarkable-calendar-notes: device mode requires QTFB (Unix/reMarkable only).\n\
         On this platform, use `remarkable-calendar-notes preview` instead."
    );
    ExitCode::FAILURE
}

#[cfg(unix)]
mod device_loop {
    use super::app::{CANVAS_H, CANVAS_W, TOOLBAR_ROW_H};
    use super::display::{self, FrameSink};
    use super::App;
    use calnotes_core::render::FrameBuffer;
    use calnotes_core::view::Rect;
    use calnotes_device::qtfb::{input_kind, QtfbClient};
    use std::io;
    use std::process::ExitCode;
    use std::time::{Duration, Instant};

    /// How often to poll enabled sources for fresh events while running,
    /// independent of any user-triggered refresh.
    const AUTO_REFRESH_INTERVAL: Duration = Duration::from_secs(15 * 60);
    /// AppLoad associates the QTFB shared image with its QML controller
    /// asynchronously and does not acknowledge repaint requests. Republish
    /// briefly after startup so a request queued before that association
    /// cannot leave the window white until the next user action.
    const STARTUP_REPAINT_INTERVAL: Duration = Duration::from_millis(300);
    const STARTUP_REPAINT_COUNT: u8 = 5;

    /// A finger contact only counts as a deliberate tap if it lasts less
    /// than this and moves less than [`TAP_MOVE_THRESHOLD`] pixels. Generous
    /// so an ordinary tap on a calendar cell reliably opens that day.
    const TAP_MAX_DURATION: Duration = Duration::from_millis(700);
    const TAP_MOVE_THRESHOLD: i32 = 60;

    /// Minimum time between on-screen updates while a stroke is in progress.
    /// Ink is captured losslessly regardless; this only coalesces the screen
    /// refresh so the display host is never handed more repaint requests
    /// than it can drain (which used to make a fast stroke stall and then
    /// "catch up"). A small fixed value keeps writing responsive.
    const PEN_PUBLISH_THROTTLE: Duration = Duration::from_millis(4);

    /// The AppLoad "close/leave" gesture is a one-finger flick that starts at
    /// the very top edge and drags downwards. AppLoad recognises it
    /// host-side (press above y=100, release between y=100 and y=400), but it
    /// may leave this app *running in the background* rather than killing it —
    /// in which case we keep the QTFB framebuffer bound and the NEXT AppLoad
    /// app opens to a broken window. So we detect the same gesture and exit
    /// cleanly ourselves, releasing the framebuffer and pen device. These are
    /// the thresholds for "a finger swipe that began at the top and travelled
    /// clearly downwards".
    const APPLOAD_CLOSE_START_Y: i32 = 100;
    const APPLOAD_CLOSE_DRAG_MIN: i32 = 150;

    /// In-progress finger contact, used for palm rejection (see the event
    /// loop). A tap is a single, brief, still contact with no pen activity.
    struct TouchTrack {
        start_x: i32,
        start_y: i32,
        start: Instant,
        moved: bool,
        /// A pen sample coincided with this contact — a palm resting while
        /// the pen writes. Only rejects *writing-area* taps; a deliberate
        /// finger tap on a toolbar button is still honoured.
        invalid: bool,
        /// A second simultaneous contact was seen (a two-finger gesture or a
        /// palm). Only used to reject *writing-area* taps, not button taps.
        multi: bool,
        active_points: u32,
        /// Whether the initial contact landed on the top view-button row —
        /// the only place the AppLoad top-edge close flick can begin. Taps
        /// here are confirmed on release (fire only if the finger did not
        /// travel), so a downward flick is not mistaken for a button press.
        /// Every other button fires immediately on press, exactly as before.
        top_row: bool,
    }

    /// The device's real display sink: QTFB shared memory plus its update
    /// requests. Deliberately trivial — everything that decides *what* to
    /// publish lives in `display`, which is tested on every platform.
    struct QtfbSink(QtfbClient);

    impl FrameSink for QtfbSink {
        fn pixels(&mut self) -> &mut [u8] {
            self.0.shared_memory()
        }

        fn request_full_update(&mut self) -> io::Result<()> {
            self.0.request_full_update()
        }

        fn request_partial_update(&mut self, rect: Rect) -> io::Result<()> {
            self.0
                .request_partial_update(rect.x, rect.y, rect.w, rect.h)
        }
    }

    pub fn run() -> ExitCode {
        super::diagnostics::log(format_args!(
            "device mode: HOME={:?}, QTFB_KEY present={}",
            std::env::var_os("HOME"),
            std::env::var_os("QTFB_KEY").is_some()
        ));
        let requested_key = std::env::var("QTFB_KEY").unwrap_or_else(|_| "<missing>".to_string());
        let client = match QtfbClient::connect(CANVAS_W as usize, CANVAS_H as usize) {
            Ok(mut c) => {
                let shared_bytes = c.shared_memory().len();
                super::diagnostics::log(format_args!(
                    "QTFB connected: requested_key={}, confirmed_key={}, dimensions={}x{}, shared_bytes={}, expected_bytes={}",
                    requested_key,
                    c.framebuffer_key,
                    c.width,
                    c.height,
                    shared_bytes,
                    CANVAS_W as usize * CANVAS_H as usize * 2
                ));
                c
            }
            Err(e) => {
                super::diagnostics::log(format_args!("QTFB connection failed: {e}"));
                return ExitCode::FAILURE;
            }
        };
        let mut sink = QtfbSink(client);
        let mut fb = FrameBuffer::new(CANVAS_W as usize, CANVAS_H as usize);
        display::render_startup_screen(&mut fb);
        fb.write_rgb565_into(sink.pixels());
        if let Err(error) = sink.request_full_update() {
            super::diagnostics::log(format_args!("startup screen publish failed: {error}"));
            return ExitCode::FAILURE;
        }
        super::diagnostics::log(format_args!("startup screen published"));

        // Run everything else under a panic guard: any internal failure
        // (state, rendering, input handling) must end on a readable error
        // screen with the log path, never a dead blank window.
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            event_loop(&mut sink, &mut fb)
        })) {
            Ok(code) => code,
            Err(_) => {
                super::diagnostics::log(format_args!(
                    "device event loop panicked; showing error screen"
                ));
                show_fatal_screen(
                    &mut sink,
                    "UNEXPECTED ERROR",
                    "THE APP HIT AN INTERNAL ERROR (PANIC). THE FULL DETAILS ARE IN THE LOG.",
                )
            }
        }
    }

    /// The real application: load state, render, and handle input until the
    /// AppLoad window closes. Any error returns via [`show_fatal_screen`] so
    /// the failure and the log path are always shown on the device.
    fn event_loop(sink: &mut QtfbSink, fb: &mut FrameBuffer) -> ExitCode {
        let mut app = match App::new() {
            Ok(app) => {
                super::diagnostics::log(format_args!(
                    "state loaded: view={:?}, sources={}, ink_days={}",
                    app.state.config.view_mode,
                    app.state.config.sources.len(),
                    app.state.ink.days.len()
                ));
                app
            }
            Err(error) => {
                super::diagnostics::log(format_args!("state load failed: {error}"));
                return show_fatal_screen(
                    sink,
                    "STARTUP ERROR",
                    &format!("STATE LOAD FAILED: {error}"),
                );
            }
        };

        // A corrupt/incompatible stored config or ink file is recovered to
        // defaults rather than failing; make that visible in the log and as
        // an on-screen status line so it is never silently swallowed.
        for warning in &app.state.load_warnings {
            super::diagnostics::log(format_args!("state warning: {warning}"));
        }
        if let Some(first) = app.state.load_warnings.first() {
            app.status = format!("STORED SETTINGS WERE RESET: {first}");
        }

        // The one and only framebuffer. It is rendered into on startup and
        // then *incrementally* modified: each pen sample draws a single
        // line segment into it and publishes just that rectangle. A full
        // re-render happens only for navigation, view/UI changes, and
        // completed background refreshes.
        app.start_refresh();
        super::diagnostics::log(format_args!("initial source refresh started"));
        if !redraw(sink, &app, fb, "initial") {
            return show_fatal_screen(sink, "DISPLAY ERROR", "CHECK DEVICE LOG");
        }
        let mut last_refresh = Instant::now();
        let mut startup_repaints_remaining = STARTUP_REPAINT_COUNT;
        let mut next_startup_repaint = Instant::now() + STARTUP_REPAINT_INTERVAL;

        let mut pen_down = false;
        // Palm rejection: a finger contact only counts as a deliberate tap
        // (open a day / press a button) if it is a single, brief, still
        // contact during which the pen was not used. A resting palm while
        // writing moves, lasts, multi-touches, or coincides with pen input,
        // so it is ignored instead of opening the day under the palm.
        let mut touch: Option<TouchTrack> = None;

        // Try to read the pen digitizer directly for smoother handwriting.
        // If it opens, we use its samples (every hardware sample, no QTFB
        // coalescing) and ignore QTFB pen events once the first raw sample
        // arrives; if it never opens or never produces a sample, QTFB pen
        // keeps working exactly as before.
        let wacom = calnotes_device::wacom::reader::spawn(CANVAS_W, CANVAS_H);
        match &wacom {
            Some((info, _)) => super::diagnostics::log(format_args!(
                "raw pen: reading {} ('{}') x={:?} y={:?} pmax={}",
                info.path, info.name, info.x, info.y, info.pressure_max
            )),
            None => super::diagnostics::log(format_args!(
                "raw pen: no digitizer found; using QTFB pen events"
            )),
        }
        let wacom_rx = wacom.map(|(_, rx)| rx);
        // Set once the first raw pen sample is seen and used, after which
        // QTFB pen events are ignored to avoid drawing each stroke twice.
        let mut raw_pen_active = false;
        // Whether the current raw stroke is writing ink (vs a pen tap that
        // hit a toolbar/settings control).
        let mut raw_writing = false;

        // Pen ink drawn but not yet pushed to the display, and when it was
        // last pushed. While writing, on-screen updates are throttled to
        // `pen_refresh_ms` so the display host is never flooded with more
        // repaint requests than it can drain — the flood is what made a fast
        // stroke stall and then "catch up". Ink itself is always captured;
        // this only coalesces the screen refresh.
        let mut pending_pen_dirty: Option<Rect> = None;
        let mut last_pen_publish = Instant::now();

        loop {
            let had_events;
            match sink.0.poll_events() {
                Ok(events) => {
                    had_events = !events.is_empty();
                    let mut needs_full_redraw = false;
                    // Segments drawn this cycle are blitted into `fb`
                    // immediately (cheap, in-memory) and their dirty rects
                    // unioned here, so the whole burst is pushed to the
                    // display in ONE partial update below instead of one
                    // blocking socket round-trip per pen sample. That keeps
                    // us draining the socket fast enough that QTFB does not
                    // overflow and drop the rest of a fast stroke.
                    let mut pen_dirty: Option<Rect> = None;
                    for ev in events {
                        match ev.kind {
                            input_kind::TOUCH_PRESS => {
                                // Buttons below the top view-button row fire
                                // immediately on press, exactly as they always
                                // did — instant and reliable. Only the top
                                // view-button row (the one place the AppLoad
                                // top-edge close flick can start) defers to
                                // release, so a downward flick is recognised as
                                // a swipe instead of pressing a view button.
                                let hits_ui = app.touch_hits_ui(ev.x, ev.y);
                                let top_row = ev.y < TOOLBAR_ROW_H;
                                if hits_ui && !top_row {
                                    app.handle_touch_tap(ev.x, ev.y);
                                    needs_full_redraw = true;
                                    touch = None;
                                } else {
                                    match touch.as_mut() {
                                        None => {
                                            touch = Some(TouchTrack {
                                                start_x: ev.x,
                                                start_y: ev.y,
                                                start: Instant::now(),
                                                moved: false,
                                                invalid: pen_down,
                                                multi: false,
                                                active_points: 1,
                                                top_row: hits_ui && top_row,
                                            });
                                        }
                                        // A second simultaneous contact.
                                        Some(track) => {
                                            track.active_points += 1;
                                            track.multi = true;
                                        }
                                    }
                                }
                            }
                            input_kind::TOUCH_UPDATE => {
                                if let Some(track) = touch.as_mut() {
                                    if (ev.x - track.start_x).abs() > TAP_MOVE_THRESHOLD
                                        || (ev.y - track.start_y).abs() > TAP_MOVE_THRESHOLD
                                    {
                                        track.moved = true;
                                    }
                                    // AppLoad top-edge close flick: a finger
                                    // that began at the very top and has
                                    // travelled clearly downwards. Exit
                                    // cleanly so the framebuffer and pen
                                    // device are released for the next app.
                                    if track.start_y < APPLOAD_CLOSE_START_Y
                                        && ev.y - track.start_y >= APPLOAD_CLOSE_DRAG_MIN
                                    {
                                        super::diagnostics::log(format_args!(
                                            "AppLoad close flick detected (start_y={}, y={}); exiting cleanly",
                                            track.start_y, ev.y
                                        ));
                                        return ExitCode::SUCCESS;
                                    }
                                }
                            }
                            input_kind::TOUCH_RELEASE => {
                                if let Some(track) = touch.as_mut() {
                                    track.active_points = track.active_points.saturating_sub(1);
                                    if track.active_points == 0 {
                                        let track = touch.take().unwrap();
                                        // Same close-flick check on release, in
                                        // case the downward travel only lands
                                        // in the release event.
                                        if track.start_y < APPLOAD_CLOSE_START_Y
                                            && ev.y - track.start_y >= APPLOAD_CLOSE_DRAG_MIN
                                        {
                                            super::diagnostics::log(format_args!(
                                                "AppLoad close flick on release (start_y={}, y={}); exiting cleanly",
                                                track.start_y, ev.y
                                            ));
                                            return ExitCode::SUCCESS;
                                        }
                                        let brief = track.start.elapsed() <= TAP_MAX_DURATION;
                                        let is_tap = if track.top_row {
                                            // A top-row view-button tap only
                                            // has to be brief and not travel —
                                            // pen proximity or a fat two-point
                                            // contact must not swallow it.
                                            brief && !track.moved
                                        } else {
                                            // Writing-area tap: full palm
                                            // rejection.
                                            brief
                                                && !track.moved
                                                && !track.invalid
                                                && !track.multi
                                                && !pen_down
                                        };
                                        if is_tap {
                                            app.handle_touch_tap(track.start_x, track.start_y);
                                            needs_full_redraw = true;
                                        }
                                    }
                                }
                            }
                            input_kind::PEN_PRESS if !raw_pen_active => {
                                // Any pen use during a finger contact marks
                                // that contact as a palm.
                                if let Some(track) = touch.as_mut() {
                                    track.invalid = true;
                                }
                                match app.handle_pen_tap(ev.x, ev.y) {
                                    // The pen operated a toolbar/settings
                                    // control, exactly like a finger tap.
                                    Some(redraw) => {
                                        needs_full_redraw |= redraw;
                                        pen_down = false;
                                    }
                                    // Below the toolbar: begin writing.
                                    None => {
                                        pen_down = true;
                                        app.pen_down(ev.x, ev.y, ev.pen_pressure());
                                    }
                                }
                            }
                            input_kind::PEN_UPDATE if pen_down && !raw_pen_active => {
                                if let Some(track) = touch.as_mut() {
                                    track.invalid = true;
                                }
                                if let Some(segment) = app.pen_move(ev.x, ev.y, ev.pen_pressure()) {
                                    if let Some(rect) = display::blit_segment(fb, segment) {
                                        pen_dirty = Some(match pen_dirty {
                                            Some(acc) => display::union_rect(acc, rect),
                                            None => rect,
                                        });
                                    }
                                }
                            }
                            input_kind::PEN_RELEASE if !raw_pen_active => {
                                if let Some(track) = touch.as_mut() {
                                    track.invalid = true;
                                }
                                pen_down = false;
                                needs_full_redraw |= app.pen_up();
                            }
                            // The VKB key code travels in the event's `x`
                            // field, not `d` — see calnotes_device::protocol.
                            input_kind::VKB_PRESS => {
                                app.handle_vkb(ev.vkb_key_code());
                                needs_full_redraw = true;
                            }
                            _ => {}
                        }
                    }

                    // Raw pen digitizer samples (every hardware sample). If
                    // the user disabled raw pen, drop any samples and let the
                    // QTFB pen path run instead.
                    let raw_enabled = app.state.config.raw_pen_input;
                    if let Some(rx) = &wacom_rx {
                        while let Ok(sample) = rx.try_recv() {
                            if !raw_enabled {
                                raw_pen_active = false;
                                raw_writing = false;
                                continue;
                            }
                            // Once we trust raw pen, ignore QTFB pen events.
                            if !raw_pen_active {
                                raw_pen_active = true;
                                super::diagnostics::log(format_args!(
                                    "raw pen: first sample received; using digitizer input"
                                ));
                            }
                            // The pen is being used → any finger contact is a
                            // palm, not a tap.
                            if let Some(track) = touch.as_mut() {
                                track.invalid = true;
                            }
                            match sample {
                                calnotes_device::wacom::PenSample::Down { x, y, pressure } => {
                                    // A pen tap on a toolbar/settings control
                                    // acts like a finger tap; below the
                                    // toolbar it begins writing.
                                    match app.handle_pen_tap(x, y) {
                                        Some(redraw) => {
                                            needs_full_redraw |= redraw;
                                            raw_writing = false;
                                        }
                                        None => {
                                            raw_writing = true;
                                            app.pen_down(x, y, pressure);
                                        }
                                    }
                                }
                                calnotes_device::wacom::PenSample::Move { x, y, pressure } => {
                                    if raw_writing {
                                        if let Some(segment) = app.pen_move(x, y, pressure) {
                                            if let Some(rect) = display::blit_segment(fb, segment) {
                                                pen_dirty = Some(match pen_dirty {
                                                    Some(acc) => display::union_rect(acc, rect),
                                                    None => rect,
                                                });
                                            }
                                        }
                                    }
                                }
                                calnotes_device::wacom::PenSample::Up => {
                                    if raw_writing {
                                        needs_full_redraw |= app.pen_up();
                                        raw_writing = false;
                                    }
                                }
                            }
                        }
                    }

                    if needs_full_redraw {
                        // A full redraw repaints the committed ink too, so it
                        // supersedes any accumulated pen rectangle.
                        pending_pen_dirty = None;
                        if !redraw(sink, &app, fb, "input") {
                            return show_fatal_screen(sink, "DISPLAY ERROR", "INPUT REDRAW FAILED");
                        }
                    } else {
                        // Fold this cycle's freshly-drawn ink into whatever is
                        // still waiting to be shown.
                        if let Some(rect) = pen_dirty {
                            pending_pen_dirty = Some(match pending_pen_dirty {
                                Some(acc) => display::union_rect(acc, rect),
                                None => rect,
                            });
                        }
                        // Publish immediately once the stroke ends; while it
                        // is still going, throttle to PEN_PUBLISH_THROTTLE.
                        let writing = pen_down || raw_writing;
                        if let Some(rect) = pending_pen_dirty {
                            if !writing || last_pen_publish.elapsed() >= PEN_PUBLISH_THROTTLE {
                                match display::publish_rect(sink, fb, rect) {
                                    Ok(_) => {}
                                    Err(e) => {
                                        super::diagnostics::log(format_args!(
                                            "partial update failed: {e}"
                                        ));
                                        return show_fatal_screen(
                                            sink,
                                            "DISPLAY ERROR",
                                            "PARTIAL UPDATE FAILED",
                                        );
                                    }
                                }
                                pending_pen_dirty = None;
                                last_pen_publish = Instant::now();
                            }
                        }
                    }
                }
                Err(_) => {
                    // Host closed the socket (window closed): exit cleanly.
                    super::diagnostics::log(format_args!("QTFB host closed the socket"));
                    return ExitCode::SUCCESS;
                }
            }

            // While a stroke is in progress, defer every heavy full-redraw
            // (background refresh results, the periodic auto-refresh, and the
            // startup repaints). A full re-render mid-stroke blocks the loop
            // long enough for pen samples to pile up and the ink to appear in
            // a late burst; running them only between strokes keeps writing
            // smooth without dropping any updates.
            let writing = pen_down || raw_writing;
            if !writing {
                // Background refresh / Google login results are applied here;
                // neither ever blocks this loop.
                if app.poll_background() {
                    super::diagnostics::log(format_args!("background update: {}", app.status));
                    if !redraw(sink, &app, fb, "background") {
                        return show_fatal_screen(
                            sink,
                            "DISPLAY ERROR",
                            "BACKGROUND REDRAW FAILED",
                        );
                    }
                }

                if last_refresh.elapsed() >= AUTO_REFRESH_INTERVAL {
                    app.start_refresh();
                    super::diagnostics::log(format_args!("automatic source refresh started"));
                    if !redraw(sink, &app, fb, "automatic refresh") {
                        return show_fatal_screen(sink, "DISPLAY ERROR", "AUTOMATIC REDRAW FAILED");
                    }
                    last_refresh = Instant::now();
                }

                if startup_repaints_remaining > 0 && Instant::now() >= next_startup_repaint {
                    let attempt = STARTUP_REPAINT_COUNT - startup_repaints_remaining + 1;
                    if !redraw(
                        sink,
                        &app,
                        fb,
                        &format!("startup repaint {attempt}/{STARTUP_REPAINT_COUNT}"),
                    ) {
                        return show_fatal_screen(sink, "DISPLAY ERROR", "STARTUP REPAINT FAILED");
                    }
                    startup_repaints_remaining -= 1;
                    next_startup_repaint += STARTUP_REPAINT_INTERVAL;
                }
            }

            // QTFB coalesces/drops pen samples between reads, so how often
            // we drain the socket sets how much of the pen's high-rate
            // motion we actually capture. While a pen or finger is on the
            // glass — or we just consumed a burst of events — poll almost
            // continuously so strokes (including the very first arc of a
            // letter) keep their shape and feel responsive. When idle, fall
            // back to a calm ~60 Hz to spare the battery.
            let input_active = pen_down || raw_writing || touch.is_some() || had_events;
            let idle_sleep = Duration::from_millis(16);
            let active_sleep = Duration::from_millis(2);
            std::thread::sleep(if input_active {
                active_sleep
            } else {
                idle_sleep
            });
        }
    }

    fn redraw(sink: &mut QtfbSink, app: &App, fb: &mut FrameBuffer, reason: &str) -> bool {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            display::redraw(sink, app, fb)
        })) {
            Ok(Ok(stats)) => {
                super::diagnostics::log(format_args!(
                    "{reason} redraw accepted: render_time={:?}, source_non_white={}, shared_non_white={}",
                    stats.render_time,
                    stats.source_has_non_white_pixels,
                    stats.shared_memory_has_non_white_bytes
                ));
                if stats.source_has_non_white_pixels && stats.shared_memory_has_non_white_bytes {
                    true
                } else {
                    super::diagnostics::log(format_args!(
                        "{reason} redraw rejected because the rendered or published frame was blank"
                    ));
                    false
                }
            }
            Ok(Err(error)) => {
                super::diagnostics::log(format_args!("{reason} redraw failed: {error}"));
                false
            }
            Err(_) => {
                super::diagnostics::log(format_args!("{reason} render panicked"));
                false
            }
        }
    }

    fn show_fatal_screen(sink: &mut QtfbSink, title: &str, detail: &str) -> ExitCode {
        let mut fb = FrameBuffer::new(CANVAS_W as usize, CANVAS_H as usize);
        let log_path = super::diagnostics::path()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "LOG UNAVAILABLE".to_string());
        display::render_fatal_screen(&mut fb, title, detail, &log_path);
        fb.write_rgb565_into(sink.pixels());
        match sink.request_full_update() {
            Ok(()) => super::diagnostics::log(format_args!("fatal screen published: {title}")),
            Err(error) => {
                super::diagnostics::log(format_args!("fatal screen publish failed: {error}"))
            }
        }
        let mut last_refresh = Instant::now();
        loop {
            if sink.0.poll_events().is_err() {
                return ExitCode::FAILURE;
            }
            if last_refresh.elapsed() >= Duration::from_secs(5) {
                let _ = sink.request_full_update();
                last_refresh = Instant::now();
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}
