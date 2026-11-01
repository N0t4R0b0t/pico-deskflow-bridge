//! Build-time settings.
//!
//! The first group are defaults: once settings are saved from the web page
//! (`http://PICO_IP/`) the saved values win, until "Save" is used again.
//! The network settings below them are fixed, since changing them from the
//! page could make the page unreachable.

use deskflow_proto::hid::MouseMode;
use deskflow_proto::keymap::KeySource;
use deskflow_proto::panel::DEFAULT_BUTTONS;
use deskflow_proto::settings::Settings;
use deskflow_proto::typing::Layout;
#[allow(unused_imports)]
use deskflow_proto::tls::{ServerTrust, parse_fingerprint};
use embassy_net::Ipv4Address;

/// Screen name announced to Deskflow. Must match a screen in the server's
/// layout (Deskflow → Configure Server).
const SCREEN_NAME: &str = "arch-laptop";

/// Resolution reported to Deskflow. The pointer is absolute, so only the
/// aspect ratio really matters, but using the laptop's real (scaled)
/// resolution keeps cursor speed across the screen edge consistent.
const SCREEN_WIDTH: u16 = 1920;
const SCREEN_HEIGHT: u16 = 1200;

/// The Deskflow server's OS, whose key codes are forwarded as physical keys:
/// `MacPhysical`, `WindowsPhysical` or `LinuxPhysical` (`KeyId` translates
/// characters instead, assuming a US layout).
const KEY_SOURCE: KeySource = KeySource::MacPhysical;

/// `Absolute` places the laptop's cursor exactly where Deskflow puts it;
/// `Relative` sends movement like a real mouse, for laptops where an
/// absolute pointer misbehaves (several monitors).
const MOUSE_MODE: MouseMode = MouseMode::Absolute;

/// Deskflow's "Use TLS encryption" setting, which is on by default. `None`
/// if it's turned off on the server.
///
/// With `ServerTrust::Any` the link is encrypted but any server is
/// accepted; the server's fingerprint is printed to the log on connect.
/// To pin it, copy it from Deskflow (the fingerprint shown in its TLS
/// settings), from the log, or use the web page:
///
/// ```ignore
/// const TLS: Option<ServerTrust> =
///     Some(ServerTrust::Pinned(parse_fingerprint("AB:CD:…")));
/// ```
const TLS: Option<ServerTrust> = Some(ServerTrust::Any);

/// The laptop's keyboard layout, for typing the clipboard.
const TYPING_LAYOUT: Layout = Layout::Us;
/// Clipboard typing speed, characters per second (5–100).
const TYPING_RATE: u8 = 40;

/// Bluetooth name the laptop sees when pairing (at most 22 characters).
const BLE_NAME: &str = "Deskflow Bridge";

pub fn defaults() -> Settings {
    Settings {
        screen_name: heapless::String::try_from(SCREEN_NAME).expect("SCREEN_NAME too long"),
        screen_width: SCREEN_WIDTH,
        screen_height: SCREEN_HEIGHT,
        ble_name: heapless::String::try_from(BLE_NAME).expect("BLE_NAME too long"),
        key_source: KEY_SOURCE,
        mouse_mode: MOUSE_MODE,
        buttons: DEFAULT_BUTTONS,
        typing_layout: TYPING_LAYOUT,
        typing_rate: TYPING_RATE,
        tls: TLS,
    }
}

/// Point-to-point USB network between the Pico and the Mac. The Pico hands
/// the Mac `HOST_IP` over DHCP (without a router or DNS, so the Mac never
/// routes other traffic through it) and dials Deskflow there. Pick a subnet
/// the Mac's other networks don't use.
pub const PICO_IP: Ipv4Address = Ipv4Address::new(192, 168, 77, 1);
pub const HOST_IP: Ipv4Address = Ipv4Address::new(192, 168, 77, 2);
pub const PREFIX_LEN: u8 = 24;

pub const DESKFLOW_PORT: u16 = deskflow_proto::DEFAULT_PORT;
