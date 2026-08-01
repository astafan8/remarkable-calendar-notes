//! Decoding of AppLoad's virtual-keyboard (VKB) input events into editable
//! text, for the in-app settings/source editor. This is the only text
//! input mechanism the app uses — there is no on-device physical keyboard,
//! and AppLoad's window chrome is what draws/shows the virtual keyboard
//! (enabled by `supportsVirtualKeyboard: true` in `external.manifest.json`).
//!
//! The event kind (`INPUT_VKB_PRESS`) and modifier/special-key constants
//! (`INPUT_VKB_SHIFTMOD`, `INPUT_VKB_DEL`, arrow keys, etc.) are the same
//! fixed protocol facts referenced in `calnotes-device`'s QTFB client; this
//! module only depends on the resulting raw `i32` code, so it stays usable
//! and testable without any device or socket connection.

use serde::{Deserialize, Serialize};

const SHIFT_MOD: i32 = 0x100000;
const CTRL_MOD: i32 = 0x200000;
const ALT_MOD: i32 = 0x400000;
const MOD_MASK: i32 = SHIFT_MOD | CTRL_MOD | ALT_MOD;

const KEY_DEL: i32 = 0x7f;
const KEY_PGUP: i32 = 0x80;
const KEY_PGDOWN: i32 = 0x81;
const KEY_DOWN: i32 = 0x82;
const KEY_UP: i32 = 0x83;
const KEY_LEFT: i32 = 0x84;
const KEY_RIGHT: i32 = 0x85;
const KEY_HOME: i32 = 0x86;
const KEY_END: i32 = 0x87;
const KEY_BACKSPACE: i32 = 0x08;
const KEY_ENTER: i32 = 0x0d;
const KEY_ENTER_LF: i32 = 0x0a;
const KEY_TAB: i32 = 0x09;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VkbKey {
    Char(char),
    Backspace,
    Delete,
    Enter,
    Tab,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Home,
    End,
    PageUp,
    PageDown,
    Unknown,
}

/// Decode one raw VKB key code into a key and its modifier state.
///
/// The raw code is the `x` field of an `INPUT_VKB_PRESS` event — QTFB
/// packs the key code there, not in the event's `d` field (see
/// `calnotes_device::protocol::InputEvent::vkb_key_code`). This function
/// only depends on the resulting `i32`, so it stays device-free.
pub fn decode(raw: i32) -> (VkbKey, Modifiers) {
    let modifiers = Modifiers {
        shift: raw & SHIFT_MOD != 0,
        ctrl: raw & CTRL_MOD != 0,
        alt: raw & ALT_MOD != 0,
    };
    let code = raw & !MOD_MASK;
    let key = match code {
        KEY_DEL => VkbKey::Delete,
        KEY_BACKSPACE => VkbKey::Backspace,
        KEY_ENTER | KEY_ENTER_LF => VkbKey::Enter,
        KEY_TAB => VkbKey::Tab,
        KEY_PGUP => VkbKey::PageUp,
        KEY_PGDOWN => VkbKey::PageDown,
        KEY_DOWN => VkbKey::ArrowDown,
        KEY_UP => VkbKey::ArrowUp,
        KEY_LEFT => VkbKey::ArrowLeft,
        KEY_RIGHT => VkbKey::ArrowRight,
        KEY_HOME => VkbKey::Home,
        KEY_END => VkbKey::End,
        c if c >= 0x20 => char::from_u32(c as u32)
            .map(VkbKey::Char)
            .unwrap_or(VkbKey::Unknown),
        _ => VkbKey::Unknown,
    };
    (key, modifiers)
}

/// A single-line editable text field with a cursor, driven entirely by
/// decoded VKB keys — used for every text input in the settings/source
/// editor (labels, URLs, paths, client IDs, passwords).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TextField {
    pub text: String,
    pub cursor: usize,
}

impl TextField {
    pub fn new(initial: impl Into<String>) -> Self {
        let text = initial.into();
        let cursor = text.chars().count();
        TextField { text, cursor }
    }

    fn char_indices_boundaries(&self) -> Vec<usize> {
        let mut bounds: Vec<usize> = self.text.char_indices().map(|(i, _)| i).collect();
        bounds.push(self.text.len());
        bounds
    }

    /// Apply one decoded key press, editing `text`/`cursor` in place.
    /// Returns `true` if the key was handled (consumed) by this field.
    pub fn apply_key(&mut self, key: VkbKey) -> bool {
        let bounds = self.char_indices_boundaries();
        match key {
            VkbKey::Char(c) => {
                let byte_pos = bounds[self.cursor];
                self.text.insert(byte_pos, c);
                self.cursor += 1;
                true
            }
            VkbKey::Backspace => {
                if self.cursor > 0 {
                    let start = bounds[self.cursor - 1];
                    let end = bounds[self.cursor];
                    self.text.replace_range(start..end, "");
                    self.cursor -= 1;
                }
                true
            }
            VkbKey::Delete => {
                if self.cursor < bounds.len() - 1 {
                    let start = bounds[self.cursor];
                    let end = bounds[self.cursor + 1];
                    self.text.replace_range(start..end, "");
                }
                true
            }
            VkbKey::ArrowLeft => {
                self.cursor = self.cursor.saturating_sub(1);
                true
            }
            VkbKey::ArrowRight => {
                self.cursor = (self.cursor + 1).min(bounds.len() - 1);
                true
            }
            VkbKey::Home => {
                self.cursor = 0;
                true
            }
            VkbKey::End => {
                self.cursor = bounds.len() - 1;
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_extracts_shift_modifier_and_uppercase_character_code() {
        let (key, mods) = decode('a' as i32 | SHIFT_MOD);
        assert_eq!(key, VkbKey::Char('a'));
        assert!(mods.shift);
        assert!(!mods.ctrl);
    }

    #[test]
    fn decode_recognizes_special_keys() {
        assert_eq!(decode(KEY_DEL).0, VkbKey::Delete);
        assert_eq!(decode(KEY_LEFT).0, VkbKey::ArrowLeft);
        assert_eq!(decode(KEY_HOME).0, VkbKey::Home);
        assert_eq!(decode(KEY_END).0, VkbKey::End);
    }

    #[test]
    fn text_field_types_characters_at_cursor() {
        let mut field = TextField::new("helo");
        field.cursor = 3; // after "hel"
        field.apply_key(VkbKey::Char('l'));
        assert_eq!(field.text, "hello");
        assert_eq!(field.cursor, 4);
    }

    #[test]
    fn text_field_backspace_removes_character_before_cursor() {
        let mut field = TextField::new("hello");
        field.apply_key(VkbKey::Backspace);
        assert_eq!(field.text, "hell");
        assert_eq!(field.cursor, 4);
    }

    #[test]
    fn text_field_delete_removes_character_after_cursor() {
        let mut field = TextField::new("hello");
        field.cursor = 0;
        field.apply_key(VkbKey::Delete);
        assert_eq!(field.text, "ello");
        assert_eq!(field.cursor, 0);
    }

    #[test]
    fn text_field_arrow_keys_move_cursor_within_bounds() {
        let mut field = TextField::new("ab");
        field.cursor = 0;
        field.apply_key(VkbKey::ArrowLeft);
        assert_eq!(field.cursor, 0); // clamped
        field.apply_key(VkbKey::ArrowRight);
        field.apply_key(VkbKey::ArrowRight);
        field.apply_key(VkbKey::ArrowRight);
        assert_eq!(field.cursor, 2); // clamped to length
    }

    #[test]
    fn text_field_handles_multibyte_characters_correctly() {
        let mut field = TextField::new("café");
        field.cursor = field.text.chars().count();
        field.apply_key(VkbKey::Backspace);
        assert_eq!(field.text, "caf");
    }

    #[test]
    fn text_field_home_and_end_jump_to_bounds() {
        let mut field = TextField::new("hello");
        field.apply_key(VkbKey::Home);
        assert_eq!(field.cursor, 0);
        field.apply_key(VkbKey::End);
        assert_eq!(field.cursor, 5);
    }
}
