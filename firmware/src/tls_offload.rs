//! TLS on the second core.
//!
//! A handshake is seconds of software elliptic-curve and RSA maths on these
//! chips, and while it runs the executor it runs on can't poll anything
//! else, so on core 0 it stalled USB, Bluetooth and the web page. Core 1
//! therefore runs the whole TLS connection (handshake and record crypto),
//! and also builds the client identity. Core 0 keeps the network stack:
//!
//! ```text
//! TCP socket ⇄ [core 0 pump] ⇄ CIPHER_IN/OUT ⇄ [core 1: embedded-tls] ⇄ PLAIN_IN/OUT ⇄ [core 0: Deskflow session]
//! ```
//!
//! Every session follows the same protocol: core 0 calls [`start`], awaits
//! [`handshake`], uses [`PlainLink`], and always finishes with [`stop`],
//! which waits for core 1 to drop the connection before the pipes are
//! reused. Core 1 may end a session early (TLS error, server closed); core 0
//! notices through `PlainLink` returning errors.

use deskflow_proto::tls::{ClientIdentity, Fingerprint, Provider, Rejection, ServerTrust};
use embassy_futures::select::{Either, Either3, select, select3};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex as Cs;
use embassy_sync::channel::Channel;
use embassy_sync::pipe::Pipe;
use embassy_sync::signal::Signal;
use embassy_time::Instant;
use embedded_io_async::{ErrorKind, ErrorType, Read, Write};
use embedded_tls::{Aes128GcmSha256, TlsConfig, TlsConnection, TlsContext, TlsError};
use portable_atomic::{AtomicBool, AtomicU32, Ordering};

use crate::csprng;

/// A full TLS record (16 KiB plaintext) plus header and AEAD overhead: the
/// server doesn't limit record sizes, so this is the most it can send.
const TLS_READ_BUF: usize = 16384 + 256;
/// Our records are small; the largest is the client certificate.
const TLS_WRITE_BUF: usize = 2048;
/// Common name in the client certificate. Fixed, so renaming the screen
/// doesn't change the fingerprint the server trusts.
const CERT_NAME: &str = "pico-deskflow-bridge";

static CIPHER_IN: Pipe<Cs, 2048> = Pipe::new();
static CIPHER_OUT: Pipe<Cs, 2048> = Pipe::new();
static PLAIN_IN: Pipe<Cs, 2048> = Pipe::new();
static PLAIN_OUT: Pipe<Cs, 512> = Pipe::new();

enum Command {
    Identity([u8; 32]),
    Session(ServerTrust),
}

enum Reply {
    Identity(Option<Fingerprint>),
    Handshake(Handshake),
    Stopped,
}

/// When ciphertext last came off the socket (low bits of microseconds, 0 =
/// never): the start of the path the Deskflow session can time itself against.
pub static CIPHER_AT: AtomicU32 = AtomicU32::new(0);

static COMMANDS: Channel<Cs, Command, 1> = Channel::new();
static REPLIES: Channel<Cs, Reply, 1> = Channel::new();
/// Core 0 → core 1: end the session. Reset by [`start`], consumed by core 1.
static STOP: Signal<Cs, ()> = Signal::new();
/// Core 1 → core 0: the session is over on core 1's side.
static ENDED: AtomicBool = AtomicBool::new(false);
static ENDED_WAKE: Signal<Cs, ()> = Signal::new();

/// The outcome of a handshake, from core 1.
pub struct Handshake {
    pub result: Result<(), TlsError>,
    pub server_fingerprint: Option<Fingerprint>,
    pub rejection: Option<Rejection>,
    pub millis: u64,
}

// --- core 0 side ---

/// Builds the client identity from `secret` on core 1, which keeps it for
/// later sessions. `None` if `secret` isn't a valid key.
pub async fn load_identity(secret: [u8; 32]) -> Option<Fingerprint> {
    COMMANDS.send(Command::Identity(secret)).await;
    match REPLIES.receive().await {
        Reply::Identity(fp) => fp,
        _ => unreachable!("core 1 answers commands in order"),
    }
}

/// Starts a TLS session on core 1. Ciphertext is then exchanged through
/// [`pump`].
pub async fn start(trust: ServerTrust) {
    for pipe in [&CIPHER_IN, &CIPHER_OUT, &PLAIN_IN] {
        pipe.clear();
    }
    PLAIN_OUT.clear();
    STOP.reset();
    ENDED.store(false, Ordering::Relaxed);
    ENDED_WAKE.reset();
    COMMANDS.send(Command::Session(trust)).await;
}

/// Waits for the handshake's outcome.
pub async fn handshake() -> Handshake {
    match REPLIES.receive().await {
        Reply::Handshake(h) => h,
        _ => unreachable!("core 1 answers commands in order"),
    }
}

/// Ends the session and waits until core 1 has let go of it.
pub async fn stop() {
    STOP.signal(());
    loop {
        match REPLIES.receive().await {
            Reply::Stopped => return,
            // A handshake outcome nobody waited for (stopped mid-handshake).
            Reply::Handshake(_) => continue,
            Reply::Identity(_) => unreachable!("no identity request is pending"),
        }
    }
}

/// Bytes waiting between the socket and the Deskflow session.
pub fn backlog() -> usize {
    CIPHER_IN.len() + PLAIN_IN.len()
}

/// Moves ciphertext between the socket and core 1 until either side closes.
pub async fn pump<R: Read, W: Write>(socket_rx: &mut R, socket_tx: &mut W) {
    let inbound = async {
        let mut buf = [0u8; 512];
        loop {
            match socket_rx.read(&mut buf).await {
                Ok(0) | Err(_) => return,
                Ok(n) => {
                    CIPHER_AT.store((Instant::now().as_micros() as u32).max(1), Ordering::Relaxed);
                    CIPHER_IN.write_all(&buf[..n]).await
                }
            }
        }
    };
    let outbound = async {
        let mut buf = [0u8; 512];
        loop {
            let n = CIPHER_OUT.read(&mut buf).await;
            if socket_tx.write_all(&buf[..n]).await.is_err() || socket_tx.flush().await.is_err() {
                return;
            }
        }
    };
    select(inbound, outbound).await;
}

/// The decrypted stream, for the Deskflow session on core 0. Errors once
/// core 1 has ended the session and everything it decrypted has been read.
pub struct PlainLink;

const ENDED_ERROR: ErrorKind = ErrorKind::ConnectionReset;

impl ErrorType for PlainLink {
    type Error = ErrorKind;
}

impl Read for PlainLink {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, ErrorKind> {
        loop {
            if let Ok(n) = PLAIN_IN.try_read(buf) {
                return Ok(n);
            }
            if ENDED.load(Ordering::Relaxed) {
                return Err(ENDED_ERROR);
            }
            if let Either::First(n) = select(PLAIN_IN.read(buf), ENDED_WAKE.wait()).await {
                return Ok(n);
            }
        }
    }
}

impl Write for PlainLink {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, ErrorKind> {
        if ENDED.load(Ordering::Relaxed) {
            return Err(ENDED_ERROR);
        }
        match select(PLAIN_OUT.write(buf), ENDED_WAKE.wait()).await {
            Either::First(n) => Ok(n),
            Either::Second(()) => Err(ENDED_ERROR),
        }
    }

    /// Core 1 flushes a TLS record for every chunk it takes from the pipe.
    async fn flush(&mut self) -> Result<(), ErrorKind> {
        Ok(())
    }
}

// --- core 1 side ---

/// embedded-tls's transport on core 1: the ciphertext pipes.
#[derive(Clone)]
struct CipherLink;

impl ErrorType for CipherLink {
    type Error = core::convert::Infallible;
}

impl Read for CipherLink {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        Ok(CIPHER_IN.read(buf).await)
    }
}

impl Write for CipherLink {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        Ok(CIPHER_OUT.write(buf).await)
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

fn end() {
    ENDED.store(true, Ordering::Relaxed);
    ENDED_WAKE.signal(());
}

/// Core 1's only task: serves identity and session commands from core 0.
#[embassy_executor::task]
pub async fn core1_task() -> ! {
    let mut identity: Option<ClientIdentity> = None;
    let mut read_buf = [0u8; TLS_READ_BUF];
    let mut write_buf = [0u8; TLS_WRITE_BUF];
    loop {
        match COMMANDS.receive().await {
            Command::Identity(secret) => {
                identity = ClientIdentity::new(&secret, CERT_NAME);
                REPLIES.send(Reply::Identity(identity.as_ref().map(ClientIdentity::fingerprint))).await;
            }
            Command::Session(trust) => {
                session(trust, identity.as_ref(), &mut read_buf, &mut write_buf).await;
                end();
                REPLIES.send(Reply::Stopped).await;
            }
        }
    }
}

/// Runs one TLS connection until core 0 stops it. Returns after `STOP`.
async fn session(trust: ServerTrust, identity: Option<&ClientIdentity>, read_buf: &mut [u8], write_buf: &mut [u8]) {
    let mut tls: TlsConnection<'_, CipherLink, Aes128GcmSha256> = TlsConnection::new(CipherLink, read_buf, write_buf);
    // Deskflow's certificate is RSA; embedded-tls only offers RSA when asked.
    let config = TlsConfig::new().enable_rsa_signatures();
    let mut provider = Provider::new(csprng::Rng, trust, identity);

    let started = Instant::now();
    let opened = match select(tls.open(TlsContext::new(&config, &mut provider)), STOP.wait()).await {
        Either::First(opened) => opened,
        Either::Second(()) => return,
    };
    let failed = opened.is_err();
    REPLIES
        .send(Reply::Handshake(Handshake {
            result: opened,
            server_fingerprint: provider.server_fingerprint(),
            rejection: provider.rejection(),
            millis: started.elapsed().as_millis(),
        }))
        .await;
    if failed {
        end();
        STOP.wait().await;
        return;
    }

    let (mut reader, mut writer) = tls.split();
    let decrypt = async {
        let mut buf = [0u8; 512];
        loop {
            match reader.read(&mut buf).await {
                Ok(0) | Err(_) => return,
                Ok(n) => PLAIN_IN.write_all(&buf[..n]).await,
            }
        }
    };
    let encrypt = async {
        let mut buf = [0u8; 512];
        loop {
            let n = PLAIN_OUT.read(&mut buf).await;
            if writer.write_all(&buf[..n]).await.is_err() || writer.flush().await.is_err() {
                return;
            }
        }
    };
    match select3(decrypt, encrypt, STOP.wait()).await {
        Either3::Third(()) => {}
        Either3::First(()) | Either3::Second(()) => {
            end();
            STOP.wait().await;
        }
    }
}
