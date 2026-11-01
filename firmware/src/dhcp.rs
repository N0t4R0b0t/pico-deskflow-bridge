//! Single-lease DHCP server for the USB link.
//!
//! There is exactly one client (the Mac's USB network adapter), so every
//! DISCOVER gets `HOST_IP` and every REQUEST is ACKed. The offer deliberately
//! carries no router or DNS option: the Mac must keep using its own
//! network for everything except this point-to-point link.

use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{IpEndpoint, Ipv4Address, Stack};

use crate::config;

const SERVER_PORT: u16 = 67;
const CLIENT_PORT: u16 = 68;
const MAGIC: [u8; 4] = [0x63, 0x82, 0x53, 0x63];
/// Fixed BOOTP header up to and including the magic cookie.
const HEADER_LEN: usize = 240;
const LEASE_SECS: u32 = 24 * 3600;

const DISCOVER: u8 = 1;
const OFFER: u8 = 2;
const REQUEST: u8 = 3;
const ACK: u8 = 5;

#[embassy_executor::task]
pub async fn server_task(stack: Stack<'static>) -> ! {
    let mut rx_meta = [PacketMetadata::EMPTY; 4];
    let mut tx_meta = [PacketMetadata::EMPTY; 4];
    let mut rx_buf = [0u8; 600];
    let mut tx_buf = [0u8; 600];
    let mut socket = UdpSocket::new(stack, &mut rx_meta, &mut rx_buf, &mut tx_meta, &mut tx_buf);
    socket.bind(SERVER_PORT).unwrap();

    let mut req = [0u8; 576];
    let mut reply = [0u8; 300];
    loop {
        let Ok((n, _)) = socket.recv_from(&mut req).await else { continue };
        let Some(kind) = message_type(&req[..n]) else { continue };
        let answer = match kind {
            DISCOVER => OFFER,
            REQUEST => ACK,
            _ => continue,
        };
        let len = build_reply(&req[..n], answer, &mut reply);
        // The client has no address yet, so reply by broadcast.
        let to = IpEndpoint::new(Ipv4Address::BROADCAST.into(), CLIENT_PORT);
        match socket.send_to(&reply[..len], to).await {
            Ok(()) if answer == ACK => log::info!("dhcp: leased {} to the host", config::HOST_IP),
            Ok(()) => {}
            Err(e) => log::warn!("dhcp: send failed: {:?}", e),
        }
    }
}

/// Option 53 of a BOOTREQUEST, if this is a well-formed DHCP message.
fn message_type(pkt: &[u8]) -> Option<u8> {
    if pkt.len() < HEADER_LEN || pkt[0] != 1 || pkt[236..240] != MAGIC {
        return None;
    }
    let mut opts = &pkt[HEADER_LEN..];
    while let [code, rest @ ..] = opts {
        match *code {
            0 => opts = rest,
            255 => break,
            _ => {
                let [len, rest @ ..] = rest else { break };
                let len = *len as usize;
                if rest.len() < len {
                    break;
                }
                if *code == 53 && len == 1 {
                    return Some(rest[0]);
                }
                opts = &rest[len..];
            }
        }
    }
    None
}

fn build_reply(req: &[u8], kind: u8, out: &mut [u8]) -> usize {
    out.fill(0);
    out[0] = 2; // BOOTREPLY
    out[1] = 1; // Ethernet
    out[2] = 6; // MAC length
    out[4..8].copy_from_slice(&req[4..8]); // xid
    out[10..12].copy_from_slice(&req[10..12]); // flags
    out[16..20].copy_from_slice(&config::HOST_IP.octets()); // yiaddr
    out[20..24].copy_from_slice(&config::PICO_IP.octets()); // siaddr
    out[28..44].copy_from_slice(&req[28..44]); // chaddr
    out[236..240].copy_from_slice(&MAGIC);

    let mask = u32::MAX << (32 - config::PREFIX_LEN);
    let mut i = HEADER_LEN;
    let mut opt = |code: u8, data: &[u8]| {
        out[i] = code;
        out[i + 1] = data.len() as u8;
        out[i + 2..i + 2 + data.len()].copy_from_slice(data);
        i += 2 + data.len();
    };
    opt(53, &[kind]);
    opt(54, &config::PICO_IP.octets()); // server identifier
    opt(51, &LEASE_SECS.to_be_bytes());
    opt(1, &mask.to_be_bytes()); // subnet mask
    out[i] = 255;
    i + 1
}
