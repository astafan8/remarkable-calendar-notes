//! The display sink: how rendered pixels reach the screen.
//!
//! The on-device implementation is QTFB's shared memory plus its
//! update-request messages, but nothing here depends on that — the logic
//! that decides *what* to copy and *which* rectangle to refresh is
//! platform-independent and unit-tested on every host. `main.rs`'s
//! Unix-only device loop supplies a thin [`FrameSink`] adapter over
//! `calnotes_device::qtfb::QtfbClient`; tests supply a plain byte buffer.
//!
//! This split is what keeps the incremental pen path honest: the
//! assertion that a pen sample touches only a small dirty rectangle (and
//! never re-renders or re-copies the full 1404x1872 framebuffer) is a
//! real test, not a comment.
//!
//! Only `main.rs`'s Unix-only `device_loop` calls into this module in a
//! normal build, so a non-Unix host build legitimately sees it as unused;
//! `#[cfg(test)]` already covers it during `cargo test` on any platform.
#![cfg_attr(not(unix), allow(dead_code))]

use crate::app::{App, PenSegment};
use calnotes_core::render::{FrameBuffer, BLACK};
use calnotes_core::view::Rect;
use std::io;
use std::time::Duration;

/// A destination for rendered pixels: a full-frame RGB565 little-endian
/// buffer plus the ability to ask for a full or partial screen refresh.
pub trait FrameSink {
    fn pixels(&mut self) -> &mut [u8];
    fn request_full_update(&mut self) -> io::Result<()>;
    fn request_partial_update(&mut self, rect: Rect) -> io::Result<()>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RedrawStats {
    pub render_time: Duration,
    pub source_has_non_white_pixels: bool,
    pub shared_memory_has_non_white_bytes: bool,
}

/// Full redraw: re-render the current screen into `fb` (reusing its
/// allocation), publish every pixel, and refresh the whole display.
///
/// Used for startup, navigation, view/UI changes, and completed
/// background refreshes — never for an individual pen sample.
pub fn redraw<S: FrameSink>(
    sink: &mut S,
    app: &App,
    fb: &mut FrameBuffer,
) -> io::Result<RedrawStats> {
    let started = std::time::Instant::now();
    app.render_into(fb);
    let render_time = started.elapsed();
    let source_has_non_white_pixels = fb.has_non_white_pixels();
    fb.write_rgb565_into(sink.pixels());
    let shared_memory_has_non_white_bytes = sink.pixels().iter().any(|byte| *byte != 0xff);
    sink.request_full_update()?;
    Ok(RedrawStats {
        render_time,
        source_has_non_white_pixels,
        shared_memory_has_non_white_bytes,
    })
}

pub fn render_fatal_screen(fb: &mut FrameBuffer, title: &str, detail: &str, log_path: &str) {
    fb.clear(calnotes_core::render::WHITE);
    let left = 80;
    let width = fb.width as i32;
    // A visible border so even a mostly-empty error screen is unmistakably
    // "the app drew something" rather than a dead/blank window.
    fb.draw_rect_outline(
        Rect {
            x: 40,
            y: 40,
            w: width - 80,
            h: fb.height as i32 - 80,
        },
        BLACK,
    );
    fb.draw_text(left, 140, "CALENDAR NOTES", BLACK, 6);
    fb.draw_text(left, 280, title, BLACK, 5);

    // The failure message, wrapped so nothing important is cut off.
    let mut y = 400;
    for line in wrap(detail, chars_per_line(width, left, 3)) {
        fb.draw_text(left, y, &line, BLACK, 3);
        y += 40;
    }

    y += 40;
    fb.draw_text(left, y, "DETAILS WERE WRITTEN TO THE LOG FILE:", BLACK, 3);
    y += 50;
    // The full log path, wrapped rather than truncated — the whole point of
    // showing it is so the user can find and share it, so it must be
    // readable in its entirety.
    for line in wrap(log_path, chars_per_line(width, left, 2)) {
        fb.draw_text(left, y, &line, BLACK, 2);
        y += 30;
    }

    y += 30;
    fb.draw_text(
        left,
        y,
        "REOPEN THE APP, OR RUN THE DIAGNOSTICS COLLECTOR",
        BLACK,
        2,
    );
    fb.draw_text(left, y + 30, "AND SHARE THE LOG TO GET HELP.", BLACK, 2);
}

pub fn render_startup_screen(fb: &mut FrameBuffer) {
    fb.clear(calnotes_core::render::WHITE);
    let width = fb.width as i32;
    let height = fb.height as i32;
    fb.draw_rect_outline(
        Rect {
            x: 48,
            y: 48,
            w: width - 96,
            h: height - 96,
        },
        BLACK,
    );
    fb.draw_text(100, 300, "CALENDAR NOTES", BLACK, 7);
    fb.draw_text(100, 480, "STARTING...", BLACK, 5);
}

/// How many characters of `scale`-sized bitmap text fit between `left` and
/// the right margin (a symmetric margin is assumed).
fn chars_per_line(width: i32, left: i32, scale: i32) -> usize {
    let usable = (width - 2 * left).max(0);
    (usable / (4 * scale)).max(1) as usize
}

/// Split `text` (uppercased for the bitmap font) into lines no wider than
/// `max_chars`, breaking on spaces where possible and hard-splitting any
/// single run that is longer than a line (e.g. a long filesystem path).
fn wrap(text: &str, max_chars: usize) -> Vec<String> {
    let upper = text.to_uppercase();
    let mut lines = Vec::new();
    for word in upper.split_whitespace() {
        let mut word = word;
        // Hard-split words longer than a whole line (paths, tokens).
        while word.chars().count() > max_chars {
            let head: String = word.chars().take(max_chars).collect();
            lines.push(head);
            word = &word[word
                .char_indices()
                .nth(max_chars)
                .map(|(i, _)| i)
                .unwrap_or(word.len())..];
        }
        match lines.last_mut() {
            Some(last) if last.chars().count() + 1 + word.chars().count() <= max_chars => {
                last.push(' ');
                last.push_str(word);
            }
            _ => lines.push(word.to_string()),
        }
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// Incremental pen update: draw one stroke segment into the framebuffer
/// that already holds the current screen, publish only the pixels that
/// segment touched, and refresh only that rectangle.
///
/// Returns the refreshed rectangle, or `None` if the segment fell
/// entirely outside the framebuffer.
pub fn draw_segment<S: FrameSink>(
    sink: &mut S,
    fb: &mut FrameBuffer,
    segment: PenSegment,
) -> io::Result<Option<Rect>> {
    let dash = if segment.dashed { Some((8, 8)) } else { None };
    fb.draw_line_styled(
        segment.x0,
        segment.y0,
        segment.x1,
        segment.y1,
        segment.gray,
        segment.thickness,
        dash,
    );
    let Some(dirty) = fb.clamp_rect(segment.dirty_rect()) else {
        return Ok(None);
    };
    if fb.write_rect_rgb565_into(sink.pixels(), dirty).is_none() {
        return Ok(None);
    }
    sink.request_partial_update(dirty)?;
    Ok(Some(dirty))
}

/// An in-memory [`FrameSink`] used by tests (and available for any
/// headless harness): it records exactly which updates were requested.
#[cfg(test)]
pub struct MemorySink {
    pub pixels: Vec<u8>,
    pub full_updates: usize,
    pub partial_updates: Vec<Rect>,
}

#[cfg(test)]
impl MemorySink {
    pub fn new(width: usize, height: usize) -> Self {
        MemorySink {
            pixels: vec![0u8; width * height * 2],
            full_updates: 0,
            partial_updates: Vec::new(),
        }
    }
}

#[cfg(test)]
impl FrameSink for MemorySink {
    fn pixels(&mut self) -> &mut [u8] {
        &mut self.pixels
    }

    fn request_full_update(&mut self) -> io::Result<()> {
        self.full_updates += 1;
        Ok(())
    }

    fn request_partial_update(&mut self, rect: Rect) -> io::Result<()> {
        self.partial_updates.push(rect);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{App, CANVAS_H, CANVAS_W};
    use calnotes_core::model::ViewMode;
    use calnotes_core::persistence;

    fn with_temp_data_dir<T>(f: impl FnOnce() -> T) -> T {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var(persistence::DATA_DIR_ENV, dir.path());
        let result = f();
        std::env::remove_var(persistence::DATA_DIR_ENV);
        result
    }

    #[test]
    #[serial_test::serial]
    fn a_pen_sample_publishes_only_its_dirty_rect() {
        with_temp_data_dir(|| {
            let mut app = App::new().unwrap();
            app.set_view_mode(ViewMode::Day);
            let mut fb = FrameBuffer::new(CANVAS_W as usize, CANVAS_H as usize);
            let mut sink = MemorySink::new(CANVAS_W as usize, CANVAS_H as usize);
            let stats = redraw(&mut sink, &app, &mut fb).unwrap();
            assert_eq!(sink.full_updates, 1);
            assert!(stats.source_has_non_white_pixels);
            assert!(stats.shared_memory_has_non_white_bytes);

            // Everything the full redraw wrote is now the baseline.
            let baseline = sink.pixels.clone();

            app.pen_down(300, 700, 1.0);
            let segment = app.pen_move(306, 704, 1.0).unwrap();
            let dirty = draw_segment(&mut sink, &mut fb, segment)
                .unwrap()
                .expect("a dirty rect");

            // Exactly one partial refresh, no extra full refresh.
            assert_eq!(sink.full_updates, 1);
            assert_eq!(sink.partial_updates, vec![dirty]);

            // The refreshed region is tiny compared with the screen.
            let screen_pixels = (CANVAS_W * CANVAS_H) as i64;
            let dirty_pixels = (dirty.w * dirty.h) as i64;
            assert!(
                dirty_pixels * 1000 < screen_pixels,
                "dirty rect {dirty:?} is not small relative to the screen"
            );

            // Every byte that changed lies inside the dirty rectangle.
            for (i, (before, after)) in baseline.iter().zip(sink.pixels.iter()).enumerate() {
                if before == after {
                    continue;
                }
                let pixel = i / 2;
                let x = (pixel % CANVAS_W as usize) as i32;
                let y = (pixel / CANVAS_W as usize) as i32;
                assert!(
                    x >= dirty.x && x < dirty.x + dirty.w && y >= dirty.y && y < dirty.y + dirty.h,
                    "pixel ({x},{y}) changed outside the published rect {dirty:?}"
                );
            }
        });
    }

    #[test]
    #[serial_test::serial]
    fn incrementally_drawn_ink_matches_a_full_redraw_of_the_same_screen() {
        with_temp_data_dir(|| {
            let mut app = App::new().unwrap();
            app.set_view_mode(ViewMode::Day);
            let mut fb = FrameBuffer::new(CANVAS_W as usize, CANVAS_H as usize);
            let mut sink = MemorySink::new(CANVAS_W as usize, CANVAS_H as usize);
            redraw(&mut sink, &app, &mut fb).unwrap();

            app.pen_down(400, 800, 1.0);
            for (x, y) in [(420, 815), (450, 790), (470, 830)] {
                let segment = app.pen_move(x, y, 1.0).unwrap();
                draw_segment(&mut sink, &mut fb, segment).unwrap();
            }
            app.pen_up();

            // What the screen shows now must equal a from-scratch render
            // of the persisted strokes.
            let mut reference = FrameBuffer::new(CANVAS_W as usize, CANVAS_H as usize);
            let mut reference_sink = MemorySink::new(CANVAS_W as usize, CANVAS_H as usize);
            redraw(&mut reference_sink, &app, &mut reference).unwrap();
            assert_eq!(sink.pixels, reference_sink.pixels);
        });
    }

    #[test]
    #[serial_test::serial]
    fn an_offscreen_segment_requests_no_refresh() {
        with_temp_data_dir(|| {
            let mut fb = FrameBuffer::new(CANVAS_W as usize, CANVAS_H as usize);
            let mut sink = MemorySink::new(CANVAS_W as usize, CANVAS_H as usize);
            let result = draw_segment(
                &mut sink,
                &mut fb,
                PenSegment {
                    x0: -50,
                    y0: -50,
                    x1: -40,
                    y1: -40,
                    thickness: 2,
                    gray: BLACK,
                    dashed: false,
                },
            )
            .unwrap();
            assert!(result.is_none());
            assert!(sink.partial_updates.is_empty());
        });
    }

    #[test]
    fn fatal_screen_is_visibly_non_white_and_includes_diagnostic_content() {
        let mut fb = FrameBuffer::new(CANVAS_W as usize, CANVAS_H as usize);
        render_fatal_screen(
            &mut fb,
            "STARTUP ERROR",
            "state could not be loaded",
            "/tmp/calendar-notes.log",
        );
        assert!(fb.non_white_pixel_count() > 1_000);
    }

    #[test]
    fn wrap_hard_splits_a_long_path_and_preserves_every_character() {
        let path = "/home/root/.local/share/remarkable-calendar-notes/calendar-notes.log";
        let lines = wrap(path, 20);
        // Each line stays within the limit...
        assert!(lines.iter().all(|line| line.chars().count() <= 20));
        // ...and no character of the path is lost to truncation.
        let rejoined: String = lines.join("").replace(' ', "");
        assert_eq!(rejoined, path.to_uppercase().replace(' ', ""));
        assert!(lines.len() > 1);
    }

    #[test]
    fn wrap_breaks_a_sentence_on_spaces() {
        let lines = wrap("state could not be loaded", 12);
        assert!(lines.len() >= 2);
        assert!(lines.iter().all(|line| line.chars().count() <= 12));
    }

    #[test]
    fn startup_screen_is_visibly_non_white() {
        let mut fb = FrameBuffer::new(CANVAS_W as usize, CANVAS_H as usize);
        render_startup_screen(&mut fb);
        assert!(fb.non_white_pixel_count() > 1_000);
    }
}
