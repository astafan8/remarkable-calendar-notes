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
         --refresh          Fetch fresh events from enabled sources before rendering"
    );
}

fn run_preview(args: &[String]) -> ExitCode {
    let mut out_path = "preview.ppm".to_string();
    let mut view_mode: Option<calnotes_core::model::ViewMode> = None;
    let mut refresh = false;
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
    use super::app::{CANVAS_H, CANVAS_W};
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
    /// than this and moves less than [`TAP_MOVE_THRESHOLD`] pixels.
    const TAP_MAX_DURATION: Duration = Duration::from_millis(400);
    const TAP_MOVE_THRESHOLD: i32 = 40;

    /// In-progress finger contact, used for palm rejection (see the event
    /// loop). A tap is a single, brief, still contact with no pen activity.
    struct TouchTrack {
        start_x: i32,
        start_y: i32,
        start: Instant,
        moved: bool,
        invalid: bool,
        active_points: u32,
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

        loop {
            match sink.0.poll_events() {
                Ok(events) => {
                    let mut needs_full_redraw = false;
                    for ev in events {
                        match ev.kind {
                            input_kind::TOUCH_PRESS => match touch.as_mut() {
                                None => {
                                    touch = Some(TouchTrack {
                                        start_x: ev.x,
                                        start_y: ev.y,
                                        start: Instant::now(),
                                        moved: false,
                                        invalid: pen_down,
                                        active_points: 1,
                                    });
                                }
                                // A second simultaneous contact means a palm,
                                // not a finger tap.
                                Some(track) => {
                                    track.active_points += 1;
                                    track.invalid = true;
                                }
                            },
                            input_kind::TOUCH_UPDATE => {
                                if let Some(track) = touch.as_mut() {
                                    if (ev.x - track.start_x).abs() > TAP_MOVE_THRESHOLD
                                        || (ev.y - track.start_y).abs() > TAP_MOVE_THRESHOLD
                                    {
                                        track.moved = true;
                                    }
                                }
                            }
                            input_kind::TOUCH_RELEASE => {
                                if let Some(track) = touch.as_mut() {
                                    track.active_points = track.active_points.saturating_sub(1);
                                    if track.active_points == 0 {
                                        let track = touch.take().unwrap();
                                        let is_tap = !track.invalid
                                            && !track.moved
                                            && !pen_down
                                            && track.start.elapsed() <= TAP_MAX_DURATION;
                                        if is_tap {
                                            app.handle_touch_tap(track.start_x, track.start_y);
                                            needs_full_redraw = true;
                                        }
                                    }
                                }
                            }
                            input_kind::PEN_PRESS => {
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
                            input_kind::PEN_UPDATE if pen_down => {
                                if let Some(track) = touch.as_mut() {
                                    track.invalid = true;
                                }
                                if let Some(segment) = app.pen_move(ev.x, ev.y, ev.pen_pressure()) {
                                    if let Err(e) = display::draw_segment(sink, fb, segment) {
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
                            }
                            input_kind::PEN_RELEASE => {
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
                    if needs_full_redraw && !redraw(sink, &app, fb, "input") {
                        return show_fatal_screen(sink, "DISPLAY ERROR", "INPUT REDRAW FAILED");
                    }
                }
                Err(_) => {
                    // Host closed the socket (window closed): exit cleanly.
                    super::diagnostics::log(format_args!("QTFB host closed the socket"));
                    return ExitCode::SUCCESS;
                }
            }

            // Background refresh / Google login results are applied here;
            // neither ever blocks this loop.
            if app.poll_background() {
                super::diagnostics::log(format_args!("background update: {}", app.status));
                if !redraw(sink, &app, fb, "background") {
                    return show_fatal_screen(sink, "DISPLAY ERROR", "BACKGROUND REDRAW FAILED");
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

            std::thread::sleep(Duration::from_millis(16));
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
