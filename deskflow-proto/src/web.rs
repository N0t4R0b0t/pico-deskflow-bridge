//! The bits of HTTP/1.1 the configuration page needs: request heads, form
//! bodies and JSON strings. The page itself is static ([`PAGE`]) and talks
//! to a small JSON API.
//!
//! The page is reachable from any process on the host, including web pages
//! open in its browser, so requests are only served when `Host` is the
//! device's own IP (against DNS rebinding) and changes need a custom header
//! (which a cross-site form can't send and a cross-site `fetch` can't send
//! without a CORS preflight the device never answers).

use core::fmt;

use crate::tls::{DbLine, DisplayFingerprint, Fingerprint};

/// The configuration page.
pub const PAGE: &str = include_str!("web/index.html");

/// Header that state-changing requests must carry.
pub const CSRF_HEADER: &str = "x-bridge";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request<'a> {
    pub method: Method,
    pub path: &'a str,
    /// After the `?`, empty if none.
    pub query: &'a str,
    pub host: Option<&'a str>,
    pub content_length: usize,
    /// Whether [`CSRF_HEADER`] was present.
    pub csrf_header: bool,
    pub cookie: Option<&'a str>,
    /// Bytes up to and including the blank line; the body follows.
    pub head_len: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BadRequest;

/// Parses a request head. `Ok(None)` if `buf` doesn't hold all of it yet.
pub fn parse_head(buf: &[u8]) -> Result<Option<Request<'_>>, BadRequest> {
    let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
        return Ok(None);
    };
    let head = core::str::from_utf8(&buf[..end]).map_err(|_| BadRequest)?;
    let mut lines = head.split("\r\n");

    let mut parts = lines.next().ok_or(BadRequest)?.split(' ');
    let method = match parts.next().ok_or(BadRequest)? {
        "GET" | "HEAD" => Method::Get,
        "POST" => Method::Post,
        _ => Method::Other,
    };
    let target = parts.next().ok_or(BadRequest)?;
    if !parts.next().is_some_and(|v| v.starts_with("HTTP/1.")) {
        return Err(BadRequest);
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));

    let mut req = Request { method, path, query, host: None, content_length: 0, csrf_header: false, cookie: None, head_len: end + 4 };
    for line in lines {
        let (name, value) = line.split_once(':').ok_or(BadRequest)?;
        let value = value.trim();
        if name.eq_ignore_ascii_case("host") {
            req.host = Some(value);
        } else if name.eq_ignore_ascii_case("content-length") {
            req.content_length = value.parse().map_err(|_| BadRequest)?;
        } else if name.eq_ignore_ascii_case(CSRF_HEADER) {
            req.csrf_header = true;
        } else if name.eq_ignore_ascii_case("cookie") {
            req.cookie = Some(value);
        }
    }
    Ok(Some(req))
}

/// Whether a `Host` header names `ip` (optionally with port 80).
pub fn host_is(host: Option<&str>, ip: [u8; 4]) -> bool {
    let Some(host) = host else { return false };
    let host = host.strip_suffix(":80").unwrap_or(host);
    let mut octets = host.split('.');
    ip.iter().all(|&o| octets.next().and_then(|s| s.parse::<u8>().ok()) == Some(o)) && octets.next().is_none()
}

/// The value of `key` in a form-encoded `body` (or query string), decoded
/// into `out`. `None` if the key is absent; `Some(None)` if the value is not
/// valid percent-encoded UTF-8 or doesn't fit.
pub fn form_value<'o>(body: &[u8], key: &str, out: &'o mut [u8]) -> Option<Option<&'o str>> {
    let raw = body.split(|&b| b == b'&').find_map(|pair| {
        let (k, v) = match pair.iter().position(|&b| b == b'=') {
            Some(i) => (&pair[..i], &pair[i + 1..]),
            None => (pair, &[][..]),
        };
        (k == key.as_bytes()).then_some(v)
    })?;
    Some(percent_decode(raw, out))
}

fn percent_decode<'o>(raw: &[u8], out: &'o mut [u8]) -> Option<&'o str> {
    fn hex(b: u8) -> Option<u8> {
        (b as char).to_digit(16).map(|d| d as u8)
    }
    let mut n = 0;
    let mut i = 0;
    while i < raw.len() {
        let byte = match raw[i] {
            b'+' => b' ',
            b'%' => {
                let hi = hex(*raw.get(i + 1)?)?;
                let lo = hex(*raw.get(i + 2)?)?;
                i += 2;
                hi << 4 | lo
            }
            b => b,
        };
        *out.get_mut(n)? = byte;
        n += 1;
        i += 1;
    }
    core::str::from_utf8(&out[..n]).ok()
}

/// A query parameter parsed as a number.
pub fn query_u32(query: &str, key: &str) -> Option<u32> {
    let mut buf = [0u8; 16];
    form_value(query.as_bytes(), key, &mut buf)??.parse().ok()
}

/// Writes a form-encoded (`application/x-www-form-urlencoded`) value.
pub struct FormStr<'a>(pub &'a str);

impl fmt::Display for FormStr<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for &b in self.0.as_bytes() {
            match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => write!(f, "{}", b as char)?,
                b' ' => f.write_str("+")?,
                b => write!(f, "%{:02X}", b)?,
            }
        }
        Ok(())
    }
}

/// Writes a quoted, escaped JSON string.
pub struct JsonStr<'a>(pub &'a str);

impl fmt::Display for JsonStr<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("\"")?;
        for c in self.0.chars() {
            match c {
                '"' => f.write_str("\\\"")?,
                '\\' => f.write_str("\\\\")?,
                // Keeps the JSON safe to embed in HTML, too.
                '<' => f.write_str("\\u003c")?,
                c if (c as u32) < 0x20 => write!(f, "\\u{:04x}", c as u32)?,
                c => write!(f, "{}", c)?,
            }
        }
        f.write_str("\"")
    }
}

/// Writes a response head for a body of `len` bytes. Every response closes
/// the connection: the device serves one request per socket.
pub fn write_head(w: &mut impl fmt::Write, status: u16, content_type: &str, len: usize, extra: &str) -> fmt::Result {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Content Too Large",
        503 => "Service Unavailable",
        _ => "Internal Server Error",
    };
    write!(
        w,
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n{}\r\n",
        status, reason, content_type, len, extra
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum DeskflowState {
    /// No USB network yet.
    WaitingForLink = 0,
    /// Dialing, or in the TLS handshake.
    Connecting = 1,
    /// Greeted by the server.
    Connected = 2,
    /// Connected, and the cursor is on this screen.
    Active = 3,
}

/// Live device state for the page.
pub struct Status {
    pub deskflow: DeskflowState,
    pub ble_connected: bool,
    pub ble_bonded: bool,
    pub server_fingerprint: Option<Fingerprint>,
    /// `None` while the device is still creating it.
    pub client_fingerprint: Option<Fingerprint>,
    pub uptime_s: u64,
    /// Characters of clipboard text held for typing.
    pub clipboard_chars: usize,
    pub clipboard_truncated: bool,
    /// Bytes of text the device can hold.
    pub clipboard_max: usize,
    /// The build that's running.
    pub firmware: &'static str,
    /// Whether the page can update the firmware.
    pub updates: bool,
    /// Characters typed so far, while typing.
    pub typing: Option<usize>,
    /// How long the last TLS handshake took.
    pub tls_handshake_ms: Option<u32>,
}

impl Status {
    pub fn write_json(&self, w: &mut impl fmt::Write) -> fmt::Result {
        let deskflow = match self.deskflow {
            DeskflowState::WaitingForLink => "waiting",
            DeskflowState::Connecting => "connecting",
            DeskflowState::Connected => "connected",
            DeskflowState::Active => "active",
        };
        write!(
            w,
            r#"{{"deskflow":"{}","ble_connected":{},"ble_bonded":{},"uptime_s":{},"clipboard_chars":{},"clipboard_truncated":{},"clipboard_max":{},"firmware":{},"updates":{},"typing":"#,
            deskflow,
            self.ble_connected,
            self.ble_bonded,
            self.uptime_s,
            self.clipboard_chars,
            self.clipboard_truncated,
            self.clipboard_max,
            JsonStr(self.firmware),
            self.updates,
        )?;
        match self.typing {
            Some(n) => write!(w, "{}", n)?,
            None => w.write_str("null")?,
        }
        w.write_str(r#","tls_handshake_ms":"#)?;
        match self.tls_handshake_ms {
            Some(ms) => write!(w, "{}", ms)?,
            None => w.write_str("null")?,
        }
        w.write_str(r#","client_fingerprint":"#)?;
        match &self.client_fingerprint {
            Some(fp) => write!(w, r#""{}","client_db_line":"{}""#, DisplayFingerprint(fp), DbLine(fp))?,
            None => w.write_str(r#"null,"client_db_line":null"#)?,
        }
        w.write_str(r#","server_fingerprint":"#)?;
        match &self.server_fingerprint {
            Some(fp) => write!(w, r#""{}"}}"#, DisplayFingerprint(fp)),
            None => w.write_str("null}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_post() {
        let raw = b"POST /api/settings?x=1 HTTP/1.1\r\nHost: 192.168.77.1\r\ncontent-length: 12\r\nX-Bridge: 1\r\nCookie: a=b; bridge_session=00\r\n\r\nscreen_name=";
        let req = parse_head(raw).unwrap().unwrap();
        assert_eq!(req.method, Method::Post);
        assert_eq!((req.path, req.query), ("/api/settings", "x=1"));
        assert_eq!(req.host, Some("192.168.77.1"));
        assert_eq!(req.content_length, 12);
        assert!(req.csrf_header);
        assert_eq!(req.cookie, Some("a=b; bridge_session=00"));
        assert_eq!(&raw[req.head_len..], b"screen_name=");
    }

    #[test]
    fn incomplete_and_malformed_heads() {
        assert_eq!(parse_head(b"GET / HTTP/1.1\r\nHost: x\r\n"), Ok(None));
        assert_eq!(parse_head(b"GET /\r\n\r\n"), Err(BadRequest));
        assert_eq!(parse_head(b"GET / HTTP/1.1\r\nno-colon\r\n\r\n"), Err(BadRequest));
        assert_eq!(parse_head(b"POST / HTTP/1.1\r\nContent-Length: lots\r\n\r\n"), Err(BadRequest));
        let req = parse_head(b"GET / HTTP/1.1\r\n\r\n").unwrap().unwrap();
        assert!(!req.csrf_header && req.host.is_none() && req.cookie.is_none());
    }

    #[test]
    fn host_check() {
        let ip = [192, 168, 77, 1];
        assert!(host_is(Some("192.168.77.1"), ip));
        assert!(host_is(Some("192.168.77.1:80"), ip));
        assert!(!host_is(Some("192.168.77.1:8080"), ip));
        assert!(!host_is(Some("192.168.77.10"), ip));
        assert!(!host_is(Some("192.168.77.1.evil.example"), ip));
        assert!(!host_is(Some("evil.example"), ip));
        assert!(!host_is(None, ip));
    }

    #[test]
    fn form_decoding() {
        let mut buf = [0u8; 32];
        let body = b"a=1&name=My+Bridge%21&empty=&flag";
        assert_eq!(form_value(body, "name", &mut buf), Some(Some("My Bridge!")));
        assert_eq!(form_value(body, "empty", &mut buf), Some(Some("")));
        assert_eq!(form_value(body, "flag", &mut buf), Some(Some("")));
        assert_eq!(form_value(body, "missing", &mut buf), None);
        assert_eq!(form_value(b"x=%zz", "x", &mut buf), Some(None));
        assert_eq!(form_value(b"x=%4", "x", &mut buf), Some(None));
        assert_eq!(form_value(b"x=%FF", "x", &mut buf), Some(None));
        assert_eq!(form_value(b"x=caf%C3%A9", "x", &mut buf), Some(Some("café")));
        let mut tiny = [0u8; 2];
        assert_eq!(form_value(b"x=abc", "x", &mut tiny), Some(None));
        assert_eq!(query_u32("since=1234&z=1", "since"), Some(1234));
        assert_eq!(query_u32("", "since"), None);
    }

    #[test]
    fn form_encoding_round_trips() {
        for s in ["plain", "My Bridge!", "a&b=c+d%e", "café ✓", ""] {
            let encoded = alloc::format!("{}", FormStr(s));
            assert!(!encoded.contains(['&', '=', ' ']), "{}", encoded);
            let body = alloc::format!("k={}", encoded);
            let mut buf = [0u8; 64];
            assert_eq!(form_value(body.as_bytes(), "k", &mut buf), Some(Some(s)));
        }
    }

    #[test]
    fn json_escaping() {
        let s = alloc::format!("{}", JsonStr("a\"b\\c\n</script>"));
        assert_eq!(s, r#""a\"b\\c\u000a\u003c/script>""#);
    }

    #[test]
    fn response_head() {
        let mut out = alloc::string::String::new();
        write_head(&mut out, 404, "text/plain", 9, "").unwrap();
        assert!(out.starts_with("HTTP/1.1 404 Not Found\r\n"));
        assert!(out.contains("Content-Length: 9\r\n"));
        assert!(out.ends_with("\r\n\r\n"));
    }

    #[test]
    fn status_json() {
        let status = Status {
            deskflow: DeskflowState::Active,
            ble_connected: true,
            ble_bonded: true,
            server_fingerprint: None,
            client_fingerprint: Some([0x0f; 32]),
            uptime_s: 42,
            clipboard_chars: 12,
            clipboard_truncated: false,
            clipboard_max: 65536,
            firmware: "abc123",
            updates: true,
            typing: Some(3),
            tls_handshake_ms: Some(850),
        };
        let mut out = alloc::string::String::new();
        status.write_json(&mut out).unwrap();
        assert!(out.starts_with(
            r#"{"deskflow":"active","ble_connected":true,"ble_bonded":true,"uptime_s":42,"clipboard_chars":12,"clipboard_truncated":false,"clipboard_max":65536,"firmware":"abc123","updates":true,"typing":3,"tls_handshake_ms":850,"client_fingerprint":"0F:0F:"#
        ));
        assert!(out.contains(r#""client_db_line":"v2:sha256:0f0f"#));
        assert!(out.ends_with(r#""server_fingerprint":null}"#));

        let starting = Status { client_fingerprint: None, ..status };
        out.clear();
        starting.write_json(&mut out).unwrap();
        assert!(out.contains(r#""client_fingerprint":null,"client_db_line":null,"server_fingerprint":null}"#), "{}", out);
    }

    #[test]
    fn page_mentions_the_api() {
        for api in ["/api/state", "/api/log", "/api/settings", "/api/restart", "/api/forget-laptop", "/api/type-clipboard", "/api/stop-typing", "/api/clipboard", "/api/laptop/", "/api/laptop/pair", "/api/panel", "/api/panel/press", "/api/update", "/api/login", "/api/logout", "/api/password"] {
            assert!(PAGE.contains(api), "{}", api);
        }
        assert!(PAGE.contains("X-Bridge"));
    }
}
