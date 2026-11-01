//! Live state shared between the tasks, for the web page and the LED.

use core::cell::{Cell, RefCell};

use deskflow_proto::diag::{Latency, LatencyWindow};
use deskflow_proto::pairing::Laptops;
use deskflow_proto::panel::LEDS as LEDS_COUNT;
use deskflow_proto::tls::Fingerprint;
use deskflow_proto::web::DeskflowState;
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use portable_atomic::{AtomicBool, AtomicI8, AtomicU8, AtomicU32, Ordering};

static DESKFLOW: AtomicU8 = AtomicU8::new(0);
static BLE_CONNECTED: AtomicBool = AtomicBool::new(false);
static BLE_BONDED: AtomicBool = AtomicBool::new(false);
/// Bumped for every report forwarded to the laptop.
static ACTIVITY: AtomicU32 = AtomicU32::new(0);
static SERVER_FINGERPRINT: Mutex<CriticalSectionRawMutex, Cell<Option<Fingerprint>>> = Mutex::new(Cell::new(None));
/// `None` until the TLS identity has been loaded or generated.
static CLIENT_FINGERPRINT: Mutex<CriticalSectionRawMutex, Cell<Option<Fingerprint>>> = Mutex::new(Cell::new(None));
/// Duration of the last TLS handshake; 0 = none yet.
static TLS_HANDSHAKE_MS: AtomicU32 = AtomicU32::new(0);
/// Diagnostics. RSSI 0 and interval 0 mean "not connected".
static BLE_RSSI: AtomicI8 = AtomicI8::new(0);
static BLE_INTERVAL_US: AtomicU32 = AtomicU32::new(0);
static BLE_CONNECTIONS: AtomicU32 = AtomicU32::new(0);
static DESKFLOW_SESSIONS: AtomicU32 = AtomicU32::new(0);
static LATENCY: Mutex<CriticalSectionRawMutex, RefCell<LatencyWindow>> = Mutex::new(RefCell::new(LatencyWindow::new()));
static TLS_PATH: Mutex<CriticalSectionRawMutex, RefCell<LatencyWindow>> = Mutex::new(RefCell::new(LatencyWindow::new()));
static BACKLOG: Mutex<CriticalSectionRawMutex, RefCell<LatencyWindow>> = Mutex::new(RefCell::new(LatencyWindow::new()));
static IN_CHIP: Mutex<CriticalSectionRawMutex, RefCell<LatencyWindow>> = Mutex::new(RefCell::new(LatencyWindow::new()));
static FLOW_TIMEOUTS: AtomicU32 = AtomicU32::new(0);
static BLE_PAIRING: AtomicBool = AtomicBool::new(false);
static ATTENTION: AtomicBool = AtomicBool::new(false);
/// The LEDs, one bit each, for the virtual panel.
static LEDS: AtomicU8 = AtomicU8::new(0);
static WEB_SERVED: AtomicBool = AtomicBool::new(false);
static LAPTOPS: Mutex<CriticalSectionRawMutex, RefCell<Option<Laptops>>> = Mutex::new(RefCell::new(None));

pub fn set_deskflow(state: DeskflowState) {
    DESKFLOW.store(state as u8, Ordering::Relaxed);
}

pub fn deskflow() -> DeskflowState {
    match DESKFLOW.load(Ordering::Relaxed) {
        0 => DeskflowState::WaitingForLink,
        1 => DeskflowState::Connecting,
        2 => DeskflowState::Connected,
        _ => DeskflowState::Active,
    }
}

pub fn deskflow_connected() -> bool {
    matches!(deskflow(), DeskflowState::Connected | DeskflowState::Active)
}

pub fn set_ble_connected(connected: bool) {
    BLE_CONNECTED.store(connected, Ordering::Relaxed);
    if connected {
        BLE_CONNECTIONS.add(1, Ordering::Relaxed);
    } else {
        BLE_RSSI.store(0, Ordering::Relaxed);
        BLE_INTERVAL_US.store(0, Ordering::Relaxed);
    }
}

pub fn ble_connected() -> bool {
    BLE_CONNECTED.load(Ordering::Relaxed)
}

pub fn set_ble_bonded(bonded: bool) {
    BLE_BONDED.store(bonded, Ordering::Relaxed);
}

pub fn ble_bonded() -> bool {
    BLE_BONDED.load(Ordering::Relaxed)
}

pub fn note_activity() {
    ACTIVITY.add(1, Ordering::Relaxed);
}

pub fn activity() -> u32 {
    ACTIVITY.load(Ordering::Relaxed)
}

pub fn set_laptops(laptops: Laptops) {
    BLE_PAIRING.store(laptops.pairing, Ordering::Relaxed);
    LAPTOPS.lock(|l| *l.borrow_mut() = Some(laptops));
}

pub fn ble_pairing() -> bool {
    BLE_PAIRING.load(Ordering::Relaxed)
}

/// A problem the log explains, for the Attention LED.
pub fn set_attention(on: bool) {
    ATTENTION.store(on, Ordering::Relaxed);
}

pub fn attention() -> bool {
    ATTENTION.load(Ordering::Relaxed)
}

/// Counts the settings page reaching the device, for confirming an update.
pub fn note_web_served() {
    WEB_SERVED.store(true, Ordering::Relaxed);
}

#[cfg_attr(not(feature = "pico2-w"), allow(dead_code))]
pub fn web_served() -> bool {
    WEB_SERVED.load(Ordering::Relaxed)
}

pub fn set_leds(leds: [bool; LEDS_COUNT]) {
    let bits = leds.iter().enumerate().fold(0, |bits, (i, &on)| bits | (on as u8) << i);
    LEDS.store(bits, Ordering::Relaxed);
}

pub fn leds() -> [bool; LEDS_COUNT] {
    let bits = LEDS.load(Ordering::Relaxed);
    core::array::from_fn(|i| bits & 1 << i != 0)
}

/// `None` until Bluetooth is up.
pub fn laptops() -> Laptops {
    let mut laptops = LAPTOPS.lock(|l| l.borrow().clone()).unwrap_or_default();
    laptops.connected = ble_connected();
    laptops
}

/// The certificate the server presented last, even if it was rejected (so
/// the page can offer to pin it).
pub fn set_server_fingerprint(fp: Fingerprint) {
    SERVER_FINGERPRINT.lock(|c| c.set(Some(fp)));
}

pub fn server_fingerprint() -> Option<Fingerprint> {
    SERVER_FINGERPRINT.lock(Cell::get)
}

pub fn set_client_fingerprint(fp: Fingerprint) {
    CLIENT_FINGERPRINT.lock(|c| c.set(Some(fp)));
}

pub fn client_fingerprint() -> Option<Fingerprint> {
    CLIENT_FINGERPRINT.lock(Cell::get)
}

pub fn set_tls_handshake_ms(ms: u32) {
    TLS_HANDSHAKE_MS.store(ms.max(1), Ordering::Relaxed);
}

pub fn tls_handshake_ms() -> Option<u32> {
    Some(TLS_HANDSHAKE_MS.load(Ordering::Relaxed)).filter(|&ms| ms > 0)
}

pub fn set_ble_rssi(rssi: i8) {
    BLE_RSSI.store(rssi, Ordering::Relaxed);
}

pub fn ble_rssi() -> Option<i8> {
    Some(BLE_RSSI.load(Ordering::Relaxed)).filter(|&r| r != 0)
}

pub fn set_ble_interval_us(us: u32) {
    BLE_INTERVAL_US.store(us, Ordering::Relaxed);
}

pub fn ble_interval_us() -> Option<u32> {
    Some(BLE_INTERVAL_US.load(Ordering::Relaxed)).filter(|&us| us > 0)
}

pub fn ble_connections() -> u32 {
    BLE_CONNECTIONS.load(Ordering::Relaxed)
}

/// Counts a session the server greeted.
pub fn note_deskflow_session() {
    DESKFLOW_SESSIONS.add(1, Ordering::Relaxed);
}

pub fn deskflow_sessions() -> u32 {
    DESKFLOW_SESSIONS.load(Ordering::Relaxed)
}

/// Time a report spent in the bridge, from the Deskflow message to the
/// Bluetooth notification.
pub fn note_latency_us(us: u32) {
    LATENCY.lock(|l| l.borrow_mut().add(us));
}

/// Time from the socket through TLS on core 1 to the parsed message.
pub fn note_tls_path_us(us: u32) {
    TLS_PATH.lock(|l| l.borrow_mut().add(us));
}

/// Bytes queued ahead of the Deskflow session as a move is handled.
pub fn note_backlog(bytes: u32) {
    BACKLOG.lock(|l| l.borrow_mut().add(bytes));
}

pub fn backlog() -> Option<Latency> {
    BACKLOG.lock(|l| l.borrow().last())
}

/// Packets in the Bluetooth controller just after a pointer report.
pub fn note_in_chip(packets: u32) {
    IN_CHIP.lock(|l| l.borrow_mut().add(packets));
}

pub fn in_chip() -> Option<Latency> {
    IN_CHIP.lock(|l| l.borrow().last())
}

/// Counts a move that went out without the last one being reported sent.
pub fn note_flow_timeout() {
    FLOW_TIMEOUTS.add(1, Ordering::Relaxed);
}

pub fn flow_timeouts() -> u32 {
    FLOW_TIMEOUTS.load(Ordering::Relaxed)
}

pub fn tls_path() -> Option<Latency> {
    TLS_PATH.lock(|l| l.borrow().last())
}

pub fn latency() -> Option<Latency> {
    LATENCY.lock(|l| l.borrow().last())
}
