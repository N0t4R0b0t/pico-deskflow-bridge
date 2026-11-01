//! User-editable settings: what the web page shows and edits, and what is
//! stored in flash. The firmware's `config.rs` provides the defaults.
//!
//! Flash holds them in the same form the page submits
//! (`screen_name=…&screen_width=…`), applied over the defaults at boot. That
//! is self-describing: later firmware can add or drop settings, and a value
//! the current rules reject only resets that one field.

use core::fmt;

use heapless::{String, Vec};

use crate::hid::MouseMode;
use crate::keymap::KeySource;
use crate::panel::{Action, BUTTONS, ButtonActions};
use crate::typing::Layout;
use crate::tls::{DisplayFingerprint, ServerTrust, try_parse_fingerprint};
use crate::web::{FormStr, JsonStr, form_value};

pub const SCREEN_NAME_MAX: usize = 32;
/// What fits in the advertisement next to the flags and the HID service UUID.
pub const BLE_NAME_MAX: usize = 22;

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// Screen name announced to Deskflow; must match the server's layout.
    pub screen_name: String<SCREEN_NAME_MAX>,
    /// Resolution reported to Deskflow.
    pub screen_width: u16,
    pub screen_height: u16,
    /// Name the laptop sees when pairing.
    pub ble_name: String<BLE_NAME_MAX>,
    pub key_source: KeySource,
    pub mouse_mode: MouseMode,
    /// `None` when TLS is off on the server.
    pub tls: Option<ServerTrust>,
    /// The laptop's layout, for typing the clipboard.
    pub typing_layout: Layout,
    /// Clipboard typing speed, characters per second.
    pub typing_rate: u8,
    /// What the optional buttons (and the page's virtual ones) do.
    pub buttons: [ButtonActions; BUTTONS],
}

/// Form fields of the button actions: (short press, long press) per button.
const BUTTON_FIELDS: [(&str, &str); BUTTONS] =
    [("button1_short", "button1_long"), ("button2_short", "button2_long"), ("button3_short", "button3_long")];
/// Longest stored form.
pub const FORM_MAX: usize = 640;

/// A rejected form field, with a message for the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FieldError {
    pub field: &'static str,
    pub message: &'static str,
}

const fn err(field: &'static str, message: &'static str) -> FieldError {
    FieldError { field, message }
}

const SCREEN_MIN: u16 = 200;
const SCREEN_MAX: u16 = 16384;
pub const TYPING_RATE_MIN: u8 = 5;
pub const TYPING_RATE_MAX: u8 = 100;

impl Settings {
    /// Applies a submitted form (`application/x-www-form-urlencoded`) on top
    /// of `self`. Missing fields keep their value.
    pub fn with_form(&self, body: &[u8]) -> Result<Settings, FieldError> {
        let mut next = self.clone();
        let mut buf = [0u8; 128];

        if let Some(v) = form_value(body, "screen_name", &mut buf) {
            let v = v.ok_or(err("screen_name", "invalid encoding"))?;
            next.screen_name = screen_name(v)?;
        }
        if let Some(v) = form_value(body, "screen_width", &mut buf) {
            next.screen_width = dimension(v, "screen_width")?;
        }
        if let Some(v) = form_value(body, "screen_height", &mut buf) {
            next.screen_height = dimension(v, "screen_height")?;
        }
        if let Some(v) = form_value(body, "ble_name", &mut buf) {
            let v = v.ok_or(err("ble_name", "invalid encoding"))?;
            next.ble_name = ble_name(v)?;
        }
        if let Some(v) = form_value(body, "key_source", &mut buf) {
            next.key_source = match v {
                Some(name) => key_source_from_name(name).ok_or(err("key_source", "unknown key source"))?,
                None => return Err(err("key_source", "unknown key source")),
            };
        }
        if let Some(v) = form_value(body, "mouse_mode", &mut buf) {
            next.mouse_mode = match v {
                Some("absolute") => MouseMode::Absolute,
                Some("relative") => MouseMode::Relative,
                _ => return Err(err("mouse_mode", "unknown mouse mode")),
            };
        }
        if let Some(v) = form_value(body, "typing_layout", &mut buf) {
            next.typing_layout =
                v.and_then(Layout::from_name).ok_or(err("typing_layout", "unknown keyboard layout"))?;
        }
        if let Some(v) = form_value(body, "typing_rate", &mut buf) {
            let rate: Option<u8> = v.and_then(|v| v.trim().parse().ok());
            next.typing_rate = rate
                .filter(|r| (TYPING_RATE_MIN..=TYPING_RATE_MAX).contains(r))
                .ok_or(err("typing_rate", "between 5 and 100 characters a second"))?;
        }
        for (actions, (short, long)) in next.buttons.iter_mut().zip(BUTTON_FIELDS) {
            if let Some(v) = form_value(body, short, &mut buf) {
                actions.short = v.and_then(Action::from_name).ok_or(err(short, "unknown action"))?;
            }
            if let Some(v) = form_value(body, long, &mut buf) {
                actions.long = v.and_then(Action::from_name).ok_or(err(long, "unknown action"))?;
            }
        }
        let mut fp_buf = [0u8; 128];
        let fingerprint = form_value(body, "fingerprint", &mut fp_buf);
        if let Some(v) = form_value(body, "tls", &mut buf) {
            next.tls = match v {
                Some("off") => None,
                Some("any") => Some(ServerTrust::Any),
                Some("pinned") => {
                    let text = fingerprint.flatten().map(str::trim).unwrap_or("");
                    let fp = try_parse_fingerprint(text)
                        .ok_or(err("fingerprint", "expected 32 hex bytes, e.g. AB:CD:… as Deskflow shows it"))?;
                    Some(ServerTrust::Pinned(fp))
                }
                _ => return Err(err("tls", "unknown TLS mode")),
            };
        }
        Ok(next)
    }

    /// Writes the settings in the form the page submits; this is also how
    /// they're stored.
    pub fn write_form(&self, w: &mut impl fmt::Write) -> fmt::Result {
        let keys = key_source_name(self.key_source);
        write!(
            w,
            "screen_name={}&screen_width={}&screen_height={}&ble_name={}&key_source={}&mouse_mode={}&typing_layout={}&typing_rate={}&tls=",
            FormStr(&self.screen_name),
            self.screen_width,
            self.screen_height,
            FormStr(&self.ble_name),
            keys,
            mouse_mode_name(self.mouse_mode),
            self.typing_layout.name(),
            self.typing_rate,
        )?;
        match &self.tls {
            None => w.write_str("off")?,
            Some(ServerTrust::Any) => w.write_str("any")?,
            // Hex digits and colons need no encoding.
            Some(ServerTrust::Pinned(fp)) => write!(w, "pinned&fingerprint={}", DisplayFingerprint(fp))?,
        }
        for (actions, (short, long)) in self.buttons.iter().zip(BUTTON_FIELDS) {
            write!(w, "&{}={}&{}={}", short, actions.short.name(), long, actions.long.name())?;
        }
        Ok(())
    }

    /// Applies stored settings (see [`Settings::write_form`]) over `self`,
    /// skipping fields that don't validate. Returns the skipped fields.
    pub fn with_stored(&self, stored: &[u8]) -> (Settings, Vec<&'static str, 8>) {
        let mut skipped: Vec<&'static str, 8> = Vec::new();
        loop {
            // Drop the fields that failed so far. A bad fingerprint takes the
            // TLS mode with it, since "pinned" can't stand without one.
            let drop = |key: &[u8]| {
                skipped.iter().any(|s| s.as_bytes() == key) || (key == b"tls" && skipped.contains(&"fingerprint"))
            };
            let mut body: Vec<u8, FORM_MAX> = Vec::new();
            for pair in stored.split(|&b| b == b'&') {
                let key = pair.split(|&b| b == b'=').next().unwrap_or(pair);
                if !pair.is_empty() && !drop(key) {
                    if !body.is_empty() {
                        let _ = body.push(b'&');
                    }
                    let _ = body.extend_from_slice(pair);
                }
            }
            match self.with_form(&body) {
                Ok(settings) => return (settings, skipped),
                Err(e) => {
                    if skipped.contains(&e.field) || skipped.push(e.field).is_err() {
                        return (self.clone(), skipped);
                    }
                }
            }
        }
    }

    /// Writes the settings as a JSON object.
    pub fn write_json(&self, w: &mut impl fmt::Write) -> fmt::Result {
        let keys = key_source_name(self.key_source);
        let tls = match self.tls {
            None => "off",
            Some(ServerTrust::Any) => "any",
            Some(ServerTrust::Pinned(_)) => "pinned",
        };
        write!(
            w,
            r#"{{"screen_name":{},"screen_width":{},"screen_height":{},"ble_name":{},"key_source":"{}","mouse_mode":"{}","typing_layout":"{}","typing_rate":{},"tls":"{}","fingerprint":"#,
            JsonStr(&self.screen_name),
            self.screen_width,
            self.screen_height,
            JsonStr(&self.ble_name),
            keys,
            mouse_mode_name(self.mouse_mode),
            self.typing_layout.name(),
            self.typing_rate,
            tls,
        )?;
        match &self.tls {
            Some(ServerTrust::Pinned(fp)) => write!(w, r#""{}""#, DisplayFingerprint(fp))?,
            _ => w.write_str("null")?,
        }
        for (actions, (short, long)) in self.buttons.iter().zip(BUTTON_FIELDS) {
            write!(w, r#","{}":"{}","{}":"{}""#, short, actions.short.name(), long, actions.long.name())?;
        }
        w.write_str("}")
    }
}

fn mouse_mode_name(mode: MouseMode) -> &'static str {
    match mode {
        MouseMode::Absolute => "absolute",
        MouseMode::Relative => "relative",
    }
}

/// The key source's name in forms and JSON.
fn key_source_name(source: KeySource) -> &'static str {
    match source {
        KeySource::MacPhysical => "mac",
        KeySource::WindowsPhysical => "windows",
        KeySource::LinuxPhysical => "linux",
        KeySource::KeyId => "keyid",
    }
}

fn key_source_from_name(name: &str) -> Option<KeySource> {
    [KeySource::MacPhysical, KeySource::WindowsPhysical, KeySource::LinuxPhysical, KeySource::KeyId]
        .into_iter()
        .find(|&k| key_source_name(k) == name)
}

/// Deskflow matches screen names against its layout and uses them in its
/// config file; stick to characters that are safe in both.
fn screen_name(v: &str) -> Result<String<SCREEN_NAME_MAX>, FieldError> {
    let field = "screen_name";
    if v.is_empty() {
        return Err(err(field, "required"));
    }
    if !v.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)) {
        return Err(err(field, "letters, digits, '-', '_' and '.' only"));
    }
    String::try_from(v).map_err(|_| err(field, "at most 32 characters"))
}

fn ble_name(v: &str) -> Result<String<BLE_NAME_MAX>, FieldError> {
    let field = "ble_name";
    let v = v.trim();
    if v.is_empty() {
        return Err(err(field, "required"));
    }
    if !v.bytes().all(|b| (0x20..0x7f).contains(&b)) {
        return Err(err(field, "printable ASCII only"));
    }
    String::try_from(v).map_err(|_| err(field, "at most 22 characters"))
}

fn dimension(v: Option<&str>, field: &'static str) -> Result<u16, FieldError> {
    let n: u16 = v.and_then(|v| v.trim().parse().ok()).ok_or(err(field, "not a number"))?;
    if !(SCREEN_MIN..=SCREEN_MAX).contains(&n) {
        return Err(err(field, "between 200 and 16384"));
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> Settings {
        Settings {
            screen_name: String::try_from("arch-laptop").unwrap(),
            screen_width: 1920,
            screen_height: 1200,
            ble_name: String::try_from("Deskflow Bridge").unwrap(),
            key_source: KeySource::MacPhysical,
            mouse_mode: MouseMode::Absolute,
            tls: Some(ServerTrust::Any),
            typing_layout: Layout::Us,
            typing_rate: 40,
            buttons: crate::panel::DEFAULT_BUTTONS,
        }
    }

    #[test]
    fn applies_a_full_form() {
        let fp = "00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF";
        let body = alloc::format!(
            "screen_name=work-pc&screen_width=2560&screen_height=1440&ble_name=My+Bridge%21&key_source=keyid&mouse_mode=relative&typing_layout=de&typing_rate=20&tls=pinned&fingerprint={}",
            fp.replace(':', "%3A")
        );
        let s = defaults().with_form(body.as_bytes()).unwrap();
        assert_eq!(s.screen_name, "work-pc");
        assert_eq!((s.screen_width, s.screen_height), (2560, 1440));
        assert_eq!(s.ble_name, "My Bridge!");
        assert_eq!(s.key_source, KeySource::KeyId);
        assert_eq!(s.mouse_mode, MouseMode::Relative);
        assert_eq!(s.tls, Some(ServerTrust::Pinned(crate::tls::parse_fingerprint(fp))));
        assert_eq!((s.typing_layout, s.typing_rate), (Layout::German, 20));
    }

    #[test]
    fn missing_fields_keep_their_value() {
        let s = defaults().with_form(b"screen_width=1280").unwrap();
        assert_eq!(s, Settings { screen_width: 1280, ..defaults() });
        assert_eq!(defaults().with_form(b"tls=off").unwrap().tls, None);
    }

    #[test]
    fn rejects_bad_values() {
        let d = defaults();
        assert_eq!(d.with_form(b"screen_name=").unwrap_err().field, "screen_name");
        assert_eq!(d.with_form(b"screen_name=has+space").unwrap_err().field, "screen_name");
        assert_eq!(d.with_form(b"screen_name=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap_err().field, "screen_name");
        assert_eq!(d.with_form(b"screen_width=abc").unwrap_err().field, "screen_width");
        assert_eq!(d.with_form(b"screen_height=100").unwrap_err().field, "screen_height");
        assert_eq!(d.with_form(b"screen_height=99999").unwrap_err().field, "screen_height");
        assert_eq!(d.with_form(b"ble_name=12345678901234567890123").unwrap_err().field, "ble_name");
        assert_eq!(d.with_form(b"ble_name=caf%C3%A9").unwrap_err().field, "ble_name");
        assert_eq!(d.with_form(b"key_source=dvorak").unwrap_err().field, "key_source");
        assert_eq!(d.with_form(b"mouse_mode=tablet").unwrap_err().field, "mouse_mode");
        assert_eq!(d.with_form(b"tls=pinned").unwrap_err().field, "fingerprint");
        assert_eq!(d.with_form(b"tls=pinned&fingerprint=AB%3ACD").unwrap_err().field, "fingerprint");
        assert_eq!(d.with_form(b"tls=maybe").unwrap_err().field, "tls");
        assert_eq!(d.with_form(b"typing_layout=dvorak").unwrap_err().field, "typing_layout");
        assert_eq!(d.with_form(b"typing_rate=4").unwrap_err().field, "typing_rate");
        assert_eq!(d.with_form(b"typing_rate=101").unwrap_err().field, "typing_rate");
        assert_eq!(d.with_form(b"button2_long=explode").unwrap_err().field, "button2_long");
    }

    #[test]
    fn json() {
        let mut out = alloc::string::String::new();
        defaults().write_json(&mut out).unwrap();
        assert_eq!(
            out,
            r#"{"screen_name":"arch-laptop","screen_width":1920,"screen_height":1200,"ble_name":"Deskflow Bridge","key_source":"mac","mouse_mode":"absolute","typing_layout":"us","typing_rate":40,"tls":"any","fingerprint":null,"button1_short":"type","button1_long":"stop","button2_short":"next","button2_long":"release","button3_short":"lock","button3_long":"restart"}"#
        );
        let pinned = Settings { tls: Some(ServerTrust::Pinned([0xab; 32])), ..defaults() };
        out.clear();
        pinned.write_json(&mut out).unwrap();
        assert!(out.contains(r#""tls":"pinned","fingerprint":"AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB","button1_short""#));
    }

    fn stored(s: &Settings) -> alloc::string::String {
        let mut out = alloc::string::String::new();
        s.write_form(&mut out).unwrap();
        out
    }

    #[test]
    fn round_trips_through_storage() {
        let base = Settings { screen_name: String::try_from("x").unwrap(), ..defaults() };
        for s in [
            defaults(),
            Settings { ble_name: String::try_from("My & Bridge = 100%").unwrap(), ..defaults() },
            Settings { tls: Some(ServerTrust::Pinned([7; 32])), key_source: KeySource::KeyId, ..defaults() },
            Settings { key_source: KeySource::WindowsPhysical, ..defaults() },
            Settings { key_source: KeySource::LinuxPhysical, mouse_mode: MouseMode::Relative, ..defaults() },
            Settings { typing_layout: Layout::French, typing_rate: 100, ..defaults() },
            Settings { buttons: [ButtonActions { short: Action::Mute, long: Action::None }; BUTTONS], ..defaults() },
            Settings { tls: None, screen_width: 3840, screen_height: 2160, ..defaults() },
        ] {
            let (back, skipped) = base.with_stored(stored(&s).as_bytes());
            assert_eq!(back, s);
            assert!(skipped.is_empty());
        }
    }

    #[test]
    fn stored_settings_skip_only_bad_fields() {
        let d = defaults();
        // From a future firmware: an unknown field and a value we'd reject.
        let (s, skipped) = d.with_stored(b"screen_name=work-pc&wifi_ssid=office&screen_width=99999&ble_name=Desk");
        assert_eq!(s, Settings { screen_name: String::try_from("work-pc").unwrap(), ble_name: String::try_from("Desk").unwrap(), ..d.clone() });
        assert_eq!(&skipped[..], &["screen_width"]);

        // A bad fingerprint drops the pinned mode too, keeping the default.
        let (s, skipped) = d.with_stored(b"tls=pinned&fingerprint=nope&screen_height=1080");
        assert_eq!((s.tls, s.screen_height), (d.tls, 1080));
        assert_eq!(&skipped[..], &["fingerprint"]);

        // Garbage or nothing: the defaults.
        assert_eq!(d.with_stored(b"").0, d);
        assert_eq!(d.with_stored(b"&&=&\xff").0, d);
    }
}
