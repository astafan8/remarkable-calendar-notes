//! Device I/O for the reMarkable 2: the AppLoad QTFB protocol.
//!
//! The crate is split so that everything testable is testable everywhere:
//!
//! - [`protocol`] is pure byte encoding/decoding (message layout, the
//!   `QTFB_KEY` framebuffer-key parsing, input-event decoding). It has no
//!   platform dependency and its tests run on any host.
//! - [`qtfb`] is the `cfg(unix)`-gated socket/shared-memory client that
//!   uses it — the reMarkable's OS is Linux and this talks directly to a
//!   Unix domain socket and POSIX shared memory, neither of which exists
//!   on Windows. `calnotes-app` uses a software-only preview mode there.
//!
//! Touch and pen input arrive over the *same* QTFB socket, already tagged
//! by kind (`INPUT_TOUCH_*` vs `INPUT_PEN_*`) by the AppLoad host process.
//! That tagging is what lets the app route pen samples to ink drawing and
//! touch samples to view navigation without them interfering with each
//! other — see `calnotes-app`'s event loop. This app runs as a normal
//! windowed AppLoad "external QTFB" app, not a full-screen "takeover" app.
//!
//! The QTFB host coalesces and drops pen samples between reads, which loses
//! the slow start of small/fast strokes. To render handwriting faithfully,
//! [`wacom`] *additionally* reads the pen digitizer device directly (never
//! grabbing it, so QTFB and the virtual keyboard keep working) and streams
//! every hardware sample; the app uses those when available and falls back
//! to QTFB pen events otherwise.

pub mod protocol;
pub mod wacom;

#[cfg(unix)]
pub mod qtfb;
