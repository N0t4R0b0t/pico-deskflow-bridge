//! Host test of firmware/src/tls_offload.rs: "core 1" is a thread running
//! core1_task under embassy's block_on; "core 0" is tokio, pumping a real
//! TCP connection to `openssl s_server` (set up like a Deskflow server).
//! Covers sessions, stop/restart, a rejected pin, stopping mid-handshake and
//! the server vanishing mid-session. `cargo run --release`; exits non-zero
//! on failure.

mod shim {
    pub struct Instant(std::time::Instant);
    pub struct Elapsed(std::time::Duration);
    impl Instant {
        pub fn as_micros(&self) -> u64 {
            self.0.elapsed().as_micros() as u64
        }

        pub fn now() -> Self {
            Instant(std::time::Instant::now())
        }
        pub fn elapsed(&self) -> Elapsed {
            Elapsed(self.0.elapsed())
        }
    }
    impl Elapsed {
        pub fn as_millis(&self) -> u64 {
            self.0.as_millis() as u64
        }
    }
}
mod csprng {
    pub use rand_core::OsRng as Rng;
}
mod tls_offload {
    include!(concat!(env!("OUT_DIR"), "/tls_offload.rs"));
}

use std::io::BufRead;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use deskflow_proto::tls::{Fingerprint, Rejection, ServerTrust, fingerprint, generate_secret};
use embassy_futures::select::{Either, select};
use embedded_io_adapters::tokio_1::FromTokio;
use embedded_io_async::{Read, Write};
use tls_offload::PlainLink;

struct Server {
    child: Child,
    port: u16,
    fp: Fingerprint,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn server(dir: &std::path::Path) -> Server {
    let ok = |args: &[&str]| assert!(Command::new("openssl").args(args).current_dir(dir).stderr(Stdio::null()).stdout(Stdio::null()).status().unwrap().success());
    ok(&["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-subj", "/CN=Deskflow", "-days", "1", "-keyout", "key.pem", "-out", "cert.pem"]);
    ok(&["x509", "-in", "cert.pem", "-outform", "DER", "-out", "cert.der"]);
    let fp = fingerprint(&std::fs::read(dir.join("cert.der")).unwrap());
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut child = Command::new("openssl")
        .args(["s_server", "-accept", &port.to_string(), "-cert", "cert.pem", "-key", "key.pem", "-Verify", "1", "-rev"])
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut out = std::io::BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    while !line.contains("ACCEPT") {
        line.clear();
        assert!(out.read_line(&mut line).unwrap() > 0);
    }
    std::thread::spawn(move || std::io::copy(&mut out, &mut std::io::sink()));
    Server { child, port, fp }
}

/// What the firmware's tls_session does, with an echo instead of Deskflow.
async fn session(port: u16, trust: ServerTrust, kill: Option<&mut Server>) -> Result<String, String> {
    let stream = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let (r, w) = stream.into_split();
    let (mut rx, mut tx) = (FromTokio::new(r), FromTokio::new(w));
    tls_offload::start(trust).await;
    let work = async {
        let h = tls_offload::handshake().await;
        if let Err(e) = h.result {
            return Err(format!("handshake: {:?} {:?}", e, h.rejection));
        }
        println!("     handshake {} ms, server {:?}", h.millis, h.server_fingerprint.map(|f| f[0]));
        if let Some(server) = kill {
            // The server goes away mid-session.
            let _ = server.child.kill();
        }
        let mut link = PlainLink;
        link.write_all(b"deskflow\n").await.map_err(|e| format!("write: {:?}", e))?;
        link.flush().await.unwrap();
        let mut reply = Vec::new();
        let mut buf = [0u8; 64];
        while !reply.ends_with(b"\n") {
            let n = link.read(&mut buf).await.map_err(|e| format!("read: {:?}", e))?;
            reply.extend_from_slice(&buf[..n]);
        }
        Ok(String::from_utf8(reply).unwrap())
    };
    let result = match select(tls_offload::pump(&mut rx, &mut tx), work).await {
        Either::First(()) => Err("socket closed".into()),
        Either::Second(r) => r,
    };
    tls_offload::stop().await;
    result
}

macro_rules! check {
    ($cond:expr, $what:expr) => {{
        let ok = $cond;
        println!("{} {}", if ok { "ok  " } else { "FAIL" }, $what);
        if !ok {
            std::process::exit(1);
        }
    }};
}

#[tokio::main]
async fn main() {
    std::thread::spawn(|| embassy_futures::block_on(tls_offload::core1_task()));
    let dir = std::env::temp_dir().join(format!("offload-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    check!(t(tls_offload::load_identity([0; 32])).await.unwrap().is_none(), "invalid secret rejected on core 1");
    let fp = t(tls_offload::load_identity(generate_secret(&mut rand_core::OsRng))).await.unwrap();
    check!(fp.is_some(), "identity built on core 1");

    let mut srv = server(&dir);
    let r = t(session(srv.port, ServerTrust::Any, None)).await.unwrap();
    check!(r.as_deref() == Ok("wolfksed\n"), format!("session 1 (any): {:?}", r));

    let r = t(session(srv.port, ServerTrust::Pinned(srv.fp), None)).await.unwrap();
    check!(r.as_deref() == Ok("wolfksed\n"), format!("session 2 (pinned, after stop/restart): {:?}", r));

    let mut wrong = srv.fp;
    wrong[0] ^= 1;
    let r = t(session(srv.port, ServerTrust::Pinned(wrong), None)).await.unwrap();
    check!(
        matches!(&r, Err(e) if e.contains(&format!("{:?}", Some(Rejection::FingerprintMismatch)))),
        format!("wrong pin rejected: {:?}", r)
    );

    // Stop while core 1 is mid-handshake (nothing pumped, so it's waiting).
    tls_offload::start(ServerTrust::Any).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    check!(t(tls_offload::stop()).await.is_ok(), "stop during handshake");

    let r = t(session(srv.port, ServerTrust::Any, None)).await.unwrap();
    check!(r.as_deref() == Ok("wolfksed\n"), format!("session after aborted handshake: {:?}", r));

    // Server dies after the handshake: the session errors out and stops cleanly.
    let r = t(session(srv.port, ServerTrust::Any, Some(&mut srv))).await;
    check!(matches!(r, Ok(Err(_))), format!("server gone mid-session ends the session: {:?}", r));

    let dir2 = dir.join("second").tap_mkdir();
    let srv2 = server(&dir2);
    let r = t(session(srv2.port, ServerTrust::Pinned(srv2.fp), None)).await.unwrap();
    check!(r.as_deref() == Ok("wolfksed\n"), format!("new server afterwards: {:?}", r));

    let _ = std::fs::remove_dir_all(&dir);
    println!("all passed");
    std::process::exit(0);
}

fn t<F: std::future::Future>(f: F) -> tokio::time::Timeout<F> {
    tokio::time::timeout(Duration::from_secs(30), f)
}

trait TapMkdir {
    fn tap_mkdir(self) -> Self;
}

impl TapMkdir for std::path::PathBuf {
    fn tap_mkdir(self) -> Self {
        std::fs::create_dir_all(&self).unwrap();
        self
    }
}
