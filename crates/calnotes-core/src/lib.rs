//! `calnotes-core`: platform-independent logic for remarkable-calendar-notes.
//!
//! Everything in this crate is pure Rust with no reMarkable/QTFB dependency,
//! so it builds and runs its full test suite on Windows, Linux and macOS.
//! Device I/O (the QTFB socket protocol, pen/touch evdev reading) lives in
//! the sibling `calnotes-device` crate and is `cfg(unix)`-gated there.

pub mod config;
pub mod ics;
pub mod ink;
pub mod model;
pub mod persistence;
pub mod recurrence;
pub mod render;
pub mod sources;
pub mod timeutil;
pub mod view;
pub mod vkb;

pub use model::{CalendarSource, Event, EventTime};
