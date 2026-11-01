//! Handshakes against `openssl s_server` set up the way a Deskflow server
//! is: self-signed RSA-2048 certificate, TLS 1.2+, client certificate
//! required but not chain-validated. Skipped when `openssl` isn't installed.

use std::io::{BufRead, BufReader};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use deskflow_proto::tls::{ClientIdentity, Fingerprint, Provider, Rejection, ServerTrust, fingerprint, generate_secret};
use embedded_io_adapters::std::FromStd;
use embedded_tls::blocking::{TlsConfig, TlsConnection, TlsContext};
use embedded_tls::{Aes128GcmSha256, TlsError};
use rand_core::OsRng;

struct Server {
    child: Child,
    port: u16,
    fingerprint: Fingerprint,
    _dir: TempDir,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn openssl(args: &[&str], dir: &Path) -> bool {
    Command::new("openssl")
        .args(args)
        .current_dir(dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn have_openssl() -> bool {
    let found = Command::new("openssl").arg("version").output().is_ok();
    if !found {
        eprintln!("openssl not installed; skipping");
    }
    found
}

/// A fresh directory per call: the tests run in parallel.
fn temp_dir() -> TempDir {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("deskflow-proto-tls-{}-{}", std::process::id(), n));
    std::fs::create_dir_all(&dir).unwrap();
    TempDir(dir)
}

/// Starts an echo-reversing server; `None` without openssl.
fn server(key: &str) -> Option<Server> {
    if !have_openssl() {
        return None;
    }
    let dir = temp_dir();
    let subject = "/CN=Deskflow";
    assert!(openssl(&["req", "-x509", "-newkey", key, "-nodes", "-subj", subject, "-days", "365", "-keyout", "key.pem", "-out", "cert.pem"], &dir.0));
    assert!(openssl(&["x509", "-in", "cert.pem", "-outform", "DER", "-out", "cert.der"], &dir.0));
    let der = std::fs::read(dir.0.join("cert.der")).unwrap();

    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let child = Command::new("openssl")
        .args(["s_server", "-accept", &port.to_string(), "-cert", "cert.pem", "-key", "key.pem", "-Verify", "1", "-rev"])
        .current_dir(&dir.0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut server = Server { child, port, fingerprint: fingerprint(&der), _dir: dir };

    // s_server prints "ACCEPT" once it's listening.
    let mut out = BufReader::new(server.child.stdout.take().unwrap());
    let mut line = String::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !line.contains("ACCEPT") {
        line.clear();
        assert!(out.read_line(&mut line).unwrap() > 0 && Instant::now() < deadline, "s_server didn't start");
    }
    std::thread::spawn(move || std::io::copy(&mut out, &mut std::io::sink()));
    Some(server)
}

fn handshake(
    server: &Server,
    trust: ServerTrust,
    client: Option<&ClientIdentity>,
) -> (Result<String, TlsError>, Option<Fingerprint>, Option<Rejection>) {
    let socket = TcpStream::connect(("127.0.0.1", server.port)).unwrap();
    socket.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut rx = [0u8; 16640];
    let mut tx = [0u8; 4096];
    let mut tls: TlsConnection<_, Aes128GcmSha256> = TlsConnection::new(FromStd::new(socket), &mut rx, &mut tx);
    let config = TlsConfig::new().enable_rsa_signatures();
    let mut provider = Provider::new(OsRng, trust, client);

    let result = tls.open(TlsContext::new(&config, &mut provider)).and_then(|()| {
        tls.write(b"deskflow\n")?;
        tls.flush()?;
        let mut reply = Vec::new();
        let mut buf = [0u8; 64];
        while !reply.ends_with(b"\n") {
            let n = tls.read(&mut buf)?;
            if n == 0 {
                return Err(TlsError::ConnectionClosed);
            }
            reply.extend_from_slice(&buf[..n]);
        }
        Ok(String::from_utf8(reply).unwrap())
    });
    (result, provider.server_fingerprint(), provider.rejection())
}

fn client() -> ClientIdentity {
    ClientIdentity::new(&generate_secret(&mut OsRng), "arch-laptop").unwrap()
}

#[test]
fn pinned_rsa_server_with_client_certificate() {
    let Some(server) = server("rsa:2048") else { return };
    let client = client();
    let (result, seen, rejection) = handshake(&server, ServerTrust::Pinned(server.fingerprint), Some(&client));
    assert_eq!(result.unwrap(), "wolfksed\n");
    assert_eq!(seen, Some(server.fingerprint));
    assert_eq!(rejection, None);
}

#[test]
fn larger_rsa_keys() {
    let Some(server) = server("rsa:4096") else { return };
    let client = client();
    let (result, _, _) = handshake(&server, ServerTrust::Pinned(server.fingerprint), Some(&client));
    assert_eq!(result.unwrap(), "wolfksed\n");
}

#[test]
fn any_server_reports_its_fingerprint() {
    let Some(server) = server("rsa:2048") else { return };
    let client = client();
    let (result, seen, _) = handshake(&server, ServerTrust::Any, Some(&client));
    assert!(result.is_ok());
    assert_eq!(seen, Some(server.fingerprint));
}

#[test]
fn wrong_fingerprint_is_rejected() {
    let Some(server) = server("rsa:2048") else { return };
    let client = client();
    let mut wrong = server.fingerprint;
    wrong[0] ^= 1;
    let (result, seen, rejection) = handshake(&server, ServerTrust::Pinned(wrong), Some(&client));
    assert!(matches!(result, Err(TlsError::InvalidCertificate)));
    assert_eq!(seen, Some(server.fingerprint));
    assert_eq!(rejection, Some(Rejection::FingerprintMismatch));
}

#[test]
fn server_requiring_a_client_certificate_rejects_none() {
    let Some(server) = server("rsa:2048") else { return };
    let (result, _, _) = handshake(&server, ServerTrust::Pinned(server.fingerprint), None);
    assert!(result.is_err());
}

#[test]
fn openssl_parses_client_certificate() {
    if !have_openssl() {
        return;
    }
    let client = client();
    let dir = temp_dir();
    std::fs::write(dir.0.join("client.der"), client.certificate()).unwrap();
    let out = Command::new("openssl")
        .args(["x509", "-inform", "DER", "-in", "client.der", "-noout", "-subject", "-fingerprint", "-sha256"])
        .current_dir(&dir.0)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("CN = arch-laptop"), "{}", text);
    let expected = deskflow_proto::tls::DisplayFingerprint(&client.fingerprint()).to_string();
    assert!(text.contains(&expected), "{} vs {}", text, expected);

    // Self-signature verifies.
    assert!(openssl(&["x509", "-inform", "DER", "-in", "client.der", "-out", "client.pem"], &dir.0));
    let out = Command::new("openssl")
        .args(["verify", "-CAfile", "client.pem", "-no_check_time", "client.pem"])
        .current_dir(&dir.0)
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("OK"), "{}", String::from_utf8_lossy(&out.stderr));
}

