//! Pure, platform-independent QTFB wire-protocol encoding/decoding.
//!
//! Everything here is byte layout and arithmetic — no sockets, no shared
//! memory, no `libc` — so it compiles and its tests run on any host,
//! including the Windows/macOS machines used for development. The socket
//! plumbing that uses it lives in the `cfg(unix)`-only [`crate::qtfb`].
//!
//! The message layout, byte offsets, and numeric constants below are a
//! fixed wire ABI dictated by the AppLoad host process. Protocol facts
//! like these are not copyrightable expression; they are re-derived here
//! rather than copied from any client implementation.
//!
//! One detail worth calling out because it is easy to get wrong by
//! copy-pasting from a reMarkable Paper Pro (aarch64, 64-bit) client: the
//! reMarkable 2 runs a 32-bit `armv7` userspace, where C's `size_t` is 4
//! bytes, not 8. That changes the byte offsets inside `ServerMessage`
//! compared to a 64-bit build of the same C struct (the embedded
//! `size_t` field is 4 bytes wide instead of 8). The offsets below are
//! computed for the 32-bit ABI, matching RM2's actual host process.

use std::fmt;

/// Every QTFB message — in both directions — is a fixed-size packet on a
/// `SOCK_SEQPACKET` socket.
pub const MESSAGE_LEN: usize = 24;

pub const MESSAGE_INITIALIZE: u8 = 0;
pub const MESSAGE_UPDATE: u8 = 1;
pub const MESSAGE_TERMINATE: u8 = 3;
pub const MESSAGE_USERINPUT: u8 = 4;

pub const UPDATE_ALL: i32 = 0;
pub const UPDATE_PARTIAL: i32 = 1;

/// reMarkable 2 native framebuffer format: RM2FB, RGB565, 1404x1872.
pub const FBFMT_RM2FB: u8 = 0;

/// Environment variable AppLoad sets for a launched external app, naming
/// the framebuffer key that app must use. There is deliberately **no**
/// hardcoded fallback: guessing a key would connect the app to a
/// framebuffer that belongs to some other client, or to none at all.
pub const QTFB_KEY_ENV: &str = "QTFB_KEY";

/// Server -> client input event type codes.
pub mod input_kind {
    pub const TOUCH_PRESS: i32 = 0x10;
    pub const TOUCH_RELEASE: i32 = 0x11;
    pub const TOUCH_UPDATE: i32 = 0x12;
    pub const PEN_PRESS: i32 = 0x20;
    pub const PEN_RELEASE: i32 = 0x21;
    pub const PEN_UPDATE: i32 = 0x22;
    pub const VKB_PRESS: i32 = 0x40;
    pub const VKB_RELEASE: i32 = 0x41;
}

/// One decoded `MESSAGE_USERINPUT` packet.
///
/// The field names mirror the host's `UserInputContents` struct
/// (`inputType`, `devId`, `x`, `y`, `d`). What each field *means* depends
/// on `kind`, and two of those meanings are easy to get wrong:
///
/// - For `VKB_PRESS`/`VKB_RELEASE`, the key code is carried in **`x`** —
///   not in `d`. See [`InputEvent::vkb_key_code`].
/// - For pen events, `d` is a pressure percentage in `0..=100`, not a raw
///   digitizer value. See [`InputEvent::pen_pressure`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputEvent {
    pub kind: i32,
    pub dev_id: i32,
    pub x: i32,
    pub y: i32,
    pub d: i32,
}

impl InputEvent {
    /// The raw virtual-keyboard key code for a `VKB_*` event.
    pub fn vkb_key_code(&self) -> i32 {
        self.x
    }

    /// Pen pressure normalized to `0.0..=1.0`. AppLoad reports pen
    /// pressure as a whole-number percentage in `d`.
    pub fn pen_pressure(&self) -> f32 {
        (self.d as f32 / 100.0).clamp(0.0, 1.0)
    }
}

/// The host's reply to `MESSAGE_INITIALIZE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitReply {
    /// The framebuffer key the host actually bound this client to. This
    /// — not the key the client asked for — names the shared-memory
    /// object (see [`shm_path`]).
    pub shm_key_defined: i32,
    /// Size of the shared-memory framebuffer, in bytes.
    pub shm_size: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyError {
    Missing,
    Invalid(String),
}

impl fmt::Display for KeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeyError::Missing => write!(
                f,
                "{QTFB_KEY_ENV} is not set: this app must be launched by AppLoad, \
                 which sets it to the framebuffer key to use"
            ),
            KeyError::Invalid(v) => {
                write!(f, "{QTFB_KEY_ENV} is not a 32-bit integer: {v:?}")
            }
        }
    }
}

impl std::error::Error for KeyError {}

/// Parse the value of `QTFB_KEY`. Accepts an optionally signed decimal
/// integer with surrounding whitespace trimmed.
pub fn parse_framebuffer_key(raw: &str) -> Result<i32, KeyError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(KeyError::Missing);
    }
    trimmed
        .parse::<i32>()
        .map_err(|_| KeyError::Invalid(trimmed.to_string()))
}

/// Read and parse the framebuffer key from the process environment.
pub fn framebuffer_key_from_env() -> Result<i32, KeyError> {
    match std::env::var(QTFB_KEY_ENV) {
        Ok(v) => parse_framebuffer_key(&v),
        Err(std::env::VarError::NotPresent) => Err(KeyError::Missing),
        Err(std::env::VarError::NotUnicode(_)) => {
            Err(KeyError::Invalid("<non-unicode>".to_string()))
        }
    }
}

/// POSIX shared-memory object path for a framebuffer key. The host
/// publishes the framebuffer as `/qtfb_<key>`, which Linux realizes as a
/// file under `/dev/shm` — opening that path directly is equivalent to
/// (and simpler than) `shm_open()` for a read-write mapping.
pub fn shm_path(key: i32) -> String {
    format!("/dev/shm/qtfb_{key}")
}

/// `ClientMessage` for `MESSAGE_INITIALIZE`: `type:u8 @0`, then a union at
/// offset 4 (aligned to the 4-byte `int`/`FBKey`), here
/// `{ FBKey key; u8 format }`.
pub fn init_message(key: i32, format: u8) -> [u8; MESSAGE_LEN] {
    let mut msg = [0u8; MESSAGE_LEN];
    msg[0] = MESSAGE_INITIALIZE;
    msg[4..8].copy_from_slice(&key.to_ne_bytes());
    msg[8] = format;
    msg
}

pub fn update_all_message() -> [u8; MESSAGE_LEN] {
    let mut msg = [0u8; MESSAGE_LEN];
    msg[0] = MESSAGE_UPDATE;
    msg[4..8].copy_from_slice(&UPDATE_ALL.to_ne_bytes());
    msg
}

pub fn update_partial_message(x: i32, y: i32, w: i32, h: i32) -> [u8; MESSAGE_LEN] {
    let mut msg = [0u8; MESSAGE_LEN];
    msg[0] = MESSAGE_UPDATE;
    msg[4..8].copy_from_slice(&UPDATE_PARTIAL.to_ne_bytes());
    msg[8..12].copy_from_slice(&x.to_ne_bytes());
    msg[12..16].copy_from_slice(&y.to_ne_bytes());
    msg[16..20].copy_from_slice(&w.to_ne_bytes());
    msg[20..24].copy_from_slice(&h.to_ne_bytes());
    msg
}

pub fn terminate_message() -> [u8; MESSAGE_LEN] {
    let mut msg = [0u8; MESSAGE_LEN];
    msg[0] = MESSAGE_TERMINATE;
    msg
}

/// Decode the host's initialize reply. `ServerMessage` on the 32-bit
/// armv7 ABI: `type:u8 @0`, union at 4 — `{ int shmKeyDefined; size_t
/// shmSize; }`, both 4 bytes wide here.
pub fn parse_init_reply(buf: &[u8], n: usize) -> Option<InitReply> {
    if n < 12 || buf.len() < 12 {
        return None;
    }
    Some(InitReply {
        shm_key_defined: i32::from_ne_bytes(buf[4..8].try_into().ok()?),
        shm_size: u32::from_ne_bytes(buf[8..12].try_into().ok()?) as usize,
    })
}

/// Decode a `MESSAGE_USERINPUT` packet, if that is what `buf` holds.
///
/// `UserInputContents` sits at union offset 0 on the 32-bit ABI (it is an
/// all-`int` struct, so no `size_t`-driven alignment applies):
/// `inputType@4, devId@8, x@12, y@16, d@20`.
pub fn parse_input_event(buf: &[u8], n: usize) -> Option<InputEvent> {
    if n < MESSAGE_LEN || buf.len() < MESSAGE_LEN || buf[0] != MESSAGE_USERINPUT {
        return None;
    }
    Some(InputEvent {
        kind: i32::from_ne_bytes(buf[4..8].try_into().ok()?),
        dev_id: i32::from_ne_bytes(buf[8..12].try_into().ok()?),
        x: i32::from_ne_bytes(buf[12..16].try_into().ok()?),
        y: i32::from_ne_bytes(buf[16..20].try_into().ok()?),
        d: i32::from_ne_bytes(buf[20..24].try_into().ok()?),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framebuffer_key_is_parsed_from_a_decimal_string() {
        assert_eq!(parse_framebuffer_key("245209899").unwrap(), 245_209_899);
        assert_eq!(parse_framebuffer_key("  42\n").unwrap(), 42);
        assert_eq!(parse_framebuffer_key("-7").unwrap(), -7);
    }

    #[test]
    fn framebuffer_key_rejects_empty_and_non_numeric_values() {
        assert_eq!(parse_framebuffer_key(""), Err(KeyError::Missing));
        assert_eq!(parse_framebuffer_key("   "), Err(KeyError::Missing));
        assert!(matches!(
            parse_framebuffer_key("0x1234"),
            Err(KeyError::Invalid(_))
        ));
        // Out of i32 range: rejected rather than silently truncated.
        assert!(matches!(
            parse_framebuffer_key("99999999999"),
            Err(KeyError::Invalid(_))
        ));
    }

    #[test]
    fn shm_path_uses_the_key_the_host_reported() {
        assert_eq!(shm_path(245_209_899), "/dev/shm/qtfb_245209899");
        assert_eq!(shm_path(1), "/dev/shm/qtfb_1");
    }

    #[test]
    fn init_message_places_key_and_format_at_the_documented_offsets() {
        let msg = init_message(245_209_899, FBFMT_RM2FB);
        assert_eq!(msg.len(), MESSAGE_LEN);
        assert_eq!(msg[0], MESSAGE_INITIALIZE);
        assert_eq!(
            i32::from_ne_bytes(msg[4..8].try_into().unwrap()),
            245_209_899
        );
        assert_eq!(msg[8], FBFMT_RM2FB);
        // Bytes 1..4 are struct padding and must stay zeroed.
        assert_eq!(&msg[1..4], &[0, 0, 0]);
    }

    #[test]
    fn update_messages_encode_mode_and_rectangle() {
        let all = update_all_message();
        assert_eq!(all[0], MESSAGE_UPDATE);
        assert_eq!(
            i32::from_ne_bytes(all[4..8].try_into().unwrap()),
            UPDATE_ALL
        );

        let partial = update_partial_message(10, 20, 30, 40);
        assert_eq!(partial[0], MESSAGE_UPDATE);
        assert_eq!(
            i32::from_ne_bytes(partial[4..8].try_into().unwrap()),
            UPDATE_PARTIAL
        );
        let fields: Vec<i32> = partial[8..24]
            .chunks_exact(4)
            .map(|c| i32::from_ne_bytes(c.try_into().unwrap()))
            .collect();
        assert_eq!(fields, vec![10, 20, 30, 40]);
    }

    #[test]
    fn terminate_message_is_a_bare_type_byte() {
        let msg = terminate_message();
        assert_eq!(msg[0], MESSAGE_TERMINATE);
        assert!(msg[1..].iter().all(|&b| b == 0));
    }

    #[test]
    fn init_reply_is_decoded_from_the_32bit_layout() {
        let mut reply = [0u8; 32];
        reply[0] = MESSAGE_INITIALIZE;
        reply[4..8].copy_from_slice(&777_i32.to_ne_bytes());
        reply[8..12].copy_from_slice(&(1404u32 * 1872 * 2).to_ne_bytes());
        let parsed = parse_init_reply(&reply, 32).unwrap();
        assert_eq!(parsed.shm_key_defined, 777);
        assert_eq!(parsed.shm_size, 1404 * 1872 * 2);
    }

    #[test]
    fn init_reply_rejects_a_short_packet() {
        assert!(parse_init_reply(&[0u8; 32], 4).is_none());
        assert!(parse_init_reply(&[0u8; 8], 8).is_none());
    }

    #[test]
    fn user_input_packet_decodes_all_five_fields() {
        let mut buf = [0u8; 32];
        buf[0] = MESSAGE_USERINPUT;
        buf[4..8].copy_from_slice(&input_kind::PEN_UPDATE.to_ne_bytes());
        buf[8..12].copy_from_slice(&3_i32.to_ne_bytes());
        buf[12..16].copy_from_slice(&700_i32.to_ne_bytes());
        buf[16..20].copy_from_slice(&900_i32.to_ne_bytes());
        buf[20..24].copy_from_slice(&65_i32.to_ne_bytes());
        let ev = parse_input_event(&buf, 24).unwrap();
        assert_eq!(
            ev,
            InputEvent {
                kind: input_kind::PEN_UPDATE,
                dev_id: 3,
                x: 700,
                y: 900,
                d: 65,
            }
        );
    }

    #[test]
    fn non_user_input_and_short_packets_decode_to_none() {
        let mut buf = [0u8; 32];
        buf[0] = MESSAGE_UPDATE;
        assert!(parse_input_event(&buf, 24).is_none());
        buf[0] = MESSAGE_USERINPUT;
        assert!(parse_input_event(&buf, 20).is_none());
    }

    #[test]
    fn vkb_key_code_comes_from_x_not_d() {
        let ev = InputEvent {
            kind: input_kind::VKB_PRESS,
            dev_id: 0,
            x: 'q' as i32,
            y: 0,
            d: 0,
        };
        assert_eq!(ev.vkb_key_code(), 'q' as i32);
    }

    #[test]
    fn pen_pressure_is_a_percentage_scaled_to_unit_range() {
        let pen = |d| InputEvent {
            kind: input_kind::PEN_UPDATE,
            dev_id: 0,
            x: 0,
            y: 0,
            d,
        };
        assert_eq!(pen(0).pen_pressure(), 0.0);
        assert_eq!(pen(50).pen_pressure(), 0.5);
        assert_eq!(pen(100).pen_pressure(), 1.0);
        // Defensive clamping against an out-of-spec host value.
        assert_eq!(pen(255).pen_pressure(), 1.0);
        assert_eq!(pen(-5).pen_pressure(), 0.0);
    }
}
