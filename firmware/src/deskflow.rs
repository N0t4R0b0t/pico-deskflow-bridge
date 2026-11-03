//! Deskflow client: dials the server on the Mac over the USB link, optionally
//! through TLS, and turns its input messages into HID reports for the BLE
//! task.

use deskflow_proto::clipboard::{self as clip, CHUNK_HEADER_LEN, Event};
use deskflow_proto::hid::{HidMapper, Report};
use deskflow_proto::keymap;
use deskflow_proto::pairing::Command;
use deskflow_proto::settings::Settings;
use deskflow_proto::tls::{self, DbLine, DisplayFingerprint, Fingerprint, Rejection, ServerTrust};
use deskflow_proto::typing::{is_paste_hotkey, laptop_hotkey};
use deskflow_proto::web::DeskflowState;
use deskflow_proto::wire::{self, FRAME_HEADER_LEN, Message};
use embassy_futures::select::{Either, select};
use embassy_net::Stack;
use embassy_net::tcp::TcpSocket;
use embassy_time::{Duration, Instant, Timer, with_timeout};
use embedded_io_async::{Read, Write};
use embedded_tls::TlsError;
use sequential_storage::map::PostcardValue;
use serde::{Deserialize, Serialize};

use crate::clipboard;
use crate::storage::{ITEM_BUF, SharedStorage, Storage, TLS_CLIENT_KEY};
use crate::tls_offload::{self, PlainLink};
use portable_atomic::Ordering;

use crate::{MOTION, MOTION_READY, ble, Outgoing, REPORTS, config, csprng, status};

/// The server sends a keep-alive every 3 s and gives up after three missed
/// replies; we do the same in the other direction.
const SERVER_SILENCE_TIMEOUT: Duration = Duration::from_secs(10);
const RETRY_DELAY: Duration = Duration::from_secs(2);
/// Largest message read whole. Bigger ones are read in pieces: clipboard
/// chunks are streamed into the clipboard, file transfers are discarded.
const MAX_MESSAGE: usize = 512;

// Fields are only read through `Debug` when the session-ended reason is logged.
#[allow(dead_code)]
#[derive(Debug)]
enum Error {
    Transport,
    Tls(TlsError),
    Timeout,
    Protocol,
    Rejected(&'static str),
}

#[derive(Serialize, Deserialize)]
struct StoredKey([u8; 32]);
impl PostcardValue<'_> for StoredKey {}

/// Loads the TLS client key from flash, generating and saving one on first
/// boot, has core 1 build the identity from it, and logs the fingerprint
/// the Deskflow server has to trust.
async fn client_identity(storage: &mut Storage) -> Fingerprint {
    let mut buf = [0u8; ITEM_BUF];
    let stored = match storage.fetch_item::<StoredKey>(&mut buf, &TLS_CLIENT_KEY).await {
        Ok(stored) => stored.map(|StoredKey(secret)| secret),
        Err(_) => {
            log::warn!("deskflow: TLS key storage unreadable");
            None
        }
    };
    let loaded = match stored {
        Some(secret) => tls_offload::load_identity(secret).await,
        None => None,
    };
    let fingerprint = match loaded {
        Some(fp) => fp,
        None => loop {
            let secret = tls::generate_secret(&mut csprng::Rng);
            if let Some(fp) = tls_offload::load_identity(secret).await {
                match storage.store_item(&mut buf, &TLS_CLIENT_KEY, &StoredKey(secret)).await {
                    Ok(()) => log::info!("deskflow: generated a TLS client key"),
                    Err(_) => log::error!("deskflow: failed to save TLS client key; it will change on reboot"),
                }
                break fp;
            }
        },
    };
    log::info!("deskflow: client fingerprint {}", DisplayFingerprint(&fingerprint));
    log::info!("deskflow: (trusted-clients line: {})", DbLine(&fingerprint));
    fingerprint
}

#[embassy_executor::task]
pub async fn client_task(stack: Stack<'static>, settings: &'static Settings, storage: &'static SharedStorage) -> ! {
    // The identity is built on core 1, but storing a new key writes flash,
    // which pauses core 1 and briefly blocks interrupts: leave it until the
    // host has configured the USB network.
    stack.wait_link_up().await;
    let identity = client_identity(&mut *storage.lock().await).await;
    status::set_client_fingerprint(identity);

    let mut rx_buf = [0u8; 2048];
    let mut tx_buf = [0u8; 512];
    let mut mapper = HidMapper::new(settings.key_source, settings.screen_width, settings.screen_height)
        .with_mouse_mode(settings.mouse_mode);

    loop {
        status::set_deskflow(DeskflowState::WaitingForLink);
        stack.wait_link_up().await;
        status::set_deskflow(DeskflowState::Connecting);
        let mut socket = TcpSocket::new(stack, &mut rx_buf, &mut tx_buf);
        socket.set_timeout(Some(SERVER_SILENCE_TIMEOUT));
        socket.set_keep_alive(Some(Duration::from_secs(5)));

        match socket.connect((config::HOST_IP, config::DESKFLOW_PORT)).await {
            Ok(()) => {
                log::info!("deskflow: connected to {}:{}", config::HOST_IP, config::DESKFLOW_PORT);
                let result = match settings.tls {
                    None => session(&mut socket, settings, &mut mapper, &mut false).await,
                    Some(trust) => tls_session(&mut socket, settings, trust, &identity, &mut mapper).await,
                };
                log::warn!("deskflow: session ended: {:?}", result);
                if matches!(result, Err(Error::Tls(_) | Error::Rejected(_))) {
                    status::set_attention(true);
                }
            }
            Err(e) => log::debug!("deskflow: connect failed: {:?}", e),
        }
        status::set_deskflow(DeskflowState::Connecting);
        // Never leave keys or buttons held on the laptop across a reconnect.
        mapper.release_all(send_report_now);
        clipboard::set_keys_held(false);
        socket.abort();
        let _ = socket.flush().await;
        Timer::after(RETRY_DELAY).await;
    }
}

/// A Deskflow session over TLS. Core 1 does the TLS (see `tls_offload`);
/// this task shuttles ciphertext between it and the socket.
async fn tls_session(
    socket: &mut TcpSocket<'_>,
    settings: &Settings,
    trust: ServerTrust,
    identity: &Fingerprint,
    mapper: &mut HidMapper,
) -> Result<(), Error> {
    tls_offload::start(trust).await;
    let (mut socket_rx, mut socket_tx) = socket.split();
    let result = match select(
        tls_offload::pump(&mut socket_rx, &mut socket_tx),
        secure_session(settings, trust, identity, mapper),
    )
    .await
    {
        Either::First(()) => Err(Error::Transport),
        Either::Second(result) => result,
    };
    tls_offload::stop().await;
    result
}

async fn secure_session(
    settings: &Settings,
    trust: ServerTrust,
    identity: &Fingerprint,
    mapper: &mut HidMapper,
) -> Result<(), Error> {
    let Ok(handshake) = with_timeout(SERVER_SILENCE_TIMEOUT, tls_offload::handshake()).await else {
        return Err(Error::Timeout);
    };
    let server = handshake.server_fingerprint;
    if let Some(seen) = server {
        status::set_server_fingerprint(seen);
    }
    log::info!("deskflow: TLS handshake took {} ms", handshake.millis);
    status::set_tls_handshake_ms(handshake.millis as u32);
    if let Err(e) = handshake.result {
        match (handshake.rejection, server) {
            (Some(Rejection::FingerprintMismatch), Some(seen)) => {
                log::error!("deskflow: server fingerprint is {}", DisplayFingerprint(&seen));
                log::error!("deskflow: but a different one is pinned; update it if the server's certificate changed");
            }
            (Some(why), _) => log::error!("deskflow: server certificate not accepted: {:?}", why),
            (None, _) => log::error!("deskflow: TLS handshake failed: {:?} (is TLS on in the server settings?)", e),
        }
        return Err(Error::Tls(e));
    }
    if let Some(seen) = server {
        log::info!("deskflow: TLS up, server fingerprint {}", DisplayFingerprint(&seen));
        if trust == ServerTrust::Any {
            log::info!("deskflow: (not pinned; see the settings page)");
        }
    }

    let mut greeted = false;
    let result = session(&mut PlainLink, settings, mapper, &mut greeted).await;
    if !greeted && matches!(result, Err(Error::Transport)) {
        status::set_attention(true);
        // A server that checks client fingerprints completes the handshake,
        // then hangs up on clients it doesn't trust.
        log::error!(
            "deskflow: server closed the connection; if it checks client certificates, trust {}",
            DisplayFingerprint(identity)
        );
    }
    result
}

/// Runs the Deskflow protocol over `link`. Sets `greeted` once the server's
/// hello has arrived.
async fn session<L: Read + Write>(
    link: &mut L,
    settings: &Settings,
    mapper: &mut HidMapper,
    greeted: &mut bool,
) -> Result<(), Error> {
    let mut msg = [0u8; MAX_MESSAGE];
    let mut out = [0u8; 64];
    let mut clip = clip::Receiver::new();
    // The paste hotkey's key, swallowed until it's released.
    let mut hotkey: Option<u16> = None;

    loop {
        let Some(len) = read_frame(link, &mut msg, &mut clip).await? else { continue };
        let parsed = wire::parse(&msg[..len]).map_err(|_| Error::Protocol)?;
        if !*greeted && !matches!(parsed, Message::Hello { .. }) {
            log::warn!("deskflow: '{}' before the server's hello", core::str::from_utf8(&msg[..len.min(4)]).unwrap_or("????"));
        }

        match parsed {
            Message::Hello { protocol, major, minor } => {
                *greeted = true;
                status::set_deskflow(DeskflowState::Connected);
                status::note_deskflow_session();
                status::set_attention(false);
                log::info!("deskflow: server speaks protocol {}.{}", major, minor);
                let n = wire::encode_hello_back(&mut out, &protocol, &settings.screen_name).ok_or(Error::Protocol)?;
                send(link, &out[..n]).await?;
            }
            Message::QueryInfo => {
                let (w, h) = mapper.screen_size();
                let (x, y) = mapper.cursor();
                let n = wire::encode_info(&mut out, w as i16, h as i16, x, y).ok_or(Error::Protocol)?;
                send(link, &out[..n]).await?;
            }
            Message::KeepAlive => {
                let n = wire::encode_keep_alive(&mut out).ok_or(Error::Protocol)?;
                send(link, &out[..n]).await?;
            }
            Message::Enter { .. } => {
                log::info!("deskflow: cursor entered");
                status::set_deskflow(DeskflowState::Active);
                mapper.handle(&parsed, send_report_now);
            }
            Message::Leave => {
                log::info!("deskflow: cursor left");
                status::set_deskflow(DeskflowState::Connected);
                mapper.handle(&parsed, send_report_now);
                clipboard::set_keys_held(false);
            }
            Message::KeyDown { id, mask, button } if is_paste_hotkey(settings.key_source, id, mask, button) => {
                log::info!("deskflow: paste hotkey");
                hotkey = Some(button);
                clipboard::request_typing();
            }
            Message::KeyDown { id, mask, button }
                if let Some(slot) = laptop_hotkey(settings.key_source, id, mask, button) =>
            {
                log::info!("deskflow: switching to laptop {}", slot + 1);
                hotkey = Some(button);
                let _ = ble::COMMANDS.try_send(Command::Switch(slot));
            }
            Message::KeyRepeat { button, .. } if hotkey == Some(button) => {}
            Message::KeyUp { button, .. } if hotkey == Some(button) => hotkey = None,
            Message::KeyDown { .. } | Message::KeyRepeat { .. } | Message::KeyUp { .. } => {
                if let Message::KeyDown { id, mask, button } = parsed
                    && keymap::consumer_usage_for(id).is_none()
                    && keymap::usage_for(settings.key_source, id, button).is_none()
                {
                    log::info!("deskflow: no key for id {:#06x}, button {}, mask {:#06x}: dropped", id, button, mask);
                }
                if matches!(parsed, Message::KeyDown { .. }) && clipboard::is_typing() {
                    log::info!("clipboard: typing stopped by a key press");
                    clipboard::stop_typing();
                }
                mapper.handle(&parsed, send_report_queued);
                clipboard::set_keys_held(mapper.keys_held());
            }
            Message::Close => return Ok(()),
            Message::Incompatible { major, minor } => {
                log::error!("deskflow: server wants protocol {}.{}", major, minor);
                return Err(Error::Rejected("incompatible protocol"));
            }
            Message::Busy => return Err(Error::Rejected("a client with this screen name is already connected")),
            Message::UnknownClient => {
                log::error!("deskflow: add a screen named '{}' to the server layout", settings.screen_name);
                return Err(Error::Rejected("unknown screen name"));
            }
            Message::Bad => return Err(Error::Rejected("server reported a protocol error")),
            Message::Other { code, body } if &code == b"DCLP" => {
                if let Some(header) = clip::parse_chunk_header(body) {
                    on_clipboard(clip.begin(header));
                    clip.data(&body[CHUNK_HEADER_LEN..], clipboard::append);
                    on_clipboard(clip.end());
                }
            }
            Message::Other { code, .. } => {
                log::debug!("deskflow: ignoring {}", core::str::from_utf8(&code).unwrap_or("????"));
            }
            Message::MouseMove { .. } | Message::MouseRelMove { .. } => {
                let at = tls_offload::CIPHER_AT.load(Ordering::Relaxed);
                if at != 0 {
                    status::note_tls_path_us((Instant::now().as_micros() as u32).wrapping_sub(at));
                    status::note_backlog(tls_offload::backlog() as u32);
                }
                mapper.handle(&parsed, send_report_queued);
            }
            _ => mapper.handle(&parsed, send_report_queued),
        }
    }
}

fn on_clipboard(event: Event) {
    match event {
        Event::Started => clipboard::begin(),
        Event::Finished => clipboard::finish(),
        Event::None => {}
    }
}

/// Writes a whole message and pushes it out (TLS buffers until flushed).
async fn send<L: Write>(link: &mut L, bytes: &[u8]) -> Result<(), Error> {
    link.write_all(bytes).await.map_err(|_| Error::Transport)?;
    link.flush().await.map_err(|_| Error::Transport)
}

/// Reads one frame into `buf`. Returns `None` for a frame too large to keep,
/// which is consumed in pieces: clipboard chunks are streamed into `clip`,
/// anything else (file transfers) is dropped.
async fn read_frame<L: Read>(
    link: &mut L,
    buf: &mut [u8],
    clip: &mut clip::Receiver,
) -> Result<Option<usize>, Error> {
    let mut header = [0u8; FRAME_HEADER_LEN];
    read_exact(link, &mut header).await?;
    let len = wire::frame_len(header);
    if len <= buf.len() {
        read_exact(link, &mut buf[..len]).await?;
        return Ok(Some(len));
    }

    let started = Instant::now();
    let first = buf.len();
    read_exact(link, buf).await?;
    let clipboard = match buf.split_first_chunk::<4>() {
        Some((b"DCLP", body)) => clip::parse_chunk_header(body),
        _ => None,
    };
    if let Some(h) = clipboard {
        // Everything else from the server waits behind this, keep-alives too.
        log::info!("clipboard: receiving {} KB (clipboard {})", len / 1024, h.id);
        on_clipboard(clip.begin(h));
        clip.data(&buf[4 + CHUNK_HEADER_LEN..], clipboard::append);
    }
    let mut left = len - first;
    while left > 0 {
        let chunk = left.min(buf.len());
        read_exact(link, &mut buf[..chunk]).await?;
        if clipboard.is_some() {
            clip.data(&buf[..chunk], clipboard::append);
        }
        left -= chunk;
    }
    if clipboard.is_some() {
        on_clipboard(clip.end());
        log::info!("clipboard: that took {} ms", started.elapsed().as_millis());
    }
    Ok(None)
}

async fn read_exact<L: Read>(link: &mut L, buf: &mut [u8]) -> Result<(), Error> {
    match with_timeout(SERVER_SILENCE_TIMEOUT, link.read_exact(buf)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(Error::Transport),
        Err(_) => Err(Error::Timeout),
    }
}

/// For state changes that must reach the laptop (keys, buttons, releases):
/// the queue is drained by the BLE task even while disconnected, so this
/// only blocks briefly.
fn send_report_now(report: Report) {
    status::note_activity();
    if REPORTS.try_send(Outgoing::input(report)).is_err() {
        // Make room by discarding the oldest pending report. Reports are
        // full-state snapshots, so the newest one still leaves the laptop in
        // the right state; only a very short tap could be lost entirely.
        let _ = REPORTS.try_receive();
        let _ = REPORTS.try_send(Outgoing::input(report));
    }
}

/// For input as it comes. Pointer reports wait in [`MOTION`], merged with
/// the next one if possible, so moves can't pile up behind the radio; one
/// that can't merge (a click, the wheel) pushes the waiting report out.
fn send_report_queued(report: Report) {
    match report {
        Report::Mouse(_) | Report::RelMouse(_) => {
            status::note_activity();
            let displaced = MOTION.lock(|slot| {
                let (next, displaced) = match slot.get() {
                    Some(old) => match old.report.merged(report) {
                        Some(merged) => (Outgoing { report: merged, since: old.since }, None),
                        None => (Outgoing::input(report), Some(old)),
                    },
                    None => (Outgoing::input(report), None),
                };
                slot.set(Some(next));
                displaced
            });
            if let Some(old) = displaced {
                send_report_now(old.report);
            }
            MOTION_READY.signal(());
        }
        Report::Keyboard(_) | Report::Consumer(_) => send_report_now(report),
    }
}
