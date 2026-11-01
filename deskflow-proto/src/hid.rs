//! Turning Deskflow input messages into HID reports.
//!
//! The device exposes one HID interface with four input reports:
//!
//! * Report 1 — boot-style keyboard: modifier bitmask, reserved byte, six key
//!   slots.
//! * Report 2 — *absolute* pointer: buttons, X/Y in `0..=ABS_MAX`, vertical
//!   wheel, horizontal pan. Deskflow sends absolute coordinates in the
//!   client's screen space, so an absolute pointer reproduces them exactly
//!   with no pointer-acceleration drift. Linux treats this layout like the
//!   QEMU/VMware "tablet" mouse.
//! * Report 3 — *relative* pointer: the same, with signed 16-bit X/Y deltas,
//!   for laptops where an absolute pointer misbehaves (several monitors).
//! * Report 4 — consumer control (media keys): one 16-bit usage, 0 = none.
//!
//! Linux makes one input device per top-level application, merging reports
//! that share one. A device with both absolute and relative X/Y gets no
//! working pointer, so the two pointers need different applications: report
//! 2 is a Pointer, report 3 a Mouse.
//!
//! Hosts cache the report map of a paired device, so any change here means
//! re-pairing the laptop.

use crate::keymap::{self, KeySource};
use crate::wire::Message;

pub const KEYBOARD_REPORT_ID: u8 = 1;
pub const MOUSE_REPORT_ID: u8 = 2;
pub const REL_MOUSE_REPORT_ID: u8 = 3;
pub const CONSUMER_REPORT_ID: u8 = 4;
pub const KEYBOARD_REPORT_LEN: usize = 8;
pub const MOUSE_REPORT_LEN: usize = 7;
pub const REL_MOUSE_REPORT_LEN: usize = 7;
pub const CONSUMER_REPORT_LEN: usize = 2;

/// Upper bound of the absolute X/Y axes in [`REPORT_DESCRIPTOR`].
pub const ABS_MAX: u16 = 0x7FFF;

/// Which pointer report the mapper sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseMode {
    /// Report 2: the laptop's cursor goes exactly where Deskflow puts it.
    Absolute,
    /// Report 3: movement as deltas, like a real mouse. For laptops where
    /// an absolute pointer misbehaves (several monitors); the laptop's
    /// pointer acceleration applies, so the cursor no longer lines up with
    /// Deskflow's idea of it.
    Relative,
}

/// Deskflow scroll units per wheel notch (same as Windows `WHEEL_DELTA`).
const WHEEL_STEP: i32 = 120;

#[rustfmt::skip]
pub const REPORT_DESCRIPTOR: &[u8] = &[
    // Keyboard
    0x05, 0x01,       // Usage Page (Generic Desktop)
    0x09, 0x06,       // Usage (Keyboard)
    0xA1, 0x01,       // Collection (Application)
    0x85, KEYBOARD_REPORT_ID,
    0x05, 0x07,       //   Usage Page (Keyboard/Keypad)
    0x19, 0xE0,       //   Usage Minimum (Left Control)
    0x29, 0xE7,       //   Usage Maximum (Right GUI)
    0x15, 0x00,       //   Logical Minimum (0)
    0x25, 0x01,       //   Logical Maximum (1)
    0x75, 0x01,       //   Report Size (1)
    0x95, 0x08,       //   Report Count (8)
    0x81, 0x02,       //   Input (Data, Var, Abs) — modifiers
    0x95, 0x01,       //   Report Count (1)
    0x75, 0x08,       //   Report Size (8)
    0x81, 0x01,       //   Input (Const) — reserved
    0x95, 0x05,       //   Report Count (5)
    0x75, 0x01,       //   Report Size (1)
    0x05, 0x08,       //   Usage Page (LEDs)
    0x19, 0x01,       //   Usage Minimum (Num Lock)
    0x29, 0x05,       //   Usage Maximum (Kana)
    0x91, 0x02,       //   Output (Data, Var, Abs) — LEDs
    0x95, 0x01,       //   Report Count (1)
    0x75, 0x03,       //   Report Size (3)
    0x91, 0x01,       //   Output (Const) — padding
    0x95, 0x06,       //   Report Count (6)
    0x75, 0x08,       //   Report Size (8)
    0x15, 0x00,       //   Logical Minimum (0)
    0x26, 0xFF, 0x00, //   Logical Maximum (255)
    0x05, 0x07,       //   Usage Page (Keyboard/Keypad)
    0x19, 0x00,       //   Usage Minimum (0)
    0x2A, 0xFF, 0x00, //   Usage Maximum (255)
    0x81, 0x00,       //   Input (Data, Array) — key slots
    0xC0,             // End Collection

    // Absolute pointer
    0x05, 0x01,       // Usage Page (Generic Desktop)
    0x09, 0x01,       // Usage (Pointer): not Mouse, see the module docs
    0xA1, 0x01,       // Collection (Application)
    0x85, MOUSE_REPORT_ID,
    0x09, 0x01,       //   Usage (Pointer)
    0xA1, 0x00,       //   Collection (Physical)
    0x05, 0x09,       //     Usage Page (Button)
    0x19, 0x01,       //     Usage Minimum (1)
    0x29, 0x05,       //     Usage Maximum (5)
    0x15, 0x00,       //     Logical Minimum (0)
    0x25, 0x01,       //     Logical Maximum (1)
    0x95, 0x05,       //     Report Count (5)
    0x75, 0x01,       //     Report Size (1)
    0x81, 0x02,       //     Input (Data, Var, Abs) — buttons
    0x95, 0x01,       //     Report Count (1)
    0x75, 0x03,       //     Report Size (3)
    0x81, 0x01,       //     Input (Const) — padding
    0x05, 0x01,       //     Usage Page (Generic Desktop)
    0x09, 0x30,       //     Usage (X)
    0x09, 0x31,       //     Usage (Y)
    0x15, 0x00,       //     Logical Minimum (0)
    0x26, 0xFF, 0x7F, //     Logical Maximum (32767)
    0x75, 0x10,       //     Report Size (16)
    0x95, 0x02,       //     Report Count (2)
    0x81, 0x02,       //     Input (Data, Var, Abs) — X, Y
    0x09, 0x38,       //     Usage (Wheel)
    0x15, 0x81,       //     Logical Minimum (-127)
    0x25, 0x7F,       //     Logical Maximum (127)
    0x75, 0x08,       //     Report Size (8)
    0x95, 0x01,       //     Report Count (1)
    0x81, 0x06,       //     Input (Data, Var, Rel) — wheel
    0x05, 0x0C,       //     Usage Page (Consumer)
    0x0A, 0x38, 0x02, //     Usage (AC Pan)
    0x95, 0x01,       //     Report Count (1)
    0x81, 0x06,       //     Input (Data, Var, Rel) — horizontal scroll
    0xC0,             //   End Collection
    0xC0,             // End Collection

    // Relative pointer
    0x05, 0x01,       // Usage Page (Generic Desktop)
    0x09, 0x02,       // Usage (Mouse)
    0xA1, 0x01,       // Collection (Application)
    0x85, REL_MOUSE_REPORT_ID,
    0x09, 0x01,       //   Usage (Pointer)
    0xA1, 0x00,       //   Collection (Physical)
    0x05, 0x09,       //     Usage Page (Button)
    0x19, 0x01,       //     Usage Minimum (1)
    0x29, 0x05,       //     Usage Maximum (5)
    0x15, 0x00,       //     Logical Minimum (0)
    0x25, 0x01,       //     Logical Maximum (1)
    0x95, 0x05,       //     Report Count (5)
    0x75, 0x01,       //     Report Size (1)
    0x81, 0x02,       //     Input (Data, Var, Abs) — buttons
    0x95, 0x01,       //     Report Count (1)
    0x75, 0x03,       //     Report Size (3)
    0x81, 0x01,       //     Input (Const) — padding
    0x05, 0x01,       //     Usage Page (Generic Desktop)
    0x09, 0x30,       //     Usage (X)
    0x09, 0x31,       //     Usage (Y)
    0x16, 0x01, 0x80, //     Logical Minimum (-32767)
    0x26, 0xFF, 0x7F, //     Logical Maximum (32767)
    0x75, 0x10,       //     Report Size (16)
    0x95, 0x02,       //     Report Count (2)
    0x81, 0x06,       //     Input (Data, Var, Rel) — X, Y
    0x09, 0x38,       //     Usage (Wheel)
    0x15, 0x81,       //     Logical Minimum (-127)
    0x25, 0x7F,       //     Logical Maximum (127)
    0x75, 0x08,       //     Report Size (8)
    0x95, 0x01,       //     Report Count (1)
    0x81, 0x06,       //     Input (Data, Var, Rel) — wheel
    0x05, 0x0C,       //     Usage Page (Consumer)
    0x0A, 0x38, 0x02, //     Usage (AC Pan)
    0x95, 0x01,       //     Report Count (1)
    0x81, 0x06,       //     Input (Data, Var, Rel) — horizontal scroll
    0xC0,             //   End Collection
    0xC0,             // End Collection

    // Consumer control (media keys)
    0x05, 0x0C,       // Usage Page (Consumer)
    0x09, 0x01,       // Usage (Consumer Control)
    0xA1, 0x01,       // Collection (Application)
    0x85, CONSUMER_REPORT_ID,
    0x15, 0x00,       //   Logical Minimum (0)
    0x26, 0xFF, 0x03, //   Logical Maximum (0x3FF)
    0x19, 0x00,       //   Usage Minimum (0)
    0x2A, 0xFF, 0x03, //   Usage Maximum (0x3FF)
    0x75, 0x10,       //   Report Size (16)
    0x95, 0x01,       //   Report Count (1)
    0x81, 0x00,       //   Input (Data, Array) — one usage at a time
    0xC0,             // End Collection
];

/// A HID input report, without its report-ID byte (BLE HID-over-GATT carries
/// the ID in the characteristic's Report Reference descriptor instead).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Report {
    Keyboard([u8; KEYBOARD_REPORT_LEN]),
    Mouse([u8; MOUSE_REPORT_LEN]),
    RelMouse([u8; REL_MOUSE_REPORT_LEN]),
    Consumer([u8; CONSUMER_REPORT_LEN]),
}

impl Report {
    /// The mouse buttons, for pointer reports.
    pub fn buttons(self) -> Option<u8> {
        match self {
            Report::Mouse(r) | Report::RelMouse(r) => Some(r[0]),
            _ => None,
        }
    }

    /// Whether this is only movement, given the buttons last sent: no click
    /// and no wheel, so it can wait for the radio's next slot.
    pub fn is_plain_move(self, buttons: u8) -> bool {
        match self {
            Report::Mouse(r) | Report::RelMouse(r) => r[0] == buttons && r[5..] == [0, 0],
            _ => false,
        }
    }

    /// This pointer report and a newer one as a single report, if nothing
    /// is lost by that: same buttons, and no wheel movement (which adds up
    /// differently). The radio is slower than Deskflow's moves, so moves
    /// waiting for it are merged rather than queued.
    pub fn merged(self, newer: Report) -> Option<Report> {
        match (self, newer) {
            (Report::Mouse(a), Report::Mouse(b)) if a[0] == b[0] && a[5..] == [0, 0] && b[5..] == [0, 0] => Some(newer),
            (Report::RelMouse(a), Report::RelMouse(b)) if a[0] == b[0] && a[5..] == [0, 0] && b[5..] == [0, 0] => {
                let sum = |lo: usize| {
                    i16::from_le_bytes([a[lo], a[lo + 1]]).saturating_add(i16::from_le_bytes([b[lo], b[lo + 1]])).to_le_bytes()
                };
                let (x, y) = (sum(1), sum(3));
                Some(Report::RelMouse([a[0], x[0], x[1], y[0], y[1], 0, 0]))
            }
            _ => None,
        }
    }
}

/// Keys held at once. Six go in the report; the rest are remembered so that
/// releasing one of the first six doesn't lose a key still held.
const MAX_HELD: usize = 16;

#[derive(Clone, Copy)]
struct HeldKey {
    /// Deskflow's per-key `button` id; `KeyUp` is matched by this, because
    /// the `KeyID` on release can differ from the press (Shift released first).
    button: u16,
    id: u16,
    usage: u8,
}

pub struct HidMapper {
    source: KeySource,
    mouse_mode: MouseMode,
    screen_w: u16,
    screen_h: u16,
    x: i32,
    y: i32,
    /// Relative mode: the position the reports so far add up to.
    sent: (i32, i32),
    buttons: u8,
    wheel_acc: (i32, i32),
    /// HID AC Pan is positive to the right. The sign of Deskflow's X wheel
    /// delta is not yet verified on hardware; flip this if horizontal
    /// scrolling comes out backwards.
    invert_pan: bool,
    held: [Option<HeldKey>; MAX_HELD],
    /// The media key held down: the consumer report carries one at a time.
    media: Option<HeldKey>,
}

impl HidMapper {
    pub const fn new(source: KeySource, screen_w: u16, screen_h: u16) -> Self {
        HidMapper {
            source,
            mouse_mode: MouseMode::Absolute,
            screen_w,
            screen_h,
            x: screen_w as i32 / 2,
            y: screen_h as i32 / 2,
            sent: (screen_w as i32 / 2, screen_h as i32 / 2),
            buttons: 0,
            wheel_acc: (0, 0),
            invert_pan: true,
            held: [None; MAX_HELD],
            media: None,
        }
    }

    pub const fn with_mouse_mode(mut self, mode: MouseMode) -> Self {
        self.mouse_mode = mode;
        self
    }

    /// Whether any key (modifiers included) is held down.
    pub fn keys_held(&self) -> bool {
        self.held.iter().any(Option::is_some)
    }

    pub fn screen_size(&self) -> (u16, u16) {
        (self.screen_w, self.screen_h)
    }

    pub fn cursor(&self) -> (i16, i16) {
        (self.x as i16, self.y as i16)
    }

    /// Feeds one server message; `emit` is called for each report to send, in
    /// order. Messages that don't affect input are ignored.
    pub fn handle(&mut self, msg: &Message, mut emit: impl FnMut(Report)) {
        match *msg {
            Message::Enter { x, y, .. } => {
                // A fresh entry starts from a clean slate on the laptop side.
                self.release_all(&mut emit);
                self.move_to(x as i32, y as i32);
                // A relative pointer can't be placed; carry on from wherever
                // the laptop's cursor is.
                self.sent = (self.x, self.y);
                emit(self.mouse_report(0, 0));
            }
            Message::Leave | Message::Close => self.release_all(&mut emit),
            Message::KeyDown { id, button, .. } | Message::KeyRepeat { id, button, .. }
                if let Some(usage) = keymap::consumer_usage_for(id) =>
            {
                // Media keys come by KeyID in every mode: their `button` isn't
                // a normal key position on every server.
                if self.media.is_none_or(|m| m.id != id) {
                    self.media = Some(HeldKey { button, id, usage: 0 });
                    emit(Report::Consumer(usage.to_le_bytes()));
                }
            }
            Message::KeyUp { id, button, .. }
                if self.media.is_some_and(|m| m.id == id || (button != 0 && m.button == button)) =>
            {
                self.media = None;
                emit(Report::Consumer([0; CONSUMER_REPORT_LEN]));
            }
            Message::KeyDown { id, button, .. } | Message::KeyRepeat { id, button, .. } => {
                // Auto-repeat happens on the laptop side because we hold the
                // key down; a repeat for a key we already hold is a no-op.
                if self.held.iter().flatten().any(|k| k.button == button && k.id == id) {
                    return;
                }
                if let Some(usage) = keymap::usage_for(self.source, id, button)
                    && let Some(slot) = self.held.iter_mut().find(|k| k.is_none())
                {
                    *slot = Some(HeldKey { button, id, usage });
                    emit(self.keyboard_report());
                }
            }
            Message::KeyUp { id, button, .. } => {
                // Prefer matching by physical button; fall back to KeyID when
                // the server sends button 0.
                let pos = self
                    .held
                    .iter()
                    .position(|k| matches!(k, Some(k) if button != 0 && k.button == button))
                    .or_else(|| self.held.iter().position(|k| matches!(k, Some(k) if k.id == id)));
                if let Some(pos) = pos {
                    self.held[pos] = None;
                    emit(self.keyboard_report());
                }
            }
            Message::MouseDown { button } => {
                self.buttons |= button_bit(button);
                emit(self.mouse_report(0, 0));
            }
            Message::MouseUp { button } => {
                self.buttons &= !button_bit(button);
                emit(self.mouse_report(0, 0));
            }
            Message::MouseMove { x, y } => {
                self.move_to(x as i32, y as i32);
                emit(self.mouse_report(0, 0));
            }
            Message::MouseRelMove { dx, dy } => {
                self.move_to(self.x + dx as i32, self.y + dy as i32);
                emit(self.mouse_report(0, 0));
            }
            Message::MouseWheel { dx, dy } => {
                self.wheel_acc.0 += dx as i32;
                self.wheel_acc.1 += dy as i32;
                let pan = take_steps(&mut self.wheel_acc.0);
                let wheel = take_steps(&mut self.wheel_acc.1);
                if pan != 0 || wheel != 0 {
                    let pan = if self.invert_pan { -pan } else { pan };
                    emit(self.mouse_report(wheel, pan));
                }
            }
            _ => {}
        }
    }

    /// Releases every key and button, e.g. when the cursor leaves this screen
    /// or the connection drops, so nothing stays stuck on the laptop.
    pub fn release_all(&mut self, mut emit: impl FnMut(Report)) {
        if self.held.iter().any(Option::is_some) {
            self.held = [None; MAX_HELD];
            emit(self.keyboard_report());
        }
        if self.media.take().is_some() {
            emit(Report::Consumer([0; CONSUMER_REPORT_LEN]));
        }
        if self.buttons != 0 {
            self.buttons = 0;
            emit(self.mouse_report(0, 0));
        }
        self.wheel_acc = (0, 0);
    }

    fn move_to(&mut self, x: i32, y: i32) {
        self.x = x.clamp(0, self.screen_w as i32 - 1);
        self.y = y.clamp(0, self.screen_h as i32 - 1);
    }

    fn keyboard_report(&self) -> Report {
        let mut r = [0u8; KEYBOARD_REPORT_LEN];
        let mut slot = 2;
        for k in self.held.iter().flatten() {
            match keymap::modifier_bit(k.usage) {
                Some(bit) => r[0] |= bit,
                None if slot < KEYBOARD_REPORT_LEN => {
                    r[slot] = k.usage;
                    slot += 1;
                }
                None => {}
            }
        }
        Report::Keyboard(r)
    }

    fn mouse_report(&mut self, wheel: i8, pan: i8) -> Report {
        match self.mouse_mode {
            MouseMode::Absolute => {
                let x = scale(self.x, self.screen_w).to_le_bytes();
                let y = scale(self.y, self.screen_h).to_le_bytes();
                Report::Mouse([self.buttons, x[0], x[1], y[0], y[1], wheel as u8, pan as u8])
            }
            MouseMode::Relative => {
                // Positions are clamped to the screen (at most 16384), so the
                // delta always fits.
                let dx = ((self.x - self.sent.0) as i16).to_le_bytes();
                let dy = ((self.y - self.sent.1) as i16).to_le_bytes();
                self.sent = (self.x, self.y);
                Report::RelMouse([self.buttons, dx[0], dx[1], dy[0], dy[1], wheel as u8, pan as u8])
            }
        }
    }
}

/// Deskflow button ids: 1 left, 2 middle, 3 right, 4/5 back/forward.
/// HID button usages 1..5: left, right, middle, back, forward.
fn button_bit(button: u8) -> u8 {
    match button {
        1 => 0x01,
        2 => 0x04,
        3 => 0x02,
        4 => 0x08,
        5 => 0x10,
        _ => 0,
    }
}

/// Maps a pixel coordinate to the absolute axis range.
fn scale(v: i32, extent: u16) -> u16 {
    let max = (extent as i32 - 1).max(1);
    ((v * ABS_MAX as i32 + max / 2) / max) as u16
}

/// Removes whole wheel notches from the accumulator, keeping the remainder so
/// high-resolution trackpad scrolling isn't lost.
fn take_steps(acc: &mut i32) -> i8 {
    let steps = (*acc / WHEEL_STEP).clamp(-127, 127);
    *acc -= steps * WHEEL_STEP;
    steps as i8
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sums the input bits of each report ID by walking the descriptor's
    /// items, so the report lengths can't drift from the descriptor.
    fn input_bits_by_report() -> std::collections::BTreeMap<u8, u32> {
        let mut bits = std::collections::BTreeMap::new();
        let (mut id, mut size, mut count) = (0u8, 0u32, 0u32);
        let d = REPORT_DESCRIPTOR;
        let mut i = 0;
        while i < d.len() {
            let prefix = d[i];
            let len = match prefix & 0x03 {
                3 => 4,
                n => n as usize,
            };
            let mut value = 0u32;
            for (k, &b) in d[i + 1..i + 1 + len].iter().enumerate() {
                value |= (b as u32) << (8 * k);
            }
            match prefix & 0xFC {
                0x84 => id = value as u8,
                0x74 => size = value,
                0x94 => count = value,
                0x80 => *bits.entry(id).or_insert(0) += size * count,
                _ => {}
            }
            i += 1 + len;
        }
        bits
    }

    #[test]
    fn media_keys_use_the_consumer_report() {
        let mut m = HidMapper::new(KeySource::MacPhysical, 1920, 1080);
        let mut r = Vec::new();
        const VOLUME_UP: u16 = 0xE0AF;
        // On a Mac the button may look like an ordinary key; the KeyID wins.
        m.handle(&Message::KeyDown { id: VOLUME_UP, mask: 0, button: 1 }, |x| r.push(x));
        m.handle(&Message::KeyRepeat { id: VOLUME_UP, mask: 0, count: 1, button: 1 }, |x| r.push(x));
        assert_eq!(r, vec![Report::Consumer([0xE9, 0x00])]);
        assert!(!m.keys_held());
        r.clear();
        m.handle(&Message::KeyUp { id: VOLUME_UP, mask: 0, button: 1 }, |x| r.push(x));
        assert_eq!(r, vec![Report::Consumer([0, 0])]);

        // Held across a screen switch: released with everything else.
        r.clear();
        m.handle(&Message::KeyDown { id: 0xE0B3, mask: 0, button: 0 }, |x| r.push(x));
        m.handle(&Message::Leave, |x| r.push(x));
        assert_eq!(r, vec![Report::Consumer([0xCD, 0x00]), Report::Consumer([0, 0])]);
    }

    #[test]
    fn windows_and_linux_servers_map_physically() {
        let mut r = Vec::new();
        let mut win = HidMapper::new(KeySource::WindowsPhysical, 1920, 1080);
        win.handle(&Message::KeyDown { id: 0x61, mask: 0, button: 0x1E }, |x| r.push(x));
        let mut linux = HidMapper::new(KeySource::LinuxPhysical, 1920, 1080);
        linux.handle(&Message::KeyDown { id: 0x61, mask: 0, button: 30 + 8 }, |x| r.push(x));
        assert_eq!(r, vec![Report::Keyboard([0, 0, 0x04, 0, 0, 0, 0, 0]); 2]);
    }

    #[test]
    fn descriptor_matches_report_lengths() {
        let bits = input_bits_by_report();
        assert_eq!(bits.len(), 4);
        assert_eq!(bits[&KEYBOARD_REPORT_ID], KEYBOARD_REPORT_LEN as u32 * 8);
        assert_eq!(bits[&MOUSE_REPORT_ID], MOUSE_REPORT_LEN as u32 * 8);
        assert_eq!(bits[&REL_MOUSE_REPORT_ID], REL_MOUSE_REPORT_LEN as u32 * 8);
        assert_eq!(bits[&CONSUMER_REPORT_ID], CONSUMER_REPORT_LEN as u32 * 8);
    }

    /// Linux merges reports that share an application into one input device.
    #[test]
    fn every_report_has_its_own_application() {
        let d = REPORT_DESCRIPTOR;
        let (mut page, mut usage) = (0u32, 0u32);
        let mut applications = Vec::new();
        let mut i = 0;
        while i < d.len() {
            let prefix = d[i];
            let len = match prefix & 0x03 {
                3 => 4,
                n => n as usize,
            };
            let mut value = 0u32;
            for (k, &b) in d[i + 1..i + 1 + len].iter().enumerate() {
                value |= (b as u32) << (8 * k);
            }
            match prefix & 0xFC {
                0x04 => page = value,
                0x08 => usage = value,
                0xA0 if value == 1 => applications.push((page, usage)),
                _ => {}
            }
            i += 1 + len;
        }
        assert_eq!(applications, vec![(1, 0x06), (1, 0x01), (1, 0x02), (0x0C, 0x01)]);
    }

    #[test]
    fn descriptor_collections_balance() {
        let opens = REPORT_DESCRIPTOR.windows(2).filter(|w| w[0] == 0xA1).count();
        let closes = REPORT_DESCRIPTOR.iter().filter(|&&b| b == 0xC0).count();
        assert_eq!(opens, 6);
        assert_eq!(closes, 6);
    }

    fn collect(m: &mut HidMapper, msg: Message) -> Vec<Report> {
        let mut out = Vec::new();
        m.handle(&msg, |r| out.push(r));
        out
    }

    fn mapper() -> HidMapper {
        HidMapper::new(KeySource::MacPhysical, 1920, 1080)
    }

    #[test]
    fn shift_a_press_and_release_out_of_order() {
        let mut m = mapper();
        // Shift (kVK_Shift 0x38), then 'A' (kVK_ANSI_A 0x00)
        collect(&mut m, Message::KeyDown { id: 0xEFE1, mask: 0, button: 0x39 });
        let r = collect(&mut m, Message::KeyDown { id: b'A' as u16, mask: 1, button: 1 });
        assert_eq!(r, vec![Report::Keyboard([0x02, 0, 0x04, 0, 0, 0, 0, 0])]);
        // Shift released first; 'a' is released with a different KeyID.
        collect(&mut m, Message::KeyUp { id: 0xEFE1, mask: 1, button: 0x39 });
        let r = collect(&mut m, Message::KeyUp { id: b'a' as u16, mask: 0, button: 1 });
        assert_eq!(r, vec![Report::Keyboard([0; 8])]);
    }

    #[test]
    fn repeat_of_held_key_is_ignored() {
        let mut m = mapper();
        collect(&mut m, Message::KeyDown { id: b'a' as u16, mask: 0, button: 1 });
        assert!(collect(&mut m, Message::KeyRepeat { id: b'a' as u16, mask: 0, count: 1, button: 1 }).is_empty());
    }

    #[test]
    fn absolute_move_scales_to_axis_range() {
        let mut m = mapper();
        let r = collect(&mut m, Message::MouseMove { x: 1919, y: 0 });
        assert_eq!(r, vec![Report::Mouse([0, 0xFF, 0x7F, 0, 0, 0, 0])]);
    }

    #[test]
    fn moves_are_clamped_to_screen() {
        let mut m = mapper();
        collect(&mut m, Message::MouseMove { x: 5000, y: -10 });
        assert_eq!(m.cursor(), (1919, 0));
        collect(&mut m, Message::MouseRelMove { dx: -3000, dy: 50 });
        assert_eq!(m.cursor(), (0, 50));
    }

    #[test]
    fn right_and_middle_buttons_swap_bits() {
        let mut m = mapper();
        let r = collect(&mut m, Message::MouseDown { button: 3 });
        assert!(matches!(r[0], Report::Mouse([0x02, ..])));
        let r = collect(&mut m, Message::MouseDown { button: 2 });
        assert!(matches!(r[0], Report::Mouse([0x06, ..])));
    }

    #[test]
    fn wheel_accumulates_partial_steps() {
        let mut m = mapper();
        assert!(collect(&mut m, Message::MouseWheel { dx: 0, dy: 60 }).is_empty());
        let r = collect(&mut m, Message::MouseWheel { dx: 0, dy: 60 });
        assert!(matches!(r[0], Report::Mouse([_, _, _, _, _, 1, 0])));
        let r = collect(&mut m, Message::MouseWheel { dx: 0, dy: -240 });
        assert!(matches!(r[0], Report::Mouse([_, _, _, _, _, 0xFE, 0])));
    }

    #[test]
    fn leave_releases_everything() {
        let mut m = mapper();
        collect(&mut m, Message::KeyDown { id: b'a' as u16, mask: 0, button: 1 });
        collect(&mut m, Message::MouseDown { button: 1 });
        let r = collect(&mut m, Message::Leave);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0], Report::Keyboard([0; 8]));
        assert!(matches!(r[1], Report::Mouse([0, ..])));
    }

    #[test]
    fn relative_mode_sends_deltas() {
        let mut m = mapper().with_mouse_mode(MouseMode::Relative);
        // Entering can't place the cursor: no movement.
        let r = collect(&mut m, Message::Enter { x: 100, y: 100, seq: 0, mask: 0 });
        assert_eq!(r, vec![Report::RelMouse([0; 7])]);
        let r = collect(&mut m, Message::MouseMove { x: 110, y: 95 });
        assert_eq!(r, vec![Report::RelMouse([0, 10, 0, 0xFB, 0xFF, 0, 0])]);
        let r = collect(&mut m, Message::MouseRelMove { dx: -3, dy: 2 });
        assert_eq!(r, vec![Report::RelMouse([0, 0xFD, 0xFF, 2, 0, 0, 0])]);
        // Buttons and the wheel go on the same report, without movement.
        let r = collect(&mut m, Message::MouseDown { button: 1 });
        assert_eq!(r, vec![Report::RelMouse([1, 0, 0, 0, 0, 0, 0])]);
        let r = collect(&mut m, Message::MouseWheel { dx: 0, dy: -120 });
        assert_eq!(r, vec![Report::RelMouse([1, 0, 0, 0, 0, 0xFF, 0])]);
        // Clamped at the edge: no delta past it.
        collect(&mut m, Message::MouseMove { x: 5000, y: 95 });
        assert!(collect(&mut m, Message::MouseMove { x: 6000, y: 95 }).contains(&Report::RelMouse([1, 0, 0, 0, 0, 0, 0])));
        let r = collect(&mut m, Message::Leave);
        assert_eq!(r, vec![Report::RelMouse([0; 7])]);
    }

    #[test]
    fn only_plain_moves_can_wait() {
        assert!(Report::Mouse([1, 9, 0, 9, 0, 0, 0]).is_plain_move(1));
        assert!(Report::RelMouse([0, 9, 0, 9, 0, 0, 0]).is_plain_move(0));
        assert!(!Report::Mouse([1, 9, 0, 9, 0, 0, 0]).is_plain_move(0));
        assert!(!Report::RelMouse([0, 0, 0, 0, 0, 1, 0]).is_plain_move(0));
        assert!(!Report::RelMouse([0, 0, 0, 0, 0, 0, 1]).is_plain_move(0));
        assert!(!Report::Keyboard([0; 8]).is_plain_move(0));
        assert_eq!(Report::Mouse([3; 7]).buttons(), Some(3));
        assert_eq!(Report::Consumer([0; 2]).buttons(), None);
    }

    #[test]
    fn moves_merge_but_clicks_and_wheels_do_not() {
        let rel = |b, dx: i16, dy: i16, wheel| {
            let (x, y) = (dx.to_le_bytes(), dy.to_le_bytes());
            Report::RelMouse([b, x[0], x[1], y[0], y[1], wheel, 0])
        };
        assert_eq!(rel(0, 10, -5, 0).merged(rel(0, -3, -5, 0)), Some(rel(0, 7, -10, 0)));
        assert_eq!(rel(1, 10, 0, 0).merged(rel(1, 1, 0, 0)), Some(rel(1, 11, 0, 0)));
        assert_eq!(rel(0, 32000, 0, 0).merged(rel(0, 32000, 0, 0)), Some(rel(0, 32767, 0, 0)));
        assert_eq!(rel(0, 1, 0, 0).merged(rel(1, 0, 0, 0)), None);
        assert_eq!(rel(1, 0, 0, 0).merged(rel(0, 0, 0, 0)), None);
        assert_eq!(rel(0, 1, 0, 0).merged(rel(0, 0, 0, 1)), None);
        assert_eq!(rel(0, 0, 0, 0xFF).merged(rel(0, 1, 0, 0)), None);

        let abs = |b, x: u8, wheel| Report::Mouse([b, x, 0, 0, 0, wheel, 0]);
        assert_eq!(abs(0, 1, 0).merged(abs(0, 2, 0)), Some(abs(0, 2, 0)));
        assert_eq!(abs(0, 1, 0).merged(abs(1, 1, 0)), None);
        assert_eq!(abs(0, 1, 0).merged(abs(0, 1, 1)), None);
        assert_eq!(abs(0, 1, 0).merged(rel(0, 1, 0, 0)), None);
        assert_eq!(Report::Consumer([0, 0]).merged(Report::Consumer([0, 0])), None);
    }

    #[test]
    fn more_than_six_keys_keeps_overflow_held() {
        let mut m = mapper();
        for b in 1..=8u16 {
            collect(&mut m, Message::KeyDown { id: 0, mask: 0, button: b }); // kVK 0..7: A S D F H G Z X
        }
        let r = collect(&mut m, Message::KeyUp { id: 0, mask: 0, button: 1 });
        // 'A' released; the 7th key (Z, usage 0x1D) moves into the report.
        assert_eq!(r, vec![Report::Keyboard([0, 0, 0x16, 0x07, 0x09, 0x0B, 0x0A, 0x1D])]);
    }
}
