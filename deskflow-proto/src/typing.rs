//! Typing text on the laptop as keystrokes, for the clipboard.
//!
//! A keyboard sends key positions, not characters, so the bridge needs to
//! know the laptop's layout ([`Layout`]) to pick the key, Shift and AltGr for
//! each character. The tables come from Linux's own layout data
//! (xkeyboard-config, via `tools/gen_layouts.py`). Characters a layout can't
//! type directly (only through dead keys, or not at all) are skipped.

use crate::hid::{KEYBOARD_REPORT_LEN, Report};
use crate::keymap::KeySource;
use crate::layouts;

/// Deskflow modifier mask bits (`KeyTypes.h`).
const MASK_CONTROL: u16 = 0x0002;
const MASK_ALT: u16 = 0x0004;
const MASK_META: u16 = 0x0008;
const MASK_SUPER: u16 = 0x0010;

/// The V key's `button` per server OS (see [`crate::keymap`]): macOS
/// `kVK_ANSI_V` + 1, the PC scan code, and Linux `KEY_V` + 8.
const MAC_V_BUTTON: u16 = 0x09 + 1;
const WINDOWS_V_BUTTON: u16 = 0x2F;
const LINUX_V_BUTTON: u16 = 47 + 8;
/// `kVK_ANSI_1` to `kVK_ANSI_4`, plus one.
const MAC_DIGIT_BUTTONS: [u16; 4] = [0x13, 0x14, 0x15, 0x16];

/// Whether a key press is the "type the clipboard" hotkey: Ctrl+Alt+V
/// (Ctrl+Option+V on a Mac keyboard), Shift allowed, Cmd/Super not.
pub fn is_paste_hotkey(source: KeySource, id: u16, mask: u16, button: u16) -> bool {
    let mods = mask & (MASK_CONTROL | MASK_ALT | MASK_META | MASK_SUPER);
    if mods != MASK_CONTROL | MASK_ALT {
        return false;
    }
    match source {
        // With Option held the Mac reports a different character (√), so
        // go by the physical key.
        KeySource::MacPhysical => button == MAC_V_BUTTON,
        KeySource::WindowsPhysical => button == WINDOWS_V_BUTTON,
        KeySource::LinuxPhysical => button == LINUX_V_BUTTON,
        KeySource::KeyId => id == u16::from(b'v') || id == u16::from(b'V'),
    }
}

/// The laptop a key press switches to, counting from 0: Ctrl+Alt+1…4
/// (Ctrl+Option on a Mac keyboard), Shift allowed, Cmd/Super not.
pub fn laptop_hotkey(source: KeySource, id: u16, mask: u16, button: u16) -> Option<u8> {
    let mods = mask & (MASK_CONTROL | MASK_ALT | MASK_META | MASK_SUPER);
    if mods != MASK_CONTROL | MASK_ALT {
        return None;
    }
    // Go by the physical key: with Ctrl+Alt held the character can be anything.
    match source {
        KeySource::MacPhysical => MAC_DIGIT_BUTTONS.iter().position(|&b| b == button).map(|i| i as u8),
        KeySource::WindowsPhysical => (2..=5).contains(&button).then(|| (button - 2) as u8),
        KeySource::LinuxPhysical => (2 + 8..=5 + 8).contains(&button).then(|| (button - 10) as u8),
        KeySource::KeyId => (0x31..=0x34).contains(&id).then(|| (id - 0x31) as u8),
    }
}

/// The laptop's keyboard layout, for typing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    Us,
    Uk,
    German,
    French,
    Spanish,
    Swedish,
}

impl Layout {
    pub const ALL: [Layout; 6] = [Layout::Us, Layout::Uk, Layout::German, Layout::French, Layout::Spanish, Layout::Swedish];

    /// The layout's name in forms and JSON.
    pub fn name(self) -> &'static str {
        match self {
            Layout::Us => "us",
            Layout::Uk => "gb",
            Layout::German => "de",
            Layout::French => "fr",
            Layout::Spanish => "es",
            Layout::Swedish => "se",
        }
    }

    pub fn from_name(name: &str) -> Option<Layout> {
        Layout::ALL.into_iter().find(|l| l.name() == name)
    }
}

/// The key (HID usage) and modifier bits (keyboard report byte 0) that type
/// `c` on `layout`.
pub fn key_for(layout: Layout, c: char) -> Option<(u8, u8)> {
    match c {
        ' ' => Some((0x2C, 0)),
        '\n' => Some((0x28, 0)),
        '\t' => Some((0x2B, 0)),
        _ => {
            let table = layouts::table(layout);
            let i = table.binary_search_by_key(&c, |&(ch, _, _)| ch).ok()?;
            Some((table[i].1, table[i].2))
        }
    }
}

/// What typing one character takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Keystroke {
    /// Send this report, then [`RELEASE`].
    Press(Report),
    /// Nothing to type (the CR of a CRLF: the LF types the newline).
    Ignore,
    /// No key types this character on the layout.
    Unsupported,
}

/// All keys up, sent after every press: a repeated letter would otherwise
/// read as one held key.
pub const RELEASE: Report = Report::Keyboard([0; KEYBOARD_REPORT_LEN]);

pub fn keystroke(layout: Layout, c: char) -> Keystroke {
    if c == '\r' {
        return Keystroke::Ignore;
    }
    match key_for(layout, c) {
        Some((usage, mods)) => {
            let mut report = [0u8; KEYBOARD_REPORT_LEN];
            report[0] = mods;
            report[2] = usage;
            Keystroke::Press(Report::Keyboard(report))
        }
        None => Keystroke::Unsupported,
    }
}

/// The next character of UTF-8 `text` at byte offset `pos`, and the offset
/// after it. For reading a buffer one character at a time.
pub fn char_at(text: &[u8], pos: usize) -> Option<(char, usize)> {
    let rest = text.get(pos..)?;
    let rest = &rest[..rest.len().min(4)];
    let valid = match core::str::from_utf8(rest) {
        Ok(s) => s,
        Err(e) => core::str::from_utf8(&rest[..e.valid_up_to()]).ok()?,
    };
    let c = valid.chars().next()?;
    Some((c, pos + c.len_utf8()))
}

/// Turns text into press/release keyboard reports.
pub struct Typist<'a> {
    layout: Layout,
    chars: core::str::Chars<'a>,
    /// The release still to send for the last press.
    release_pending: bool,
    pub typed: usize,
    pub skipped: usize,
}

impl<'a> Typist<'a> {
    pub fn new(layout: Layout, text: &'a str) -> Self {
        Self { layout, chars: text.chars(), release_pending: false, typed: 0, skipped: 0 }
    }
}

impl Iterator for Typist<'_> {
    type Item = Report;

    fn next(&mut self) -> Option<Report> {
        if self.release_pending {
            self.release_pending = false;
            return Some(RELEASE);
        }
        loop {
            match keystroke(self.layout, self.chars.next()?) {
                Keystroke::Press(report) => {
                    self.typed += 1;
                    self.release_pending = true;
                    return Some(report);
                }
                Keystroke::Ignore => {}
                Keystroke::Unsupported => self.skipped += 1,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layouts::{ALTGR, SHIFT};
    use alloc::vec::Vec;

    fn keys(layout: Layout, text: &str) -> Vec<[u8; 8]> {
        Typist::new(layout, text)
            .map(|r| match r {
                Report::Keyboard(k) => k,
                _ => unreachable!(),
            })
            .collect()
    }

    /// The hand-written US table the bridge used before the generated ones:
    /// the generator must reproduce it exactly.
    fn us_reference(c: char) -> Option<(u8, u8)> {
        const UNSHIFTED: &[u8] = b"1234567890-=[]\\;'`,./";
        const SHIFTED: &[u8] = b"!@#$%^&*()_+{}|:\"~<>?";
        const USAGES: [u8; 21] = [
            0x1e, 0x1f, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, // 1-0
            0x2d, 0x2e, 0x2f, 0x30, 0x31, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38, // - = [ ] \ ; ' ` , . /
        ];
        let b = u8::try_from(u32::from(c)).ok()?;
        Some(match b {
            b'a'..=b'z' => (0x04 + (b - b'a'), 0),
            b'A'..=b'Z' => (0x04 + (b - b'A'), SHIFT),
            b' ' => (0x2c, 0),
            _ => {
                if let Some(i) = UNSHIFTED.iter().position(|&u| u == b) {
                    (USAGES[i], 0)
                } else {
                    (USAGES[SHIFTED.iter().position(|&s| s == b)?], SHIFT)
                }
            }
        })
    }

    #[test]
    fn generated_us_matches_the_reference() {
        for b in 0x20u8..0x7f {
            let c = b as char;
            assert_eq!(key_for(Layout::Us, c), us_reference(c), "{:?}", c);
        }
        assert_eq!(key_for(Layout::Us, 'é'), None);
    }

    #[test]
    fn tables_are_sorted_and_unique() {
        for layout in Layout::ALL {
            let t = layouts::table(layout);
            assert!(t.windows(2).all(|w| w[0].0 < w[1].0), "{:?}", layout);
            // Every printable ASCII character but a few dead-key-only ones.
            let ascii = t.iter().filter(|e| e.0.is_ascii()).count();
            assert!(ascii >= 90, "{:?} has only {} ASCII characters", layout, ascii);
        }
    }

    #[test]
    fn layout_specifics() {
        // QWERTZ: Y and Z swapped, @ on AltGr+Q, ß where US has -.
        assert_eq!(key_for(Layout::German, 'z'), Some((0x1C, 0)));
        assert_eq!(key_for(Layout::German, 'Y'), Some((0x1D, SHIFT)));
        assert_eq!(key_for(Layout::German, '@'), Some((0x14, ALTGR)));
        assert_eq!(key_for(Layout::German, 'ß'), Some((0x2D, 0)));
        assert_eq!(key_for(Layout::German, '€'), Some((0x08, ALTGR)));
        // AZERTY: A/Q and Z/W swapped, M on the US ; key, digits shifted.
        assert_eq!(key_for(Layout::French, 'a'), Some((0x14, 0)));
        assert_eq!(key_for(Layout::French, 'w'), Some((0x1D, 0)));
        assert_eq!(key_for(Layout::French, 'm'), Some((0x33, 0)));
        assert_eq!(key_for(Layout::French, '1'), Some((0x1E, SHIFT)));
        assert_eq!(key_for(Layout::French, 'é'), Some((0x1F, 0)));
        // UK: @ and " swap places with US; £ on Shift+3.
        assert_eq!(key_for(Layout::Uk, '@'), Some((0x34, SHIFT)));
        assert_eq!(key_for(Layout::Uk, '"'), Some((0x1F, SHIFT)));
        assert_eq!(key_for(Layout::Uk, '£'), Some((0x20, SHIFT)));
        assert_eq!(key_for(Layout::Spanish, 'ñ'), Some((0x33, 0)));
        assert_eq!(key_for(Layout::Swedish, 'å'), Some((0x2F, 0)));
        assert_eq!(key_for(Layout::Swedish, '@'), Some((0x1F, ALTGR)));
    }

    #[test]
    fn layout_names_round_trip() {
        for layout in Layout::ALL {
            assert_eq!(Layout::from_name(layout.name()), Some(layout));
        }
        assert_eq!(Layout::from_name("dvorak"), None);
    }

    #[test]
    fn presses_and_releases_each_character() {
        let k = keys(Layout::Us, "aA\r\n");
        assert_eq!(k.len(), 6);
        assert_eq!(k[0], [0, 0, 0x04, 0, 0, 0, 0, 0]);
        assert_eq!(k[1], [0; 8]);
        assert_eq!(k[2], [SHIFT, 0, 0x04, 0, 0, 0, 0, 0]);
        assert_eq!(k[4], [0, 0, 0x28, 0, 0, 0, 0, 0]);
        assert_eq!(k[5], [0; 8]);
        // AltGr travels in the modifier byte.
        assert_eq!(keys(Layout::German, "@")[0], [ALTGR, 0, 0x14, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn counts_typed_and_skipped() {
        let mut t = Typist::new(Layout::Us, "héllo ✓");
        while t.next().is_some() {}
        assert_eq!((t.typed, t.skipped), (5, 2));
        let mut t = Typist::new(Layout::French, "héllo ✓");
        while t.next().is_some() {}
        assert_eq!((t.typed, t.skipped), (6, 1));
    }

    #[test]
    fn reads_characters_by_offset() {
        let text = "aé✓".as_bytes();
        assert_eq!(char_at(text, 0), Some(('a', 1)));
        assert_eq!(char_at(text, 1), Some(('é', 3)));
        assert_eq!(char_at(text, 3), Some(('✓', 6)));
        assert_eq!(char_at(text, 6), None);
        // Mid-character or past the end: nothing.
        assert_eq!(char_at(text, 2), None);
        assert_eq!(char_at(text, 99), None);
    }

    #[test]
    fn laptop_hotkeys() {
        let ctrl_alt = MASK_CONTROL | MASK_ALT;
        let mac = KeySource::MacPhysical;
        assert_eq!(laptop_hotkey(mac, 0, ctrl_alt, 0x12 + 1), Some(0));
        assert_eq!(laptop_hotkey(mac, 0, ctrl_alt, 0x15 + 1), Some(3));
        assert_eq!(laptop_hotkey(mac, 0, ctrl_alt, 0x17 + 1), None);
        assert_eq!(laptop_hotkey(KeySource::WindowsPhysical, 0, ctrl_alt, 0x02), Some(0));
        assert_eq!(laptop_hotkey(KeySource::WindowsPhysical, 0, ctrl_alt, 0x05), Some(3));
        assert_eq!(laptop_hotkey(KeySource::WindowsPhysical, 0, ctrl_alt, 0x06), None);
        assert_eq!(laptop_hotkey(KeySource::LinuxPhysical, 0, ctrl_alt, 2 + 8), Some(0));
        assert_eq!(laptop_hotkey(KeySource::LinuxPhysical, 0, ctrl_alt, 5 + 8), Some(3));
        assert_eq!(laptop_hotkey(KeySource::LinuxPhysical, 0, ctrl_alt | 0x0001, 3 + 8), Some(1));
        assert_eq!(laptop_hotkey(KeySource::KeyId, u16::from(b'2'), ctrl_alt, 0), Some(1));
        assert_eq!(laptop_hotkey(KeySource::KeyId, u16::from(b'5'), ctrl_alt, 0), None);
        assert_eq!(laptop_hotkey(KeySource::LinuxPhysical, 0, MASK_CONTROL, 2 + 8), None);
        assert_eq!(laptop_hotkey(KeySource::LinuxPhysical, 0, ctrl_alt | MASK_SUPER, 2 + 8), None);
    }

    #[test]
    fn hotkey() {
        let mac = KeySource::MacPhysical;
        let ctrl_alt = MASK_CONTROL | MASK_ALT;
        assert!(is_paste_hotkey(mac, 0x221a, ctrl_alt, MAC_V_BUTTON));
        assert!(is_paste_hotkey(mac, 0x221a, ctrl_alt | 0x0001, MAC_V_BUTTON));
        assert!(!is_paste_hotkey(mac, u16::from(b'v'), MASK_CONTROL, MAC_V_BUTTON));
        assert!(!is_paste_hotkey(mac, u16::from(b'v'), ctrl_alt | MASK_SUPER, MAC_V_BUTTON));
        assert!(!is_paste_hotkey(mac, u16::from(b'v'), ctrl_alt, 0x08 + 1));
        // With Ctrl+Alt held the character can be anything; go by the key.
        assert!(is_paste_hotkey(KeySource::WindowsPhysical, 0x16, ctrl_alt, WINDOWS_V_BUTTON));
        assert!(is_paste_hotkey(KeySource::LinuxPhysical, 0, ctrl_alt, LINUX_V_BUTTON));
        assert!(!is_paste_hotkey(KeySource::LinuxPhysical, u16::from(b'v'), ctrl_alt, 46 + 8));
        let ids = KeySource::KeyId;
        assert!(is_paste_hotkey(ids, u16::from(b'v'), ctrl_alt, 0));
        assert!(is_paste_hotkey(ids, u16::from(b'V'), ctrl_alt, 0));
        assert!(!is_paste_hotkey(ids, u16::from(b'c'), ctrl_alt, 0));
    }
}
