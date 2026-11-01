//! Translating Deskflow key events into USB HID keyboard usages (page 0x07).
//!
//! Each `DKDN`/`DKUP` carries two identities for the key:
//!
//! * `id` — a Deskflow `KeyID`, i.e. the *character* the server's layout
//!   produced (`'a'`, `'!'`, `kKeyLeft`, …). Characters composed with Option on
//!   macOS (`£`, `é`) arrive as their Unicode value and have no HID usage.
//! * `button` — the server's *physical* key, in the server OS's own numbering:
//!   * macOS: virtual keycode + 1 (`OSXKeyState::mapVirtualKeyToKeyButton`);
//!   * Windows: PC scan code, with 0x100 set for E0-prefixed keys
//!     (`MSWindowsComputer`, from the key message's `lParam`);
//!   * Linux: evdev keycode + 8, i.e. the X11 keycode, on both the X11
//!     (`XWindowsComputer`) and Wayland (`EiComputer`) backends.
//!
//! Forwarding physical keys is what a real keyboard does, so the laptop applies
//! its own layout and shortcuts like Alt+3 keep working. [`KeySource`] selects
//! the strategy; the physical ones fall back to the `KeyID` table for codes
//! they do not know.
//!
//! Media keys (volume, playback, brightness, …) aren't keyboard usages at all:
//! [`consumer_usage_for`] maps them, by `KeyID`, to the consumer-control
//! report, whatever the key source.

/// Which field of a key event to translate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySource {
    /// `button` is a macOS virtual keycode + 1 (Deskflow server on a Mac).
    MacPhysical,
    /// `button` is a PC scan code (Deskflow server on Windows).
    WindowsPhysical,
    /// `button` is an X11 keycode, evdev + 8 (Deskflow server on Linux).
    LinuxPhysical,
    /// Only use the character `KeyID`; works with any server OS for US layouts.
    KeyId,
}

/// HID usage for a key event, or `None` if it cannot be represented.
pub fn usage_for(source: KeySource, id: u16, button: u16) -> Option<u8> {
    let physical = match source {
        KeySource::MacPhysical => button.checked_sub(1).and_then(mac_vk_to_usage),
        KeySource::WindowsPhysical => windows_scan_to_usage(button),
        KeySource::LinuxPhysical => button.checked_sub(8).and_then(evdev_to_usage),
        KeySource::KeyId => None,
    };
    physical.or_else(|| key_id_to_usage(id))
}

/// Consumer-control usage (HID page 0x0C) for a media key, by `KeyID`.
pub fn consumer_usage_for(id: u16) -> Option<u16> {
    let usage = match id {
        0xE001 => 0x00B8, // Eject
        0xE05F => 0x0032, // Sleep
        0xE0A6 => 0x0224, // WWW Back → AC Back
        0xE0A7 => 0x0225, // WWW Forward → AC Forward
        0xE0A8 => 0x0227, // WWW Refresh → AC Refresh
        0xE0A9 => 0x0226, // WWW Stop → AC Stop
        0xE0AA => 0x0221, // WWW Search → AC Search
        0xE0AB => 0x022A, // WWW Favorites → AC Bookmarks
        0xE0AC => 0x0223, // WWW Home → AC Home
        0xE0AD => 0x00E2, // Mute
        0xE0AE => 0x00EA, // Volume Down
        0xE0AF => 0x00E9, // Volume Up
        0xE0B0 => 0x00B5, // Next Track
        0xE0B1 => 0x00B6, // Previous Track
        0xE0B2 => 0x00B7, // Stop
        0xE0B3 => 0x00CD, // Play/Pause
        0xE0B4 => 0x018A, // Mail → AL Email Reader
        0xE0B5 => 0x0183, // Media → AL Consumer Control Configuration
        0xE0B6 => 0x0192, // App 1 → AL Calculator
        0xE0B7 => 0x0194, // App 2 → AL Local Machine Browser
        0xE0B8 => 0x0070, // Brightness Down
        0xE0B9 => 0x006F, // Brightness Up
        _ => return None,
    };
    Some(usage)
}

/// HID usages 0xE0..=0xE7 are the eight modifier keys; boot-protocol keyboard
/// reports carry them as a bitmask instead of in the key array.
pub fn modifier_bit(usage: u8) -> Option<u8> {
    (0xE0..=0xE7).contains(&usage).then(|| 1 << (usage - 0xE0))
}

/// macOS `kVK_*` virtual keycode → HID usage.
fn mac_vk_to_usage(vk: u16) -> Option<u8> {
    let usage = match vk {
        0x00 => 0x04, // A
        0x01 => 0x16, // S
        0x02 => 0x07, // D
        0x03 => 0x09, // F
        0x04 => 0x0B, // H
        0x05 => 0x0A, // G
        0x06 => 0x1D, // Z
        0x07 => 0x1B, // X
        0x08 => 0x06, // C
        0x09 => 0x19, // V
        0x0A => 0x64, // ISO section key → Non-US backslash
        0x0B => 0x05, // B
        0x0C => 0x14, // Q
        0x0D => 0x1A, // W
        0x0E => 0x08, // E
        0x0F => 0x15, // R
        0x10 => 0x1C, // Y
        0x11 => 0x17, // T
        0x12 => 0x1E, // 1
        0x13 => 0x1F, // 2
        0x14 => 0x20, // 3
        0x15 => 0x21, // 4
        0x16 => 0x23, // 6
        0x17 => 0x22, // 5
        0x18 => 0x2E, // =
        0x19 => 0x26, // 9
        0x1A => 0x24, // 7
        0x1B => 0x2D, // -
        0x1C => 0x25, // 8
        0x1D => 0x27, // 0
        0x1E => 0x30, // ]
        0x1F => 0x12, // O
        0x20 => 0x18, // U
        0x21 => 0x2F, // [
        0x22 => 0x0C, // I
        0x23 => 0x13, // P
        0x24 => 0x28, // Return
        0x25 => 0x0F, // L
        0x26 => 0x0D, // J
        0x27 => 0x34, // '
        0x28 => 0x0E, // K
        0x29 => 0x33, // ;
        0x2A => 0x31, // \
        0x2B => 0x36, // ,
        0x2C => 0x38, // /
        0x2D => 0x11, // N
        0x2E => 0x10, // M
        0x2F => 0x37, // .
        0x30 => 0x2B, // Tab
        0x31 => 0x2C, // Space
        0x32 => 0x35, // `
        0x33 => 0x2A, // Delete (backspace)
        0x35 => 0x29, // Escape
        0x36 => 0xE7, // Right Command → Right GUI
        0x37 => 0xE3, // Command → Left GUI
        0x38 => 0xE1, // Shift
        0x39 => 0x39, // Caps Lock
        0x3A => 0xE2, // Option → Left Alt
        0x3B => 0xE0, // Control
        0x3C => 0xE5, // Right Shift
        0x3D => 0xE6, // Right Option → Right Alt
        0x3E => 0xE4, // Right Control
        0x40 => 0x6C, // F17
        0x41 => 0x63, // Keypad .
        0x43 => 0x55, // Keypad *
        0x45 => 0x57, // Keypad +
        0x47 => 0x53, // Keypad Clear → Num Lock
        0x48 => 0x80, // Volume Up
        0x49 => 0x81, // Volume Down
        0x4A => 0x7F, // Mute
        0x4B => 0x54, // Keypad /
        0x4C => 0x58, // Keypad Enter
        0x4E => 0x56, // Keypad -
        0x4F => 0x6D, // F18
        0x50 => 0x6E, // F19
        0x51 => 0x67, // Keypad =
        0x52 => 0x62, // Keypad 0
        0x53..=0x59 => 0x59 + (vk - 0x53) as u8, // Keypad 1..7
        0x5A => 0x6F, // F20
        0x5B => 0x60, // Keypad 8
        0x5C => 0x61, // Keypad 9
        0x5D => 0x89, // JIS Yen → International3
        0x5E => 0x87, // JIS Underscore → International1
        0x5F => 0x85, // JIS Keypad comma
        0x60 => 0x3E, // F5
        0x61 => 0x3F, // F6
        0x62 => 0x40, // F7
        0x63 => 0x3C, // F3
        0x64 => 0x41, // F8
        0x65 => 0x42, // F9
        0x66 => 0x91, // JIS Eisu → LANG2
        0x67 => 0x44, // F11
        0x68 => 0x90, // JIS Kana → LANG1
        0x69 => 0x68, // F13
        0x6A => 0x6B, // F16
        0x6B => 0x69, // F14
        0x6D => 0x43, // F10
        0x6F => 0x45, // F12
        0x71 => 0x6A, // F15
        0x72 => 0x49, // Help → Insert (same position on PC keyboards)
        0x73 => 0x4A, // Home
        0x74 => 0x4B, // Page Up
        0x75 => 0x4C, // Forward Delete
        0x76 => 0x3D, // F4
        0x77 => 0x4D, // End
        0x78 => 0x3B, // F2
        0x79 => 0x4E, // Page Down
        0x7A => 0x3A, // F1
        0x7B => 0x50, // Left
        0x7C => 0x4F, // Right
        0x7D => 0x51, // Down
        0x7E => 0x52, // Up
        _ => return None,
    };
    Some(usage)
}

/// PC scan code (set 1) → HID usage for the codes Windows scan codes and Linux
/// evdev codes share: evdev numbers the main block exactly like set 1. Num
/// Lock (69 / 0x45) is left to the callers: Windows reports Pause with the
/// same scan code.
fn pc_base_to_usage(code: u16) -> Option<u8> {
    let usage = match code {
        1 => 0x29,                                // Esc
        2..=10 => 0x1E + (code - 2) as u8,        // 1..9
        11 => 0x27,                               // 0
        12 => 0x2D,                               // -
        13 => 0x2E,                               // =
        14 => 0x2A,                               // Backspace
        15 => 0x2B,                               // Tab
        16 => 0x14,                               // Q
        17 => 0x1A,                               // W
        18 => 0x08,                               // E
        19 => 0x15,                               // R
        20 => 0x17,                               // T
        21 => 0x1C,                               // Y
        22 => 0x18,                               // U
        23 => 0x0C,                               // I
        24 => 0x12,                               // O
        25 => 0x13,                               // P
        26 => 0x2F,                               // [
        27 => 0x30,                               // ]
        28 => 0x28,                               // Enter
        29 => 0xE0,                               // Left Ctrl
        30 => 0x04,                               // A
        31 => 0x16,                               // S
        32 => 0x07,                               // D
        33 => 0x09,                               // F
        34 => 0x0A,                               // G
        35 => 0x0B,                               // H
        36 => 0x0D,                               // J
        37 => 0x0E,                               // K
        38 => 0x0F,                               // L
        39 => 0x33,                               // ;
        40 => 0x34,                               // '
        41 => 0x35,                               // `
        42 => 0xE1,                               // Left Shift
        43 => 0x31,                               // \ (ISO: #, which Linux treats the same)
        44 => 0x1D,                               // Z
        45 => 0x1B,                               // X
        46 => 0x06,                               // C
        47 => 0x19,                               // V
        48 => 0x05,                               // B
        49 => 0x11,                               // N
        50 => 0x10,                               // M
        51 => 0x36,                               // ,
        52 => 0x37,                               // .
        53 => 0x38,                               // /
        54 => 0xE5,                               // Right Shift
        55 => 0x55,                               // Keypad *
        56 => 0xE2,                               // Left Alt
        57 => 0x2C,                               // Space
        58 => 0x39,                               // Caps Lock
        59..=68 => 0x3A + (code - 59) as u8,      // F1..F10
        70 => 0x47,                               // Scroll Lock
        71 => 0x5F,                               // Keypad 7
        72 => 0x60,                               // Keypad 8
        73 => 0x61,                               // Keypad 9
        74 => 0x56,                               // Keypad -
        75 => 0x5C,                               // Keypad 4
        76 => 0x5D,                               // Keypad 5
        77 => 0x5E,                               // Keypad 6
        78 => 0x57,                               // Keypad +
        79 => 0x59,                               // Keypad 1
        80 => 0x5A,                               // Keypad 2
        81 => 0x5B,                               // Keypad 3
        82 => 0x62,                               // Keypad 0
        83 => 0x63,                               // Keypad .
        86 => 0x64,                               // ISO key left of Z
        87 => 0x44,                               // F11
        88 => 0x45,                               // F12
        _ => return None,
    };
    Some(usage)
}

/// Windows scan code (0x100 = E0 prefix) → HID usage. Media keys are left to
/// [`consumer_usage_for`], and Pause/Num Lock (both 0x45, told apart by the
/// prefix inconsistently across Windows versions) to the `KeyID`.
fn windows_scan_to_usage(code: u16) -> Option<u8> {
    let usage = match code {
        0x45 | 0x145 => return None,
        0x54 => 0x46,                              // SysRq (Alt+Print Screen)
        0x64..=0x6E => 0x68 + (code - 0x64) as u8, // F13..F23
        0x76 => 0x73,                              // F24
        0x70 => 0x88,                              // Katakana/Hiragana
        0x73 => 0x87,                              // Ro
        0x79 => 0x8A,                              // Henkan
        0x7B => 0x8B,                              // Muhenkan
        0x7D => 0x89,                              // Yen
        0x7E => 0x85,                              // Keypad , (Brazil)
        0x11C => 0x58,                             // Keypad Enter
        0x11D => 0xE4,                             // Right Ctrl
        0x135 => 0x54,                             // Keypad /
        0x137 => 0x46,                             // Print Screen
        0x138 => 0xE6,                             // Right Alt / AltGr
        0x147 => 0x4A,                             // Home
        0x148 => 0x52,                             // Up
        0x149 => 0x4B,                             // Page Up
        0x14B => 0x50,                             // Left
        0x14D => 0x4F,                             // Right
        0x14F => 0x4D,                             // End
        0x150 => 0x51,                             // Down
        0x151 => 0x4E,                             // Page Down
        0x152 => 0x49,                             // Insert
        0x153 => 0x4C,                             // Delete
        0x15B => 0xE3,                             // Left Windows
        0x15C => 0xE7,                             // Right Windows
        0x15D => 0x65,                             // Menu
        0x00..=0xFF => return pc_base_to_usage(code),
        _ => return None,
    };
    Some(usage)
}

/// Linux evdev keycode (`input-event-codes.h`) → HID usage. Media keys are
/// left to [`consumer_usage_for`].
fn evdev_to_usage(code: u16) -> Option<u8> {
    let usage = match code {
        69 => 0x53,                               // Num Lock
        89 => 0x87,                               // Ro
        92 => 0x8A,                               // Henkan
        93 => 0x88,                               // Katakana/Hiragana
        94 => 0x8B,                               // Muhenkan
        96 => 0x58,                               // Keypad Enter
        97 => 0xE4,                               // Right Ctrl
        98 => 0x54,                               // Keypad /
        99 => 0x46,                               // SysRq / Print Screen
        100 => 0xE6,                              // Right Alt / AltGr
        102 => 0x4A,                              // Home
        103 => 0x52,                              // Up
        104 => 0x4B,                              // Page Up
        105 => 0x50,                              // Left
        106 => 0x4F,                              // Right
        107 => 0x4D,                              // End
        108 => 0x51,                              // Down
        109 => 0x4E,                              // Page Down
        110 => 0x49,                              // Insert
        111 => 0x4C,                              // Delete
        117 => 0x67,                              // Keypad =
        119 => 0x48,                              // Pause
        121 => 0x85,                              // Keypad ,
        122 => 0x90,                              // Hangeul
        123 => 0x91,                              // Hanja
        124 => 0x89,                              // Yen
        125 => 0xE3,                              // Left Meta
        126 => 0xE7,                              // Right Meta
        127 => 0x65,                              // Compose / Menu
        183..=194 => 0x68 + (code - 183) as u8,   // F13..F24
        _ => return pc_base_to_usage(code),
    };
    Some(usage)
}

/// Deskflow `KeyID` (`src/lib/deskflow/KeyTypes.h`) → HID usage, assuming a
/// US layout on the receiving machine. Shifted symbols map to their unshifted
/// key because Deskflow sends the Shift press as its own event.
fn key_id_to_usage(id: u16) -> Option<u8> {
    let usage = match id {
        0x61..=0x7A => 0x04 + (id - 0x61) as u8, // a..z
        0x41..=0x5A => 0x04 + (id - 0x41) as u8, // A..Z
        0x31..=0x39 => 0x1E + (id - 0x31) as u8, // 1..9
        0x30 => 0x27,
        0x21 => 0x1E,        // !
        0x40 => 0x1F,        // @
        0x23 => 0x20,        // #
        0x24 => 0x21,        // $
        0x25 => 0x22,        // %
        0x5E => 0x23,        // ^
        0x26 => 0x24,        // &
        0x2A => 0x25,        // *
        0x28 => 0x26,        // (
        0x29 => 0x27,        // )
        0x20 => 0x2C,        // space
        0x2D | 0x5F => 0x2D, // - _
        0x3D | 0x2B => 0x2E, // = +
        0x5B | 0x7B => 0x2F, // [ {
        0x5D | 0x7D => 0x30, // ] }
        0x5C | 0x7C => 0x31, // \ |
        0x3B | 0x3A => 0x33, // ; :
        0x27 | 0x22 => 0x34, // ' "
        0x60 | 0x7E => 0x35, // ` ~
        0x2C | 0x3C => 0x36, // , <
        0x2E | 0x3E => 0x37, // . >
        0x2F | 0x3F => 0x38, // / ?
        0xEF08 => 0x2A,      // BackSpace
        0xEF09 => 0x2B,      // Tab
        0xEF0D => 0x28,      // Return
        0xEF13 => 0x48,      // Pause
        0xEF14 => 0x47,      // Scroll Lock
        0xEF1B => 0x29,      // Escape
        0xEF50 => 0x4A,      // Home
        0xEF51 => 0x50,      // Left
        0xEF52 => 0x52,      // Up
        0xEF53 => 0x4F,      // Right
        0xEF54 => 0x51,      // Down
        0xEF55 => 0x4B,      // Page Up
        0xEF56 => 0x4E,      // Page Down
        0xEF57 => 0x4D,      // End
        0xEF61 => 0x46,      // Print
        0xEF63 => 0x49,      // Insert
        0xEF67 => 0x65,      // Menu → Application
        0xEF7E => 0xE6,      // AltGr → Right Alt
        0xEF7F => 0x53,      // Num Lock
        0xEF8D => 0x58,      // Keypad Enter
        0xEFAA => 0x55,      // Keypad *
        0xEFAB => 0x57,      // Keypad +
        0xEFAD => 0x56,      // Keypad -
        0xEFAE => 0x63,      // Keypad .
        0xEFAF => 0x54,      // Keypad /
        0xEFB0 => 0x62,      // Keypad 0
        0xEFB1..=0xEFB9 => 0x59 + (id - 0xEFB1) as u8, // Keypad 1..9
        0xEFBD => 0x67,      // Keypad =
        0xEFBE..=0xEFC9 => 0x3A + (id - 0xEFBE) as u8, // F1..F12
        0xEFCA..=0xEFD5 => 0x68 + (id - 0xEFCA) as u8, // F13..F24
        0xEFE1 => 0xE1,      // Shift L
        0xEFE2 => 0xE5,      // Shift R
        0xEFE3 => 0xE0,      // Control L
        0xEFE4 => 0xE4,      // Control R
        0xEFE5 => 0x39,      // Caps Lock
        0xEFE7 => 0xE3,      // Meta L → Left GUI
        0xEFE8 => 0xE7,      // Meta R → Right GUI
        0xEFE9 => 0xE2,      // Alt L
        0xEFEA => 0xE6,      // Alt R
        0xEFEB => 0xE3,      // Super L → Left GUI
        0xEFEC => 0xE7,      // Super R → Right GUI
        0xEFFF => 0x4C,      // Delete
        _ => return None,
    };
    Some(usage)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_physical_letters_and_modifiers() {
        // kVK_ANSI_A is 0x00, so Deskflow sends button 1.
        assert_eq!(usage_for(KeySource::MacPhysical, 0x61, 1), Some(0x04));
        assert_eq!(usage_for(KeySource::MacPhysical, 0xEFEB, 0x37 + 1), Some(0xE3));
    }

    #[test]
    fn option_composed_character_still_maps_physically() {
        // Option+3 on a US Mac produces '£' (0xA3), which has no KeyID mapping.
        assert_eq!(usage_for(KeySource::KeyId, 0xA3, 0x14 + 1), None);
        assert_eq!(usage_for(KeySource::MacPhysical, 0xA3, 0x14 + 1), Some(0x20));
    }

    #[test]
    fn falls_back_to_key_id() {
        assert_eq!(usage_for(KeySource::MacPhysical, 0xEFC9, 0), Some(0x45)); // F12
    }

    #[test]
    fn key_id_shifted_symbols_use_base_key() {
        assert_eq!(usage_for(KeySource::KeyId, b'!' as u16, 0), usage_for(KeySource::KeyId, b'1' as u16, 0));
        assert_eq!(usage_for(KeySource::KeyId, b'Q' as u16, 0), Some(0x14));
    }

    #[test]
    fn keypad_ranges() {
        assert_eq!(usage_for(KeySource::KeyId, 0xEFB9, 0), Some(0x61)); // KP 9
        assert_eq!(usage_for(KeySource::MacPhysical, 0, 0x59 + 1), Some(0x5F)); // KP 7
    }

    /// The same physical keys in each server's numbering: (macOS kVK + 1,
    /// Windows scan code, X11 keycode = evdev + 8, HID usage).
    #[allow(clippy::identity_op)] // `0x00 + 1` reads as "kVK + 1"
    const SAME_KEYS: &[(u16, u16, u16, u8, &str)] = &[
        (0x00 + 1, 0x1E, 30 + 8, 0x04, "A"),
        (0x0C + 1, 0x10, 16 + 8, 0x14, "Q"),
        (0x06 + 1, 0x2C, 44 + 8, 0x1D, "Z"),
        (0x12 + 1, 0x02, 2 + 8, 0x1E, "1"),
        (0x1D + 1, 0x0B, 11 + 8, 0x27, "0"),
        (0x24 + 1, 0x1C, 28 + 8, 0x28, "Return"),
        (0x35 + 1, 0x01, 1 + 8, 0x29, "Escape"),
        (0x31 + 1, 0x39, 57 + 8, 0x2C, "Space"),
        (0x2A + 1, 0x2B, 43 + 8, 0x31, "Backslash"),
        (0x0A + 1, 0x56, 86 + 8, 0x64, "ISO extra key"),
        (0x7A + 1, 0x3B, 59 + 8, 0x3A, "F1"),
        (0x6F + 1, 0x58, 88 + 8, 0x45, "F12"),
        (0x7B + 1, 0x14B, 105 + 8, 0x50, "Left"),
        (0x7E + 1, 0x148, 103 + 8, 0x52, "Up"),
        (0x75 + 1, 0x153, 111 + 8, 0x4C, "Forward Delete"),
        (0x3B + 1, 0x1D, 29 + 8, 0xE0, "Left Control"),
        (0x38 + 1, 0x2A, 42 + 8, 0xE1, "Left Shift"),
        (0x3A + 1, 0x38, 56 + 8, 0xE2, "Left Option/Alt"),
        (0x37 + 1, 0x15B, 125 + 8, 0xE3, "Left Command/Windows/Meta"),
        (0x3D + 1, 0x138, 100 + 8, 0xE6, "Right Option/AltGr"),
        (0x4C + 1, 0x11C, 96 + 8, 0x58, "Keypad Enter"),
        (0x59 + 1, 0x47, 71 + 8, 0x5F, "Keypad 7"),
    ];

    #[test]
    fn physical_keys_agree_across_servers() {
        for &(mac, win, x11, usage, name) in SAME_KEYS {
            // KeyID 0: nothing to fall back to, so this is the physical table.
            assert_eq!(usage_for(KeySource::MacPhysical, 0, mac), Some(usage), "macOS {}", name);
            assert_eq!(usage_for(KeySource::WindowsPhysical, 0, win), Some(usage), "Windows {}", name);
            assert_eq!(usage_for(KeySource::LinuxPhysical, 0, x11), Some(usage), "Linux {}", name);
        }
    }

    #[test]
    fn pc_base_table_is_one_to_one() {
        let mut seen = std::collections::BTreeMap::new();
        for code in 0..=0xFF {
            if let Some(u) = pc_base_to_usage(code) {
                assert!(seen.insert(u, code).is_none(), "usage {:#x} from {} and {}", u, seen[&u], code);
            }
        }
        assert_eq!(seen.len(), 85);
    }

    #[test]
    fn windows_pause_and_num_lock_go_by_key_id() {
        const PAUSE: u16 = 0xEF13;
        const NUM_LOCK: u16 = 0xEF7F;
        assert_eq!(usage_for(KeySource::WindowsPhysical, PAUSE, 0x45), Some(0x48));
        assert_eq!(usage_for(KeySource::WindowsPhysical, NUM_LOCK, 0x145), Some(0x53));
        assert_eq!(usage_for(KeySource::LinuxPhysical, NUM_LOCK, 69 + 8), Some(0x53));
        assert_eq!(usage_for(KeySource::LinuxPhysical, PAUSE, 119 + 8), Some(0x48));
    }

    #[test]
    fn non_us_characters_still_map_physically() {
        // German layout: the key right of 0 types ß (0xDF), which has no KeyID
        // mapping, but it's the US '-' key physically.
        assert_eq!(usage_for(KeySource::KeyId, 0xDF, 0x0C), None);
        assert_eq!(usage_for(KeySource::WindowsPhysical, 0xDF, 0x0C), Some(0x2D));
        assert_eq!(usage_for(KeySource::LinuxPhysical, 0xDF, 12 + 8), Some(0x2D));
    }

    #[test]
    fn linux_keycodes_below_8_are_not_keys() {
        assert_eq!(usage_for(KeySource::LinuxPhysical, 0, 3), None);
        assert_eq!(usage_for(KeySource::LinuxPhysical, 0, 183 + 8), Some(0x68)); // F13
    }

    #[test]
    fn media_keys_map_to_consumer_usages() {
        assert_eq!(consumer_usage_for(0xE0AF), Some(0xE9)); // Volume Up
        assert_eq!(consumer_usage_for(0xE0B3), Some(0xCD)); // Play/Pause
        assert_eq!(consumer_usage_for(0xE0B9), Some(0x6F)); // Brightness Up
        assert_eq!(consumer_usage_for(u16::from(b'a')), None);
        // Every usage fits the consumer report's logical range.
        for id in 0xE000..=0xE0FF {
            if let Some(u) = consumer_usage_for(id) {
                assert!(u <= 0x3FF);
            }
        }
    }

    #[test]
    fn modifier_bits() {
        assert_eq!(modifier_bit(0xE0), Some(0x01));
        assert_eq!(modifier_bit(0xE7), Some(0x80));
        assert_eq!(modifier_bit(0x04), None);
    }
}
