//! Deskflow client protocol, independent of any transport or hardware.
//!
//! Deskflow (and Synergy/Barrier before it) speaks a simple binary protocol
//! over TCP port 24800. Every message is a 4-byte big-endian length followed by
//! that many payload bytes; the payload begins with a 4-character message code
//! (`DKDN`, `DMMV`, …) except for the initial hello. See [`wire`] for the codec
//! and [`hid::HidMapper`] for turning the input messages into USB/BLE HID
//! reports.
//!
//! The rest is the device around it, kept here so it can be tested on the
//! host: [`tls`] for the encrypted connection, [`clipboard`] and [`typing`]
//! for typing the server's clipboard on the laptop, [`settings`] and [`web`]
//! for the configuration page (with [`auth`] for its password and [`diag`]
//! for its diagnostics), [`logring`] for the log it shows, and
//! [`indicator`] for the status LED.
#![cfg_attr(not(test), no_std)]

// RSA signature checks (`tls`) allocate.
extern crate alloc;

pub mod auth;
pub mod clipboard;
pub mod diag;
pub mod flow;
pub mod hid;
pub mod indicator;
pub mod keymap;
mod layouts;
pub mod logring;
pub mod pairing;
pub mod panel;
pub mod settings;
pub mod tls;
pub mod typing;
pub mod uf2;
pub mod web;
pub mod wire;

/// Protocol version this client announces. 1.6 is the newest version that
/// sends key events without the language-code suffix added in 1.8, and every
/// current Deskflow server still accepts it.
pub const PROTOCOL_MAJOR: i16 = 1;
pub const PROTOCOL_MINOR: i16 = 6;

/// Default Deskflow server port.
pub const DEFAULT_PORT: u16 = 24800;
