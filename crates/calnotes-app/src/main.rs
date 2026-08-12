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
         --view <mode>      day | week | workweek | twoweeks | month\n  \
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
                    &mut sink,
                    "STARTUP ERROR",
                    &format!("STATE LOAD FAILED: {error}"),
                );
            }
        };

        // The one and only framebuffer. It is rendered into on startup and
        // then *incrementally* modified: each pen sample draws a single
        // line segment into it and publishes just that rectangle. A full
        // re-render happens only for navigation, view/UI changes, and
        // completed background refreshes.
        app.start_refresh();
        super::diagnostics::log(format_args!("initial source refresh started"));
        if !redraw(&mut sink, &app, &mut fb, "initial") {
            return show_fatal_screen(&mut sink, "DISPLAY ERROR", "CHECK DEVICE LOG");
        }
        let mut last_refresh = Instant::now();

        let mut pen_down = false;

        loop {
            match sink.0.poll_events() {
                Ok(events) => {
                    let mut needs_full_redraw = false;
                    for ev in events {
                        match ev.kind {
                            input_kind::TOUCH_PRESS => {
                                app.handle_touch_tap(ev.x, ev.y);
                                needs_full_redraw = true;
                            }
                            input_kind::PEN_PRESS => {
                                pen_down = true;
                                app.pen_down(ev.x, ev.y, ev.pen_pressure());
                            }
                            input_kind::PEN_UPDATE if pen_down => {
                                if let Some(segment) = app.pen_move(ev.x, ev.y, ev.pen_pressure()) {
                                    if let Err(e) =
                                        display::draw_segment(&mut sink, &mut fb, segment)
                                    {
                                        super::diagnostics::log(format_args!(
                                            "partial update failed: {e}"
                                        ));
                                        return show_fatal_screen(
                                            &mut sink,
                                            "DISPLAY ERROR",
                                            "PARTIAL UPDATE FAILED",
                                        );
                                    }
                                }
                            }
                            input_kind::PEN_RELEASE => {
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
                    if needs_full_redraw && !redraw(&mut sink, &app, &mut fb, "input") {
                        return show_fatal_screen(
                            &mut sink,
                            "DISPLAY ERROR",
                            "INPUT REDRAW FAILED",
                        );
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
                if !redraw(&mut sink, &app, &mut fb, "background") {
                    return show_fatal_screen(
                        &mut sink,
                        "DISPLAY ERROR",
                        "BACKGROUND REDRAW FAILED",
                    );
                }
            }

            if last_refresh.elapsed() >= AUTO_REFRESH_INTERVAL {
                app.start_refresh();
                super::diagnostics::log(format_args!("automatic source refresh started"));
                if !redraw(&mut sink, &app, &mut fb, "automatic refresh") {
                    return show_fatal_screen(
                        &mut sink,
                        "DISPLAY ERROR",
                        "AUTOMATIC REDRAW FAILED",
                    );
                }
                last_refresh = Instant::now();
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
