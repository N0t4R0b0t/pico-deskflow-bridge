//! BLE HID-over-GATT peripheral: keyboard, absolute and relative pointers,
//! and media keys (see `deskflow_proto::hid` for the reports).
//!
//! Linux (BlueZ) only talks to HID services over an encrypted, bonded link,
//! so every HID attribute requires encryption, and the bonds are kept in
//! flash so paired laptops reconnect after a power cycle without re-pairing.
//!
//! Several laptops can be paired, and a peripheral doesn't choose who
//! connects, so all of them try to. While one is active the controller's accept
//! list refuses the others, which just keep trying. Pairing a new laptop opens
//! that up for a while.

use cyw43::bluetooth::BtDriver;
use deskflow_proto::hid::{
    CONSUMER_REPORT_ID, CONSUMER_REPORT_LEN, KEYBOARD_REPORT_ID, KEYBOARD_REPORT_LEN, MOUSE_REPORT_ID, MOUSE_REPORT_LEN,
    REL_MOUSE_REPORT_ID, REL_MOUSE_REPORT_LEN, REPORT_DESCRIPTOR, Report,
};
use bt_hci::cmd::le::{LeAddDeviceToFilterAcceptList, LeClearFilterAcceptList};
use deskflow_proto::pairing::{self, Command};
use embassy_futures::select::{Either, Either3, Either4, select, select3, select4};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Instant, Ticker, Timer, with_timeout};
use trouble_host::prelude::*;

use crate::storage::{self, Bonds, SharedStorage};
use crate::{MOTION, MOTION_READY, Outgoing, REPORTS, flow, status};

const CONNECTIONS_MAX: usize = 1;
/// How long "Pair a new laptop" lets anything connect.
const PAIRING_WINDOW: Duration = Duration::from_secs(120);

/// From the settings page and the hotkeys.
pub static COMMANDS: Channel<CriticalSectionRawMutex, Command, 4> = Channel::new();

/// How often the signal strength is read for the settings page.
const RSSI_INTERVAL: Duration = Duration::from_secs(2);

/// Connection intervals to ask for, (min, max) in microseconds, fastest
/// first: BlueZ's default of 30-50 ms is far too slow for a mouse, and
/// Apple's guidelines reject a minimum under 11.25 ms for HID devices. Each
/// goes out shortly after connecting while the interval is still slower
/// than `FAST_ENOUGH_US`. The first has no range because a host given one may
/// settle on its slow end.
const FAST_INTERVALS: [(u64, u64); 4] = [(7_500, 7_500), (7_500, 15_000), (11_250, 26_250), (15_000, 15_000)];
const FAST_ENOUGH_US: u32 = 15_000;
/// The connection interval, until it's known.
const DEFAULT_INTERVAL_US: u32 = 15_000;
/// A move goes out at this many intervals even if the controller still looks
/// full: it may be slow to report what it sent.
const FLOW_PATIENCE: u32 = 2;
/// The signal isn't read within this long of a pointer report: the read
/// holds up the next one.
const RSSI_QUIET: Duration = Duration::from_secs(1);

type BleController = ExternalController<flow::Observed<BtDriver<'static>>, 10>;
type BleStack<'a> = Stack<'a, BleController, DefaultPacketPool>;
/// Signal + ATT + SMP.
const L2CAP_CHANNELS_MAX: usize = 3;

const REPORT_MAP_LEN: usize = REPORT_DESCRIPTOR.len();
/// The GATT macros want a fixed-size array value.
const REPORT_MAP: [u8; REPORT_MAP_LEN] = {
    let mut map = [0u8; REPORT_MAP_LEN];
    let mut i = 0;
    while i < REPORT_MAP_LEN {
        map[i] = REPORT_DESCRIPTOR[i];
        i += 1;
    }
    map
};

/// Report Reference descriptor (0x2908) report types.
const INPUT: u8 = 1;
const OUTPUT: u8 = 2;

#[gatt_server]
struct Server {
    hid: HidService,
}

#[gatt_service(uuid = service::HUMAN_INTERFACE_DEVICE)]
struct HidService {
    /// bcdHID 1.11, country 0, flags: normally connectable.
    #[characteristic(uuid = "2a4a", read, value = [0x11, 0x01, 0x00, 0x02], permissions(encrypted))]
    hid_info: [u8; 4],
    #[characteristic(uuid = "2a4b", read, value = REPORT_MAP, permissions(encrypted))]
    report_map: [u8; REPORT_MAP_LEN],
    #[characteristic(uuid = "2a4c", write_without_response, permissions(encrypted))]
    control_point: u8,
    /// 1 = report protocol.
    #[characteristic(uuid = "2a4e", read, write_without_response, value = 1, permissions(encrypted))]
    protocol_mode: u8,
    #[descriptor(uuid = "2908", read = encrypted, value = [KEYBOARD_REPORT_ID, INPUT])]
    #[characteristic(uuid = "2a4d", read, notify, permissions(encrypted))]
    keyboard: [u8; KEYBOARD_REPORT_LEN],
    /// Caps/Num Lock LEDs from the laptop; accepted and ignored.
    #[descriptor(uuid = "2908", read = encrypted, value = [KEYBOARD_REPORT_ID, OUTPUT])]
    #[characteristic(uuid = "2a4d", read, write, write_without_response, permissions(encrypted))]
    keyboard_leds: [u8; 1],
    #[descriptor(uuid = "2908", read = encrypted, value = [MOUSE_REPORT_ID, INPUT])]
    #[characteristic(uuid = "2a4d", read, notify, permissions(encrypted))]
    mouse: [u8; MOUSE_REPORT_LEN],
    #[descriptor(uuid = "2908", read = encrypted, value = [REL_MOUSE_REPORT_ID, INPUT])]
    #[characteristic(uuid = "2a4d", read, notify, permissions(encrypted))]
    rel_mouse: [u8; REL_MOUSE_REPORT_LEN],
    #[descriptor(uuid = "2908", read = encrypted, value = [CONSUMER_REPORT_ID, INPUT])]
    #[characteristic(uuid = "2a4d", read, notify, permissions(encrypted))]
    consumer: [u8; CONSUMER_REPORT_LEN],
}

/// The paired laptops and who has the keyboard and mouse.
struct Laptops {
    bonds: Bonds,
    active: Option<usize>,
    /// Anyone may connect.
    pairing: bool,
}

fn address_bytes(bond: &BondInformation) -> [u8; 6] {
    bond.identity.addr.addr.into_inner()
}

impl Laptops {
    fn publish(&self) {
        status::set_ble_bonded(!self.bonds.is_empty());
        status::set_laptops(pairing::Laptops {
            addresses: self.bonds.iter().map(address_bytes).collect(),
            active: self.active,
            connected: false,
            pairing: self.pairing,
        });
    }

    async fn set_active(&mut self, storage: &SharedStorage, slot: Option<usize>) {
        self.active = slot;
        if let Some(bond) = slot.and_then(|i| self.bonds.get(i))
            && storage::save_active(&mut *storage.lock().await, &address_bytes(bond)).await.is_err()
        {
            log::error!("ble: failed to save the active laptop");
        }
    }

    /// Carries out a command. Returns whether the current link has to go.
    async fn apply(&mut self, command: Command, stack: &BleStack<'_>, storage: &SharedStorage) -> bool {
        let count = self.bonds.len();
        let target = match command {
            Command::Pair => {
                log::info!("ble: pairing open for {} s", PAIRING_WINDOW.as_secs());
                self.pairing = true;
                return true;
            }
            Command::Forget(slot) => return self.forget(slot as usize, stack, storage).await,
            Command::Switch(slot) => Some(slot as usize).filter(|&i| i < count),
            Command::Next => pairing::next_slot(self.active, count),
        };
        let Some(target) = target else {
            log::warn!("ble: no such laptop");
            return false;
        };
        if self.active == Some(target) && !self.pairing {
            return false;
        }
        log::info!("ble: switching to laptop {}", target + 1);
        self.pairing = false;
        self.set_active(storage, Some(target)).await;
        true
    }

    async fn forget(&mut self, slot: usize, stack: &BleStack<'_>, storage: &SharedStorage) -> bool {
        if slot >= self.bonds.len() {
            return false;
        }
        let bond = self.bonds.remove(slot);
        log::info!("ble: forgetting laptop {}", slot + 1);
        let _ = stack.remove_bond_information(bond.identity);
        if storage::save_bonds(&mut *storage.lock().await, &self.bonds).await.is_err() {
            log::error!("ble: failed to save the pairings");
        }
        let was_active = self.active == Some(slot);
        let active = match self.active {
            Some(i) if i == slot => (!self.bonds.is_empty()).then_some(0),
            Some(i) if i > slot => Some(i - 1),
            other => other,
        };
        self.set_active(storage, active).await;
        self.pairing |= self.bonds.is_empty();
        was_active
    }

    /// The laptop that just connected, if it's a known one, takes over when
    /// pairing was open.
    async fn claim(&mut self, peer: Address, storage: &SharedStorage) {
        if !self.pairing {
            return;
        }
        if let Some(slot) = self.bonds.iter().position(|b| b.identity.match_address(&peer)) {
            log::info!("ble: laptop {} is active", slot + 1);
            self.pairing = false;
            self.set_active(storage, Some(slot)).await;
        }
    }
}

/// Lets only `peer` connect. Retries because the controller refuses changes
/// until advertising has really stopped.
async fn accept_only(stack: &BleStack<'_>, peer: Address) -> bool {
    for _ in 0..5 {
        if stack.command(LeClearFilterAcceptList::new()).await.is_ok()
            && stack.command(LeAddDeviceToFilterAcceptList::new(peer.kind, peer.addr)).await.is_ok()
        {
            return true;
        }
        Timer::after_millis(50).await;
    }
    log::warn!("ble: couldn't restrict connections; any paired laptop may connect");
    false
}

pub async fn run(bt: BtDriver<'static>, storage: &SharedStorage, name: &str, uid: [u8; 8]) -> ! {
    let controller: BleController = ExternalController::new(flow::Observed(bt));

    // Random static address derived from the chip's unique ID: stable across
    // reboots, distinct per board. The two most significant bits must be set.
    let mut addr = [0u8; 6];
    addr.copy_from_slice(&uid[..6]);
    addr[5] |= 0xC0;
    let address = Address::random(addr);
    log::info!("ble: address {:?}", address);

    let mut resources: HostResources<DefaultPacketPool, CONNECTIONS_MAX, L2CAP_CHANNELS_MAX> = HostResources::new();
    let stack = trouble_host::new(controller, &mut resources)
        .set_random_address(address)
        // No display or keypad: "Just Works" pairing.
        .set_io_capabilities(IoCapabilities::NoInputNoOutput)
        .build();

    let bonds = storage::load_bonds(&mut *storage.lock().await).await;
    for bond in &bonds {
        log::info!("ble: restoring pairing with {:?}", bond.identity.addr);
        let _ = stack.add_bond_information(bond.clone());
    }
    if bonds.is_empty() {
        log::info!("ble: no laptop paired yet; waiting for pairing");
    }
    let last_active = storage::load_active(&mut *storage.lock().await).await;
    let active = bonds.iter().position(|b| Some(address_bytes(b)) == last_active).or((!bonds.is_empty()).then_some(0));
    let mut laptops = Laptops { pairing: bonds.is_empty(), active, bonds };

    let server = Server::new_with_config(GapConfig::Peripheral(PeripheralConfig {
        name,
        appearance: &appearance::human_interface_device::GENERIC_HUMAN_INTERFACE_DEVICE,
    }))
    .unwrap();

    let mut runner = stack.runner();
    let mut peripheral = stack.peripheral();

    let host = async {
        loop {
            if let Err(e) = runner.run().await {
                log::error!("ble: host stopped: {:?}", e);
            }
        }
    };

    let app = async {
        loop {
            laptops.publish();
            let target = laptops.active.filter(|_| !laptops.pairing).map(|i| laptops.bonds[i].identity.addr);
            let filtered = match target {
                Some(peer) => accept_only(&stack, peer).await,
                None => false,
            };
            let window_ends = if laptops.pairing && !laptops.bonds.is_empty() {
                Instant::now() + PAIRING_WINDOW
            } else {
                Instant::MAX
            };
            // Reports produced while nobody is listening are stale by the time
            // the laptop connects, so throw them away.
            let advertising = advertise(&mut peripheral, &server, name, filtered);
            let conn = match select4(advertising, discard_reports(), COMMANDS.receive(), Timer::at(window_ends)).await {
                Either4::First(Ok(conn)) => conn,
                Either4::First(Err(e)) => {
                    log::error!("ble: advertising failed: {:?}", e);
                    Timer::after_millis(200).await;
                    continue;
                }
                Either4::Second(never) => match never {},
                Either4::Third(command) => {
                    laptops.apply(command, &stack, storage).await;
                    continue;
                }
                Either4::Fourth(()) => {
                    log::info!("ble: pairing closed");
                    laptops.pairing = false;
                    continue;
                }
            };
            log::info!("ble: connected");
            status::set_ble_connected(true);
            status::set_ble_interval_us(conn.raw().params().conn_interval.as_micros() as u32);
            if conn.raw().set_bondable(true).is_err() {
                log::warn!("ble: could not mark connection bondable");
            }
            laptops.claim(conn.raw().peer_address(), storage).await;
            laptops.publish();
            loop {
                let events = gatt_events(&server, &conn, storage, &mut laptops);
                match select3(events, send_reports(&stack, &server, &conn), COMMANDS.receive()).await {
                    Either3::First(()) | Either3::Second(()) => break,
                    Either3::Third(command) => {
                        let leave = laptops.apply(command, &stack, storage).await;
                        laptops.publish();
                        if leave {
                            conn.raw().disconnect();
                            let events = gatt_events(&server, &conn, storage, &mut laptops);
                            let _ = with_timeout(Duration::from_secs(2), events).await;
                            break;
                        }
                    }
                }
            }
            status::set_ble_connected(false);
            log::info!("ble: disconnected");
        }
    };

    match select(host, app).await {
        Either::First(never) | Either::Second(never) => never,
    }
}

async fn advertise<'v, 's, C: Controller>(
    peripheral: &mut Peripheral<'v, C, DefaultPacketPool>,
    server: &'s Server<'v>,
    name: &str,
    filtered: bool,
) -> Result<GattConnection<'v, 's, DefaultPacketPool>, BleHostError<C::Error>> {
    let mut adv = [0u8; 31];
    let len = AdStructure::encode_slice(
        &[
            AdStructure::Flags(LE_GENERAL_DISCOVERABLE | BR_EDR_NOT_SUPPORTED),
            AdStructure::IncompleteServiceUuids16(&[service::HUMAN_INTERFACE_DEVICE.to_le_bytes()]),
            AdStructure::CompleteLocalName(name.as_bytes()),
        ],
        &mut adv[..],
    )?;
    let params = AdvertisementParameters {
        filter_policy: if filtered { AdvFilterPolicy::FilterConn } else { AdvFilterPolicy::Unfiltered },
        ..Default::default()
    };
    let advertiser = peripheral
        .advertise(
            &params,
            Advertisement::ConnectableScannableUndirected { adv_data: &adv[..len], scan_data: &[] },
        )
        .await?;
    log::info!("ble: advertising as '{}'", name);
    Ok(advertiser.accept().await?.with_attribute_server(server)?)
}

/// Answers GATT requests and saves the bond once pairing completes.
async fn gatt_events(
    server: &Server<'_>,
    conn: &GattConnection<'_, '_, DefaultPacketPool>,
    storage: &SharedStorage,
    laptops: &mut Laptops,
) {
    loop {
        match conn.next().await {
            GattConnectionEvent::Disconnected { reason } => {
                log::info!("ble: link closed: {:?}", reason);
                return;
            }
            GattConnectionEvent::PairingComplete { security_level, bond } => {
                log::info!("ble: paired, security {:?}", security_level);
                if let Some(bond) = bond {
                    let identity = bond.identity;
                    if let Some(dropped) = pairing::remember(&mut laptops.bonds, bond, |a, b| a.identity == b.identity) {
                        log::info!("ble: forgetting the oldest pairing ({:?})", dropped.identity.addr);
                    }
                    match storage::save_bonds(&mut *storage.lock().await, &laptops.bonds).await {
                        Ok(()) => log::info!("ble: pairing saved ({} laptops)", laptops.bonds.len()),
                        Err(_) => log::error!("ble: failed to save the pairing"),
                    }
                    let slot = laptops.bonds.iter().position(|b| b.identity == identity);
                    laptops.pairing = false;
                    laptops.set_active(storage, slot).await;
                    laptops.publish();
                }
            }
            // Fires for new pairings and for bonded laptops coming back.
            GattConnectionEvent::Encrypted { security_level, .. } => {
                log::info!("ble: link encrypted, security {:?}", security_level);
                subscribe_reports(server, conn);
            }
            GattConnectionEvent::PairingFailed(e) => log::warn!("ble: pairing failed: {:?}", e),
            GattConnectionEvent::ConnectionParamsUpdated { conn_interval, peripheral_latency, .. } => {
                log::info!("ble: connection interval {} us, latency {}", conn_interval.as_micros(), peripheral_latency);
                status::set_ble_interval_us(conn_interval.as_micros() as u32);
            }
            GattConnectionEvent::Gatt { event } => match event.accept() {
                Ok(reply) => reply.send().await,
                Err(e) => log::warn!("ble: GATT reply failed: {:?}", e),
            },
            _ => {}
        }
    }
}

/// Sends queued reports; also reads the signal strength now and then.
///
/// Plain moves wait while the controller is busy (see `flow`):
/// held back here they keep merging, so the radio always carries the newest,
/// at whatever rate the link manages. Clicks, the wheel and keys don't wait.
async fn send_reports(stack: &BleStack<'_>, server: &Server<'_>, conn: &GattConnection<'_, '_, DefaultPacketPool>) {
    REPORTS.clear();
    take_motion();
    flow::reset();
    let mut rssi = Ticker::every(RSSI_INTERVAL);
    let mut fast_requests = 0;
    let mut buttons = 0;
    let mut give_up = Instant::now();
    let mut last_pointer = Instant::now();
    loop {
        // The queue first: whatever a merged move displaced is older.
        let waiting = MOTION.lock(|slot| slot.get());
        let overdue = Instant::now() >= give_up;
        let due = waiting.is_some_and(|m| flow::has_room() || overdue || !m.report.is_plain_move(buttons));
        let out = match REPORTS.try_receive().ok().or_else(|| if due { take_motion() } else { None }) {
            Some(out) => {
                if due && out.report.buttons().is_some() && !flow::has_room() && overdue {
                    status::note_flow_timeout();
                }
                out
            }
            None => {
                let wake = if waiting.is_some() { give_up } else { Instant::MAX };
                match select4(REPORTS.receive(), MOTION_READY.wait(), Timer::at(wake), rssi.next()).await {
                    Either4::First(out) => out,
                    Either4::Second(()) | Either4::Third(()) => continue,
                    Either4::Fourth(()) => {
                        if last_pointer.elapsed() > RSSI_QUIET
                            && let Ok(rssi) = conn.raw().rssi(stack).await
                        {
                            status::set_ble_rssi(rssi);
                        }
                        if status::ble_interval_us().is_none_or(|us| us > FAST_ENOUGH_US)
                            && let Some(&(min, max)) = FAST_INTERVALS.get(fast_requests)
                        {
                            fast_requests += 1;
                            request_interval(stack, conn, min, max).await;
                        }
                        continue;
                    }
                }
            }
        };
        let sent = match out.report {
            Report::Keyboard(r) => server.hid.keyboard.notify(conn, &r, true).await,
            Report::Mouse(r) => server.hid.mouse.notify(conn, &r, true).await,
            Report::RelMouse(r) => server.hid.rel_mouse.notify(conn, &r, true).await,
            Report::Consumer(r) => server.hid.consumer.notify(conn, &r, true).await,
        };
        if sent.is_err() {
            return;
        }
        if let Some(b) = out.report.buttons() {
            buttons = b;
            let interval = status::ble_interval_us().unwrap_or(DEFAULT_INTERVAL_US);
            status::note_in_chip(flow::in_controller());
            last_pointer = Instant::now();
            give_up = last_pointer + Duration::from_micros((interval * FLOW_PATIENCE) as u64);
        }
        if let Some(since) = out.since {
            status::note_latency_us(Instant::now().duration_since(since).as_micros() as u32);
        }
    }
}

/// A bonded host may expect its report subscriptions to outlive the link
/// (Windows doesn't renew them), but they're only kept in RAM, so after a
/// restart it would connect and get no input until pairing again. Every
/// report is meant to be notified, so turn them all on for a secure session.
fn subscribe_reports(server: &Server<'_>, conn: &GattConnection<'_, '_, DefaultPacketPool>) {
    let Some(mut table) = server.get_client_att_table(conn.raw()) else {
        log::warn!("ble: no subscription table for this laptop");
        return;
    };
    let hid = &server.hid;
    let cccds = [hid.keyboard.cccd_handle, hid.mouse.cccd_handle, hid.rel_mouse.cccd_handle, hid.consumer.cccd_handle];
    for cccd in cccds.into_iter().flatten() {
        if table.write(cccd, 0, &[1, 0]).is_err() {
            log::warn!("ble: couldn't restore subscription {}", cccd);
        }
    }
    server.set_client_att_table(conn.raw(), &table.view());
}

async fn request_interval(stack: &BleStack<'_>, conn: &GattConnection<'_, '_, DefaultPacketPool>, min_us: u64, max_us: u64) {
    log::info!("ble: asking for a connection interval of {}-{} us", min_us, max_us);
    let params = RequestedConnParams {
        min_connection_interval: Duration::from_micros(min_us),
        max_connection_interval: Duration::from_micros(max_us),
        max_latency: 0,
        supervision_timeout: Duration::from_secs(2),
        ..Default::default()
    };
    if let Err(e) = conn.raw().update_connection_params(stack, &params).await {
        log::warn!("ble: couldn't ask for a faster connection: {:?}", e);
    }
}

fn take_motion() -> Option<Outgoing> {
    MOTION.lock(|slot| slot.take())
}

async fn discard_reports() -> core::convert::Infallible {
    loop {
        select(REPORTS.receive(), MOTION_READY.wait()).await;
        take_motion();
    }
}
