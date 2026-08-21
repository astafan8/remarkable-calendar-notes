//! Direct reading of the reMarkable 2 Wacom pen digitizer (`/dev/input`).
//!
//! The AppLoad/QTFB host coalesces and drops pen samples between reads, so
//! the *start* of a quick or small stroke (the slow initial arc, and even
//! the contact point) can be lost. Reading the digitizer device directly
//! gives us every sample at the hardware's full rate, exactly like the
//! community `harmony` app does — which is what makes small handwriting
//! render faithfully.
//!
//! This module has two halves:
//!
//! - A pure, host-testable **decoder** ([`PenDecoder`]) that turns the raw
//!   `struct input_event` stream (position/pressure/contact) into screen-
//!   space [`PenSample`]s, applying the rM2 digitizer→screen transform
//!   (swap axes, invert Y), with the axis ranges read from the device
//!   itself.
//! - A `cfg(unix)` [`reader`] that finds and opens the device, reads its
//!   axis ranges, and streams decoded samples over a channel.
//!
//! We never `EVIOCGRAB` the device — we read it in parallel with the host,
//! so QTFB keeps working and remains the automatic fallback if the device
//! can't be opened or produces nothing.

// Linux input event codes we care about (from <linux/input-event-codes.h>).
pub const EV_SYN: u16 = 0x00;
pub const EV_KEY: u16 = 0x01;
pub const EV_ABS: u16 = 0x03;
pub const SYN_REPORT: u16 = 0x00;
pub const ABS_X: u16 = 0x00;
pub const ABS_Y: u16 = 0x01;
pub const ABS_PRESSURE: u16 = 0x18;
pub const BTN_TOUCH: u16 = 0x14a;

/// One decoded field triple from a `struct input_event` (its `time` is
/// irrelevant to us and dropped).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputEventRaw {
    pub kind: u16,
    pub code: u16,
    pub value: i32,
}

/// The size, in bytes, of the kernel's `struct input_event` on the target
/// this binary runs on: `struct timeval` (two `long`s) followed by
/// `__u16 type, __u16 code, __s32 value`.
pub const EVENT_SIZE: usize = 2 * std::mem::size_of::<std::os::raw::c_long>() + 8;

/// Offset of the `type` field, just past the `struct timeval`.
const FIELDS_OFFSET: usize = 2 * std::mem::size_of::<std::os::raw::c_long>();

/// Parse one `struct input_event` from the front of `bytes`, or `None` if
/// there are too few bytes.
pub fn parse_event(bytes: &[u8]) -> Option<InputEventRaw> {
    if bytes.len() < EVENT_SIZE {
        return None;
    }
    let kind = u16::from_ne_bytes(bytes[FIELDS_OFFSET..FIELDS_OFFSET + 2].try_into().ok()?);
    let code = u16::from_ne_bytes(
        bytes[FIELDS_OFFSET + 2..FIELDS_OFFSET + 4]
            .try_into()
            .ok()?,
    );
    let value = i32::from_ne_bytes(
        bytes[FIELDS_OFFSET + 4..FIELDS_OFFSET + 8]
            .try_into()
            .ok()?,
    );
    Some(InputEventRaw { kind, code, value })
}

/// A pen event in screen pixels, ready to drive ink drawing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PenSample {
    Down { x: i32, y: i32, pressure: f32 },
    Move { x: i32, y: i32, pressure: f32 },
    Up,
}

/// Inclusive `[min, max]` range of a digitizer axis, as reported by the
/// device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AxisRange {
    pub min: i32,
    pub max: i32,
}

impl AxisRange {
    pub fn new(min: i32, max: i32) -> Self {
        AxisRange { min, max }
    }
    fn span(&self) -> i32 {
        (self.max - self.min).max(1)
    }
}

/// Accumulates raw events and emits one [`PenSample`] per input frame
/// (`SYN_REPORT`) while the state meaningfully changes.
#[derive(Debug, Clone)]
pub struct PenDecoder {
    x: AxisRange,
    y: AxisRange,
    pressure_max: i32,
    screen_w: i32,
    screen_h: i32,
    raw_x: i32,
    raw_y: i32,
    pressure: f32,
    contact: bool,
    was_contact: bool,
    last_emitted: Option<(i32, i32)>,
}

impl PenDecoder {
    pub fn new(
        x: AxisRange,
        y: AxisRange,
        pressure_max: i32,
        screen_w: i32,
        screen_h: i32,
    ) -> Self {
        PenDecoder {
            x,
            y,
            pressure_max: pressure_max.max(1),
            screen_w,
            screen_h,
            raw_x: x.min,
            raw_y: y.min,
            pressure: 1.0,
            contact: false,
            was_contact: false,
            last_emitted: None,
        }
    }

    /// The rM2 digitizer is mounted rotated 90° relative to the screen and
    /// with an inverted Y axis, so the raw ABS_X drives the screen's Y and
    /// the raw ABS_Y drives the screen's X (this matches the community
    /// `rmkit`/`harmony` transform). Ranges come from the device.
    fn screen_coords(&self) -> (i32, i32) {
        let sx = ((self.raw_y - self.y.min) as f32 / self.y.span() as f32 * self.screen_w as f32)
            .round() as i32;
        let sy = (self.screen_h as f32
            - (self.raw_x - self.x.min) as f32 / self.x.span() as f32 * self.screen_h as f32)
            .round() as i32;
        (
            sx.clamp(0, self.screen_w - 1),
            sy.clamp(0, self.screen_h - 1),
        )
    }

    /// Feed one raw event; returns a screen-space sample at frame
    /// boundaries (`SYN_REPORT`) when the pen goes down, moves, or lifts.
    pub fn feed(&mut self, ev: InputEventRaw) -> Option<PenSample> {
        match ev.kind {
            EV_ABS => {
                match ev.code {
                    ABS_X => self.raw_x = ev.value,
                    ABS_Y => self.raw_y = ev.value,
                    ABS_PRESSURE => {
                        self.pressure = (ev.value as f32 / self.pressure_max as f32).clamp(0.0, 1.0)
                    }
                    _ => {}
                }
                None
            }
            EV_KEY if ev.code == BTN_TOUCH => {
                self.contact = ev.value != 0;
                None
            }
            EV_SYN if ev.code == SYN_REPORT => {
                let (sx, sy) = self.screen_coords();
                if self.contact && !self.was_contact {
                    self.was_contact = true;
                    self.last_emitted = Some((sx, sy));
                    Some(PenSample::Down {
                        x: sx,
                        y: sy,
                        pressure: self.pressure,
                    })
                } else if !self.contact && self.was_contact {
                    self.was_contact = false;
                    self.last_emitted = None;
                    Some(PenSample::Up)
                } else if self.contact {
                    // Emit every distinct position — that is the whole point:
                    // do not drop the slow start of a small stroke.
                    if self.last_emitted != Some((sx, sy)) {
                        self.last_emitted = Some((sx, sy));
                        Some(PenSample::Move {
                            x: sx,
                            y: sy,
                            pressure: self.pressure,
                        })
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

#[cfg(unix)]
pub mod reader {
    //! Find, open, and stream the Wacom device on the reMarkable.

    use super::*;
    use std::os::unix::io::RawFd;
    use std::sync::mpsc::{channel, Receiver};

    /// What we discovered about the opened device (for the diagnostic log).
    #[derive(Debug, Clone)]
    pub struct WacomInfo {
        pub path: String,
        pub name: String,
        pub x: AxisRange,
        pub y: AxisRange,
        pub pressure_max: i32,
    }

    const IOC_READ: u64 = 2;

    // musl's `ioctl` takes a `c_int` request; glibc's takes a `c_ulong`.
    #[cfg(target_env = "musl")]
    type IoctlReq = libc::c_int;
    #[cfg(not(target_env = "musl"))]
    type IoctlReq = libc::c_ulong;

    fn ioc(dir: u64, typ: u8, nr: u8, size: usize) -> u64 {
        (dir << 30) | ((size as u64) << 16) | ((typ as u64) << 8) | (nr as u64)
    }

    fn eviocgname(len: usize) -> u64 {
        ioc(IOC_READ, b'E', 0x06, len)
    }

    fn eviocgabs(axis: u16) -> u64 {
        // struct input_absinfo is 6 x i32 = 24 bytes.
        ioc(IOC_READ, b'E', 0x40 + axis as u8, 24)
    }

    fn read_name(fd: RawFd) -> String {
        let mut buf = [0u8; 256];
        let n = unsafe { libc::ioctl(fd, eviocgname(buf.len()) as IoctlReq, buf.as_mut_ptr()) };
        if n <= 0 {
            return String::new();
        }
        let end = buf.iter().position(|&b| b == 0).unwrap_or(n as usize);
        String::from_utf8_lossy(&buf[..end]).into_owned()
    }

    fn read_axis(fd: RawFd, axis: u16) -> Option<(AxisRange, i32)> {
        // input_absinfo: value, minimum, maximum, fuzz, flat, resolution.
        let mut info = [0i32; 6];
        let rc = unsafe { libc::ioctl(fd, eviocgabs(axis) as IoctlReq, info.as_mut_ptr()) };
        if rc < 0 {
            return None;
        }
        Some((AxisRange::new(info[1], info[2]), info[2]))
    }

    /// True if this device looks like the pen digitizer: it has a plausible
    /// ABS_X range and reports pressure.
    fn looks_like_pen(fd: RawFd, name: &str) -> bool {
        if name.to_lowercase().contains("wacom") {
            return true;
        }
        match (read_axis(fd, ABS_X), read_axis(fd, ABS_PRESSURE)) {
            (Some((x, _)), Some((_, pmax))) => x.max > 10_000 && pmax > 0,
            _ => false,
        }
    }

    /// Scan `/dev/input/event*` for the pen digitizer and open it read-only
    /// (never grabbed). Returns the fd and what we learned about it.
    pub fn find_device() -> Option<(RawFd, WacomInfo)> {
        for n in 0..32 {
            let path = format!("/dev/input/event{n}");
            let c_path = match std::ffi::CString::new(path.clone()) {
                Ok(p) => p,
                Err(_) => continue,
            };
            let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDONLY) };
            if fd < 0 {
                continue;
            }
            let name = read_name(fd);
            if looks_like_pen(fd, &name) {
                if let (Some((x, _)), Some((y, _)), Some((_, pmax))) = (
                    read_axis(fd, ABS_X),
                    read_axis(fd, ABS_Y),
                    read_axis(fd, ABS_PRESSURE),
                ) {
                    return Some((
                        fd,
                        WacomInfo {
                            path,
                            name,
                            x,
                            y,
                            pressure_max: pmax,
                        },
                    ));
                }
            }
            unsafe {
                libc::close(fd);
            }
        }
        None
    }

    /// Find the pen device and stream decoded samples over a channel from a
    /// background thread. Returns `None` (and the caller falls back to QTFB
    /// pen events) if no device could be opened.
    pub fn spawn(screen_w: i32, screen_h: i32) -> Option<(WacomInfo, Receiver<PenSample>)> {
        let (fd, info) = find_device()?;
        let mut decoder = PenDecoder::new(info.x, info.y, info.pressure_max, screen_w, screen_h);
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let mut buf = [0u8; EVENT_SIZE * 64];
            loop {
                let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
                if n > 0 {
                    let bytes = &buf[..n as usize];
                    let mut off = 0;
                    while off + EVENT_SIZE <= bytes.len() {
                        if let Some(ev) = parse_event(&bytes[off..off + EVENT_SIZE]) {
                            if let Some(sample) = decoder.feed(ev) {
                                if tx.send(sample).is_err() {
                                    return; // receiver gone
                                }
                            }
                        }
                        off += EVENT_SIZE;
                    }
                } else if n == 0 {
                    return; // device closed
                } else {
                    let err = std::io::Error::last_os_error();
                    match err.raw_os_error() {
                        Some(libc::EINTR) => continue,
                        Some(libc::EAGAIN) => {
                            std::thread::sleep(std::time::Duration::from_millis(2));
                            continue;
                        }
                        _ => return,
                    }
                }
            }
        });
        Some((info, rx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // rM2-like axis ranges and a 1404x1872 screen.
    fn decoder() -> PenDecoder {
        PenDecoder::new(
            AxisRange::new(0, 20966),
            AxisRange::new(0, 15725),
            AxisRange::new(0, 4095).max,
            1404,
            1872,
        )
    }

    fn abs(code: u16, value: i32) -> InputEventRaw {
        InputEventRaw {
            kind: EV_ABS,
            code,
            value,
        }
    }
    fn touch(down: bool) -> InputEventRaw {
        InputEventRaw {
            kind: EV_KEY,
            code: BTN_TOUCH,
            value: if down { 1 } else { 0 },
        }
    }
    fn syn() -> InputEventRaw {
        InputEventRaw {
            kind: EV_SYN,
            code: SYN_REPORT,
            value: 0,
        }
    }

    #[test]
    fn parse_event_reads_type_code_value_after_the_timeval() {
        let mut bytes = vec![0u8; EVENT_SIZE];
        bytes[FIELDS_OFFSET..FIELDS_OFFSET + 2].copy_from_slice(&EV_ABS.to_ne_bytes());
        bytes[FIELDS_OFFSET + 2..FIELDS_OFFSET + 4].copy_from_slice(&ABS_X.to_ne_bytes());
        bytes[FIELDS_OFFSET + 4..FIELDS_OFFSET + 8].copy_from_slice(&12345i32.to_ne_bytes());
        assert_eq!(
            parse_event(&bytes),
            Some(InputEventRaw {
                kind: EV_ABS,
                code: ABS_X,
                value: 12345
            })
        );
        assert_eq!(parse_event(&bytes[..EVENT_SIZE - 1]), None);
    }

    #[test]
    fn digitizer_corners_map_to_the_expected_screen_corners() {
        let mut d = decoder();
        // raw (x_min, y_min) → bottom-left of the screen.
        d.raw_x = 0;
        d.raw_y = 0;
        assert_eq!(d.screen_coords(), (0, 1871));
        // raw (x_max, y_max) → top-right.
        d.raw_x = 20966;
        d.raw_y = 15725;
        assert_eq!(d.screen_coords(), (1403, 0));
    }

    #[test]
    fn a_down_move_up_sequence_emits_the_expected_samples() {
        let mut d = decoder();
        // Nothing until the pen touches.
        assert_eq!(d.feed(abs(ABS_X, 10000)), None);
        assert_eq!(d.feed(abs(ABS_Y, 8000)), None);
        assert_eq!(d.feed(abs(ABS_PRESSURE, 2048)), None);
        assert_eq!(d.feed(touch(true)), None);
        assert!(matches!(d.feed(syn()), Some(PenSample::Down { .. })));

        // A tiny move (a few raw units) still produces a Move — the slow
        // start of a small stroke is not dropped.
        d.feed(abs(ABS_X, 10040));
        d.feed(abs(ABS_Y, 8030));
        assert!(matches!(d.feed(syn()), Some(PenSample::Move { .. })));

        // No change → no sample.
        assert_eq!(d.feed(syn()), None);

        // Lift.
        d.feed(touch(false));
        assert_eq!(d.feed(syn()), Some(PenSample::Up));
    }

    #[test]
    fn pressure_is_normalized_against_the_reported_max() {
        let mut d = decoder();
        d.feed(touch(true));
        d.feed(abs(ABS_PRESSURE, 4095));
        if let Some(PenSample::Down { pressure, .. }) = d.feed(syn()) {
            assert!((pressure - 1.0).abs() < 0.01);
        } else {
            panic!("expected a Down sample");
        }
    }
}
