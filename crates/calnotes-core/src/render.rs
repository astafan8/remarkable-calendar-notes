//! Software rendering into an RGB565 pixel buffer, shared by the on-device
//! QTFB backend and the desktop preview (PNG/PPM) command so the exact same
//! drawing code is exercised by tests on any platform.
//!
//! Two typefaces are available via [`Font`]. The calendar chrome (buttons,
//! month names, day numbers) uses a compact, hand-authored 3x5 bitmap font
//! for a crisp, deterministic look. The settings menu — where the user
//! types emails, Apple IDs and passwords and needs real mixed-case,
//! monospaced glyphs — uses a Latin subset of JetBrains Mono (SIL OFL,
//! embedded via `include_bytes!`) rasterized anti-aliased by the pure-Rust
//! `ab_glyph` crate. Neither path needs Qt or a system font, so the binary
//! still links statically for musl.

use crate::view::Rect;

/// RM2 QTFB native resolution: 1404x1872, RGB565.
pub const SCREEN_WIDTH: usize = 1404;
pub const SCREEN_HEIGHT: usize = 1872;

/// A grayscale-only RGB565 pixel buffer (the app never needs color for a
/// black-and-white e-ink display, so the whole rendering pipeline works in
/// 8-bit gray and only converts to RGB565 at the pixel-set boundary).
pub struct FrameBuffer {
    pub width: usize,
    pub height: usize,
    pixels: Vec<u16>,
}

pub const WHITE: u8 = 255;
pub const BLACK: u8 = 0;
pub const GRAY: u8 = 160;
/// A near-white grey used only for transient eraser feedback: the faint
/// trail shown under the pen while the eraser tool is active, so the user
/// can see what they are about to remove (as on the native notebook).
pub const LIGHT_GRAY: u8 = 210;

/// Which typeface `draw_text` should use. The calendar screen and the
/// settings menu both use the embedded, anti-aliased JetBrains Mono
/// [`Font::Ui`]; the compact built-in [`Font::Bitmap`] is kept only for the
/// startup/fatal splash screens, which must render before any font asset is
/// touched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Font {
    Bitmap,
    Ui,
}

fn gray_to_rgb565(gray: u8) -> u16 {
    let r = (gray as u16 >> 3) & 0x1F;
    let g = (gray as u16 >> 2) & 0x3F;
    let b = (gray as u16 >> 3) & 0x1F;
    (r << 11) | (g << 5) | b
}

impl FrameBuffer {
    pub fn new(width: usize, height: usize) -> Self {
        FrameBuffer {
            width,
            height,
            pixels: vec![gray_to_rgb565(WHITE); width * height],
        }
    }

    pub fn clear(&mut self, gray: u8) {
        self.pixels.fill(gray_to_rgb565(gray));
    }

    /// Number of pixels that are not pure white, useful for startup
    /// diagnostics when a device reports an apparently blank framebuffer.
    pub fn non_white_pixel_count(&self) -> usize {
        let white = gray_to_rgb565(WHITE);
        self.pixels.iter().filter(|pixel| **pixel != white).count()
    }

    pub fn has_non_white_pixels(&self) -> bool {
        let white = gray_to_rgb565(WHITE);
        self.pixels.iter().any(|pixel| *pixel != white)
    }

    pub fn set_pixel(&mut self, x: i32, y: i32, gray: u8) {
        if x < 0 || y < 0 || x as usize >= self.width || y as usize >= self.height {
            return;
        }
        self.pixels[y as usize * self.width + x as usize] = gray_to_rgb565(gray);
    }

    pub fn fill_rect(&mut self, rect: Rect, gray: u8) {
        let x0 = rect.x.max(0);
        let y0 = rect.y.max(0);
        let x1 = (rect.x + rect.w).min(self.width as i32);
        let y1 = (rect.y + rect.h).min(self.height as i32);
        for y in y0..y1 {
            for x in x0..x1 {
                self.set_pixel(x, y, gray);
            }
        }
    }

    pub fn draw_rect_outline(&mut self, rect: Rect, gray: u8) {
        self.fill_rect(
            Rect {
                x: rect.x,
                y: rect.y,
                w: rect.w,
                h: 1,
            },
            gray,
        );
        self.fill_rect(
            Rect {
                x: rect.x,
                y: rect.y + rect.h - 1,
                w: rect.w,
                h: 1,
            },
            gray,
        );
        self.fill_rect(
            Rect {
                x: rect.x,
                y: rect.y,
                w: 1,
                h: rect.h,
            },
            gray,
        );
        self.fill_rect(
            Rect {
                x: rect.x + rect.w - 1,
                y: rect.y,
                w: 1,
                h: rect.h,
            },
            gray,
        );
    }

    /// Bresenham line, drawn with a square `thickness`-pixel brush — cheap
    /// enough to call per incoming pen sample for direct incremental
    /// drawing (see docs/LIMITATIONS.md on QTFB pen latency).
    pub fn draw_line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, gray: u8, thickness: i32) {
        self.draw_line_styled(x0, y0, x1, y1, gray, thickness, None);
    }

    /// Like [`FrameBuffer::draw_line`], but optionally dashed: `dash =
    /// Some((on, off))` plots `on` pixels then skips `off` along the line,
    /// used to draw the lasso outline distinctly from real ink. `dash =
    /// None` is a solid line, pixel-for-pixel identical to `draw_line`.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_line_styled(
        &mut self,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        gray: u8,
        thickness: i32,
        dash: Option<(i32, i32)>,
    ) {
        let dx = (x1 - x0).abs();
        let dy = -(y1 - y0).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut err = dx + dy;
        let (mut x, mut y) = (x0, y0);
        let half = (thickness / 2).max(0);
        let period = dash.map(|(on, off)| (on.max(1), (on + off).max(1)));
        let mut step: i32 = 0;
        loop {
            let plot = match period {
                Some((on, cycle)) => step % cycle < on,
                None => true,
            };
            if plot {
                for oy in -half..=half {
                    for ox in -half..=half {
                        self.set_pixel(x + ox, y + oy, gray);
                    }
                }
            }
            if x == x1 && y == y1 {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                x += sx;
            }
            if e2 <= dx {
                err += dx;
                y += sy;
            }
            step += 1;
        }
    }

    /// Bounding rect (clamped to the buffer) touched while drawing a line
    /// with the given thickness — used to compute the smallest QTFB partial
    /// refresh region for a single incremental pen segment.
    pub fn line_bounds(x0: i32, y0: i32, x1: i32, y1: i32, thickness: i32) -> Rect {
        let half = (thickness / 2).max(1);
        let min_x = x0.min(x1) - half;
        let min_y = y0.min(y1) - half;
        let max_x = x0.max(x1) + half;
        let max_y = y0.max(y1) + half;
        Rect {
            x: min_x,
            y: min_y,
            w: max_x - min_x + 1,
            h: max_y - min_y + 1,
        }
    }

    /// Draw `text` with its visual top-left at `(x, y)`, in the given grey,
    /// using the selected [`Font`]. The `Bitmap` font is the compact
    /// built-in calendar font; the `Ui` font is the embedded, anti-aliased
    /// JetBrains Mono used in the settings menu.
    pub fn draw_text(&mut self, x: i32, y: i32, text: &str, gray: u8, scale: i32, font: Font) {
        match font {
            Font::Bitmap => self.draw_text_bitmap(x, y, text, gray, scale),
            Font::Ui => self.draw_text_ui(x, y, text, gray, scale),
        }
    }

    fn draw_text_bitmap(&mut self, x: i32, y: i32, text: &str, gray: u8, scale: i32) {
        let mut cursor_x = x;
        for c in text.chars() {
            if let Some(glyph) = bitmap_glyph(c) {
                for (row, bits) in glyph.iter().enumerate() {
                    for col in 0..3 {
                        if bits & (1 << (2 - col)) != 0 {
                            self.fill_rect(
                                Rect {
                                    x: cursor_x + col * scale,
                                    y: y + row as i32 * scale,
                                    w: scale,
                                    h: scale,
                                },
                                gray,
                            );
                        }
                    }
                }
            }
            cursor_x += 4 * scale; // 3 columns of glyph + 1 column of spacing
        }
    }

    fn draw_text_ui(&mut self, x: i32, y: i32, text: &str, gray: u8, scale: i32) {
        use ab_glyph::{Font as _, ScaleFont};
        let px = (scale.max(1) as f32) * PX_PER_SCALE;
        let font = ui_font();
        let scaled = font.as_scaled(px);
        let baseline = y as f32 + scaled.ascent();
        let mut caret = x as f32;
        let mut previous: Option<ab_glyph::GlyphId> = None;
        for c in text.chars() {
            let id = font.glyph_id(c);
            if let Some(prev) = previous {
                caret += scaled.kern(prev, id);
            }
            let glyph = id.with_scale_and_position(px, ab_glyph::point(caret, baseline));
            if let Some(outline) = font.outline_glyph(glyph) {
                let bounds = outline.px_bounds();
                let ox = bounds.min.x as i32;
                let oy = bounds.min.y as i32;
                outline.draw(|gx, gy, coverage| {
                    self.blend_gray(ox + gx as i32, oy + gy as i32, gray, coverage);
                });
            }
            caret += scaled.h_advance(id);
            previous = Some(id);
        }
    }

    /// Pixel width `draw_text` would occupy for `text` at `scale` in `font`.
    pub fn text_width(text: &str, scale: i32, font: Font) -> i32 {
        match font {
            Font::Bitmap => text.chars().count() as i32 * 4 * scale,
            Font::Ui => {
                use ab_glyph::{Font as _, ScaleFont};
                let px = (scale.max(1) as f32) * PX_PER_SCALE;
                let scaled = ui_font().as_scaled(px);
                let mut width = 0.0f32;
                let mut previous: Option<ab_glyph::GlyphId> = None;
                for c in text.chars() {
                    let id = ui_font().glyph_id(c);
                    if let Some(prev) = previous {
                        width += scaled.kern(prev, id);
                    }
                    width += scaled.h_advance(id);
                    previous = Some(id);
                }
                width.ceil() as i32
            }
        }
    }

    /// The vertical space one line of `scale`-sized text occupies in `font`,
    /// used to position and stack text.
    pub fn text_height(scale: i32, font: Font) -> i32 {
        match font {
            // 5 glyph rows plus two rows of leading, matching how the bitmap
            // font stacks vertically.
            Font::Bitmap => 7 * scale,
            Font::Ui => {
                use ab_glyph::{Font as _, ScaleFont};
                let px = (scale.max(1) as f32) * PX_PER_SCALE;
                ui_font().as_scaled(px).height().ceil() as i32
            }
        }
    }

    /// Alpha-blend a glyph pixel of grey `text_gray` with `coverage` in
    /// `0.0..=1.0` over the existing background pixel.
    fn blend_gray(&mut self, x: i32, y: i32, text_gray: u8, coverage: f32) {
        if coverage <= 0.0
            || x < 0
            || y < 0
            || x as usize >= self.width
            || y as usize >= self.height
        {
            return;
        }
        let idx = y as usize * self.width + x as usize;
        let background = rgb565_to_gray(self.pixels[idx]);
        let c = coverage.min(1.0);
        let blended = (background as f32 * (1.0 - c) + text_gray as f32 * c).round() as u8;
        self.pixels[idx] = gray_to_rgb565(blended);
    }

    /// Clamp `rect` to the buffer, returning `None` if nothing of it is
    /// visible. Used to keep dirty rectangles (and the QTFB partial-update
    /// requests derived from them) inside the real framebuffer.
    pub fn clamp_rect(&self, rect: Rect) -> Option<Rect> {
        let x0 = rect.x.max(0);
        let y0 = rect.y.max(0);
        let x1 = (rect.x + rect.w).min(self.width as i32);
        let y1 = (rect.y + rect.h).min(self.height as i32);
        if x1 <= x0 || y1 <= y0 {
            None
        } else {
            Some(Rect {
                x: x0,
                y: y0,
                w: x1 - x0,
                h: y1 - y0,
            })
        }
    }

    /// Copy just `rect` of this buffer into `dst` (a full-frame RGB565
    /// little-endian byte buffer with the same width and row stride, e.g.
    /// QTFB's shared memory). Returns the clamped rectangle that was
    /// actually written, or `None` if nothing was.
    ///
    /// This is what makes incremental pen drawing cheap: a stroke segment
    /// touches a few hundred pixels, so only those rows/columns are
    /// copied out instead of all 1404x1872 of them.
    pub fn write_rect_rgb565_into(&self, dst: &mut [u8], rect: Rect) -> Option<Rect> {
        let r = self.clamp_rect(rect)?;
        for row in 0..r.h {
            let y = (r.y + row) as usize;
            let src_start = y * self.width + r.x as usize;
            let src_end = src_start + r.w as usize;
            let dst_start = src_start * 2;
            let dst_end = src_end * 2;
            if dst_end > dst.len() {
                return None;
            }
            for (i, pixel) in self.pixels[src_start..src_end].iter().enumerate() {
                let bytes = pixel.to_le_bytes();
                dst[dst_start + i * 2] = bytes[0];
                dst[dst_start + i * 2 + 1] = bytes[1];
            }
        }
        Some(r)
    }

    /// Copy the whole buffer into `dst` as RGB565 little-endian bytes,
    /// without allocating (unlike [`FrameBuffer::as_rgb565_bytes`]).
    pub fn write_rgb565_into(&self, dst: &mut [u8]) {
        let n = (self.pixels.len() * 2).min(dst.len());
        for (i, pixel) in self.pixels.iter().take(n / 2).enumerate() {
            let bytes = pixel.to_le_bytes();
            dst[i * 2] = bytes[0];
            dst[i * 2 + 1] = bytes[1];
        }
    }

    /// Raw RGB565 little-endian bytes, exactly as QTFB's shared framebuffer
    /// memory expects them.
    pub fn as_rgb565_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.pixels.len() * 2);
        for p in &self.pixels {
            out.extend_from_slice(&p.to_le_bytes());
        }
        out
    }

    /// Encode as a binary PPM (P6) for a deterministic, dependency-free
    /// desktop preview.
    pub fn to_ppm(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.width * self.height * 3 + 32);
        out.extend_from_slice(format!("P6\n{} {}\n255\n", self.width, self.height).as_bytes());
        for p in &self.pixels {
            let r = ((p >> 11) & 0x1F) as u32 * 255 / 31;
            let g = ((p >> 5) & 0x3F) as u32 * 255 / 63;
            let b = (p & 0x1F) as u32 * 255 / 31;
            out.push(r as u8);
            out.push(g as u8);
            out.push(b as u8);
        }
        out
    }
}

/// The embedded settings-menu font (a Latin subset of JetBrains Mono, SIL
/// OFL — see `assets/fonts/`). Parsed once and reused for every `Font::Ui`
/// `draw_text` call.
static FONT_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/fonts/JetBrainsMono-Regular.ttf"
));

/// Pixels of font height per historical `scale` unit. Chosen so the UI-font
/// call sites (which pass `scale` values of 2–7) render at a comparable
/// size to the bitmap font.
const PX_PER_SCALE: f32 = 8.0;

fn ui_font() -> &'static ab_glyph::FontRef<'static> {
    use std::sync::OnceLock;
    static FONT: OnceLock<ab_glyph::FontRef<'static>> = OnceLock::new();
    FONT.get_or_init(|| {
        ab_glyph::FontRef::try_from_slice(FONT_BYTES).expect("embedded UI font must parse")
    })
}

/// Recover an approximate grey level (0=black..255=white) from a stored
/// RGB565 grayscale pixel, for alpha-blending anti-aliased text over it.
fn rgb565_to_gray(pixel: u16) -> u8 {
    (((pixel >> 11) & 0x1F) as u32 * 255 / 31) as u8
}

/// A 3-column x 5-row bitmap glyph, one `u8` per row using its 3 low bits
/// (bit 2 = leftmost column). Used by [`Font::Bitmap`].
type Glyph = [u8; 5];

fn bitmap_glyph(c: char) -> Option<Glyph> {
    Some(match c.to_ascii_uppercase() {
        ' ' => [0b000, 0b000, 0b000, 0b000, 0b000],
        '0' => [0b111, 0b101, 0b101, 0b101, 0b111],
        '1' => [0b010, 0b110, 0b010, 0b010, 0b111],
        '2' => [0b111, 0b001, 0b111, 0b100, 0b111],
        '3' => [0b111, 0b001, 0b111, 0b001, 0b111],
        '4' => [0b101, 0b101, 0b111, 0b001, 0b001],
        '5' => [0b111, 0b100, 0b111, 0b001, 0b111],
        '6' => [0b111, 0b100, 0b111, 0b101, 0b111],
        '7' => [0b111, 0b001, 0b001, 0b001, 0b001],
        '8' => [0b111, 0b101, 0b111, 0b101, 0b111],
        '9' => [0b111, 0b101, 0b111, 0b001, 0b111],
        'A' => [0b010, 0b101, 0b111, 0b101, 0b101],
        'B' => [0b110, 0b101, 0b110, 0b101, 0b110],
        'C' => [0b011, 0b100, 0b100, 0b100, 0b011],
        'D' => [0b110, 0b101, 0b101, 0b101, 0b110],
        'E' => [0b111, 0b100, 0b110, 0b100, 0b111],
        'F' => [0b111, 0b100, 0b110, 0b100, 0b100],
        'G' => [0b011, 0b100, 0b101, 0b101, 0b011],
        'H' => [0b101, 0b101, 0b111, 0b101, 0b101],
        'I' => [0b111, 0b010, 0b010, 0b010, 0b111],
        'J' => [0b001, 0b001, 0b001, 0b101, 0b111],
        'K' => [0b101, 0b101, 0b110, 0b101, 0b101],
        'L' => [0b100, 0b100, 0b100, 0b100, 0b111],
        'M' => [0b101, 0b111, 0b111, 0b101, 0b101],
        'N' => [0b101, 0b111, 0b111, 0b111, 0b101],
        'O' => [0b111, 0b101, 0b101, 0b101, 0b111],
        'P' => [0b111, 0b101, 0b111, 0b100, 0b100],
        'Q' => [0b111, 0b101, 0b101, 0b111, 0b001],
        'R' => [0b111, 0b101, 0b110, 0b101, 0b101],
        'S' => [0b011, 0b100, 0b111, 0b001, 0b110],
        'T' => [0b111, 0b010, 0b010, 0b010, 0b010],
        'U' => [0b101, 0b101, 0b101, 0b101, 0b111],
        'V' => [0b101, 0b101, 0b101, 0b101, 0b010],
        'W' => [0b101, 0b101, 0b111, 0b111, 0b101],
        'X' => [0b101, 0b101, 0b010, 0b101, 0b101],
        'Y' => [0b101, 0b101, 0b010, 0b010, 0b010],
        'Z' => [0b111, 0b001, 0b010, 0b100, 0b111],
        ':' => [0b000, 0b010, 0b000, 0b010, 0b000],
        '-' => [0b000, 0b000, 0b111, 0b000, 0b000],
        '/' => [0b001, 0b001, 0b010, 0b100, 0b100],
        '.' => [0b000, 0b000, 0b000, 0b000, 0b010],
        ',' => [0b000, 0b000, 0b000, 0b010, 0b100],
        '?' => [0b111, 0b001, 0b010, 0b000, 0b010],
        '!' => [0b010, 0b010, 0b010, 0b000, 0b010],
        '\'' => [0b010, 0b010, 0b000, 0b000, 0b000],
        '@' => [0b111, 0b101, 0b111, 0b100, 0b011],
        '_' => [0b000, 0b000, 0b000, 0b000, 0b111],
        '+' => [0b000, 0b010, 0b111, 0b010, 0b000],
        '=' => [0b000, 0b111, 0b000, 0b111, 0b000],
        '#' => [0b101, 0b111, 0b101, 0b111, 0b101],
        '%' => [0b101, 0b001, 0b010, 0b100, 0b101],
        '&' => [0b010, 0b101, 0b010, 0b101, 0b011],
        '*' => [0b101, 0b010, 0b101, 0b000, 0b000],
        '(' => [0b001, 0b010, 0b010, 0b010, 0b001],
        ')' => [0b100, 0b010, 0b010, 0b010, 0b100],
        ';' => [0b000, 0b010, 0b000, 0b010, 0b100],
        '$' => [0b011, 0b110, 0b011, 0b110, 0b010],
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_buffer_is_all_white() {
        let fb = FrameBuffer::new(4, 4);
        let bytes = fb.as_rgb565_bytes();
        let expected = gray_to_rgb565(WHITE).to_le_bytes();
        for chunk in bytes.chunks_exact(2) {
            assert_eq!(chunk, expected);
        }
    }

    #[test]
    fn set_pixel_out_of_bounds_is_a_silent_no_op() {
        let mut fb = FrameBuffer::new(2, 2);
        fb.set_pixel(-1, -1, BLACK);
        fb.set_pixel(100, 100, BLACK);
        // Nothing should have changed.
        let bytes = fb.as_rgb565_bytes();
        let expected = gray_to_rgb565(WHITE).to_le_bytes();
        for chunk in bytes.chunks_exact(2) {
            assert_eq!(chunk, expected);
        }
    }

    #[test]
    fn email_and_password_symbols_render_visible_pixels() {
        // Entering an Apple ID / email / password needs these to render in
        // both fonts.
        for font in [Font::Bitmap, Font::Ui] {
            let mut fb = FrameBuffer::new(400, 40);
            fb.draw_text(0, 0, "a.b@c_d+e=f#1$2", BLACK, 3, font);
            assert!(fb.has_non_white_pixels());
        }
    }

    #[test]
    fn text_has_a_positive_measured_width_and_height() {
        for font in [Font::Bitmap, Font::Ui] {
            assert!(FrameBuffer::text_width("Calendar", 3, font) > 0);
            assert!(FrameBuffer::text_height(3, font) > 0);
            // A longer string is wider than a shorter one at the same size.
            assert!(
                FrameBuffer::text_width("MMMMMM", 3, font) > FrameBuffer::text_width("M", 3, font)
            );
        }
    }

    #[test]
    fn dashed_line_draws_fewer_pixels_than_a_solid_one() {
        let solid_count = {
            let mut fb = FrameBuffer::new(100, 4);
            fb.draw_line_styled(0, 1, 99, 1, BLACK, 1, None);
            fb.non_white_pixel_count()
        };
        let dashed_count = {
            let mut fb = FrameBuffer::new(100, 4);
            fb.draw_line_styled(0, 1, 99, 1, BLACK, 1, Some((8, 8)));
            fb.non_white_pixel_count()
        };
        assert!(dashed_count > 0);
        assert!(dashed_count < solid_count);
    }

    #[test]
    fn fill_rect_clips_to_buffer_bounds() {
        let mut fb = FrameBuffer::new(4, 4);
        fb.fill_rect(
            Rect {
                x: 2,
                y: 2,
                w: 10,
                h: 10,
            },
            BLACK,
        );
        // (3,3) should be black, (1,1) should remain white.
        let idx = |x: usize, y: usize| (y * 4 + x) * 2;
        let bytes = fb.as_rgb565_bytes();
        assert_eq!(
            &bytes[idx(3, 3)..idx(3, 3) + 2],
            gray_to_rgb565(BLACK).to_le_bytes()
        );
        assert_eq!(
            &bytes[idx(1, 1)..idx(1, 1) + 2],
            gray_to_rgb565(WHITE).to_le_bytes()
        );
    }

    #[test]
    fn line_bounds_covers_a_diagonal_line_with_thickness() {
        let b = FrameBuffer::line_bounds(0, 0, 10, 5, 3);
        assert!(b.x <= 0 && b.y <= 0);
        assert!(b.x + b.w >= 11 && b.y + b.h >= 6);
    }

    #[test]
    fn ppm_header_matches_dimensions() {
        let fb = FrameBuffer::new(3, 2);
        let ppm = fb.to_ppm();
        let header = String::from_utf8_lossy(&ppm[..10]);
        assert!(header.starts_with("P6\n3 2\n255"));
    }

    #[test]
    fn draw_text_of_known_glyphs_sets_at_least_one_pixel() {
        let mut fb = FrameBuffer::new(60, 40);
        fb.draw_text(0, 0, "12", BLACK, 3, Font::Bitmap);
        assert!(fb.has_non_white_pixels());
    }

    #[test]
    fn text_width_scales_with_character_count_and_scale() {
        assert_eq!(FrameBuffer::text_width("", 2, Font::Bitmap), 0);
        // The bitmap font is fixed-pitch: two characters are exactly twice
        // one, and doubling the scale doubles the width.
        assert_eq!(FrameBuffer::text_width("AB", 2, Font::Bitmap), 16);
        assert_eq!(FrameBuffer::text_width("AB", 4, Font::Bitmap), 32);
        assert!(
            FrameBuffer::text_width("AB", 2, Font::Ui) > FrameBuffer::text_width("A", 2, Font::Ui)
        );
    }

    #[test]
    fn unsupported_characters_are_skipped_without_panicking() {
        let mut fb = FrameBuffer::new(20, 10);
        fb.draw_text(0, 0, "a~$", BLACK, 1, Font::Bitmap); // '~' unsupported in bitmap
        fb.draw_text(0, 0, "a~$", BLACK, 1, Font::Ui);
    }

    #[test]
    fn clamp_rect_trims_to_the_buffer_and_rejects_offscreen_rects() {
        let fb = FrameBuffer::new(10, 10);
        assert_eq!(
            fb.clamp_rect(Rect {
                x: -5,
                y: -5,
                w: 20,
                h: 20
            }),
            Some(Rect {
                x: 0,
                y: 0,
                w: 10,
                h: 10
            })
        );
        assert_eq!(
            fb.clamp_rect(Rect {
                x: 20,
                y: 0,
                w: 5,
                h: 5
            }),
            None
        );
    }

    #[test]
    fn write_rect_rgb565_only_touches_the_requested_region() {
        let mut fb = FrameBuffer::new(4, 4);
        fb.fill_rect(
            Rect {
                x: 1,
                y: 1,
                w: 2,
                h: 2,
            },
            BLACK,
        );
        let mut dst = vec![0u8; 4 * 4 * 2];
        let written = fb
            .write_rect_rgb565_into(
                &mut dst,
                Rect {
                    x: 1,
                    y: 1,
                    w: 2,
                    h: 2,
                },
            )
            .unwrap();
        assert_eq!(
            written,
            Rect {
                x: 1,
                y: 1,
                w: 2,
                h: 2
            }
        );
        let black = gray_to_rgb565(BLACK).to_le_bytes();
        let idx = |x: usize, y: usize| (y * 4 + x) * 2;
        assert_eq!(&dst[idx(1, 1)..idx(1, 1) + 2], black);
        assert_eq!(&dst[idx(2, 2)..idx(2, 2) + 2], black);
        // Outside the rect nothing was written at all (still zeroed).
        assert_eq!(&dst[idx(0, 0)..idx(0, 0) + 2], &[0, 0]);
        assert_eq!(&dst[idx(3, 3)..idx(3, 3) + 2], &[0, 0]);
    }

    #[test]
    fn write_rgb565_into_matches_as_rgb565_bytes() {
        let mut fb = FrameBuffer::new(5, 3);
        fb.draw_line(0, 0, 4, 2, BLACK, 1);
        let mut dst = vec![0u8; 5 * 3 * 2];
        fb.write_rgb565_into(&mut dst);
        assert_eq!(dst, fb.as_rgb565_bytes());
    }
}
