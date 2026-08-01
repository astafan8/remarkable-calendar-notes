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
mod display;

use app::App;
use std::env;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("preview") => run_preview(&args[1..]),
        Some("run") | None => run_device(),
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
        let mut app = match App::new() {
            Ok(a) => a,
            Err(e) => {
                eprintln!("remarkable-calendar-notes: failed to load app state: {e}");
                return ExitCode::FAILURE;
            }
        };

        let client = match QtfbClient::connect(CANVAS_W as usize, CANVAS_H as usize) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("remarkable-calendar-notes: failed to connect to QTFB host: {e}");
                return ExitCode::FAILURE;
            }
        };
        let mut sink = QtfbSink(client);

        // The one and only framebuffer. It is rendered into on startup and
        // then *incrementally* modified: each pen sample draws a single
        // line segment into it and publishes just that rectangle. A full
        // re-render happens only for navigation, view/UI changes, and
        // completed background refreshes.
        let mut fb = FrameBuffer::new(CANVAS_W as usize, CANVAS_H as usize);
        app.start_refresh();
        redraw(&mut sink, &app, &mut fb);
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
                                        eprintln!(
                                            "remarkable-calendar-notes: qtfb partial update failed: {e}"
                                        );
                                    }
                                }
                            }
                            input_kind::PEN_RELEASE => {
                                pen_down = false;
                                app.pen_up();
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
                    if needs_full_redraw {
                        redraw(&mut sink, &app, &mut fb);
                    }
                }
                Err(_) => {
                    // Host closed the socket (window closed): exit cleanly.
                    return ExitCode::SUCCESS;
                }
            }

            // Background refresh / Google login results are applied here;
            // neither ever blocks this loop.
            if app.poll_background() {
                redraw(&mut sink, &app, &mut fb);
            }

            if last_refresh.elapsed() >= AUTO_REFRESH_INTERVAL {
                app.start_refresh();
                redraw(&mut sink, &app, &mut fb);
                last_refresh = Instant::now();
            }

            std::thread::sleep(Duration::from_millis(16));
        }
    }

    fn redraw(sink: &mut QtfbSink, app: &App, fb: &mut FrameBuffer) {
        if let Err(e) = display::redraw(sink, app, fb) {
            eprintln!("remarkable-calendar-notes: qtfb update request failed: {e}");
        }
    }
}
