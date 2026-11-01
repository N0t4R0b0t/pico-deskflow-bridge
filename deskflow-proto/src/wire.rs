//! Message framing and parsing.
//!
//! Formats mirror `src/lib/deskflow/ProtocolTypes.cpp` upstream: `%1i`, `%2i`
//! and `%4i` are big-endian integers of that many bytes, `%s` is a 4-byte
//! length followed by bytes, and the hello's protocol name is 7 raw bytes.

/// Size of the big-endian length prefix in front of every message.
pub const FRAME_HEADER_LEN: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    /// Payload ended before all fields of a known message were read.
    Truncated,
}

/// A decoded server → client message. Payloads the client does not act on
/// (clipboard data, options, file transfer, …) come back as [`Message::Other`]
/// so the caller can log and skip them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Message<'a> {
    /// Initial greeting. `protocol` is `b"Synergy"` or `b"Barrier"`; the reply
    /// must echo the same name.
    Hello { protocol: [u8; 7], major: i16, minor: i16 },
    QueryInfo,
    InfoAck,
    KeepAlive,
    NoOp,
    Close,
    Enter { x: i16, y: i16, seq: u32, mask: u16 },
    Leave,
    ResetOptions,
    KeyDown { id: u16, mask: u16, button: u16 },
    KeyRepeat { id: u16, mask: u16, count: u16, button: u16 },
    KeyUp { id: u16, mask: u16, button: u16 },
    MouseDown { button: u8 },
    MouseUp { button: u8 },
    MouseMove { x: i16, y: i16 },
    MouseRelMove { dx: i16, dy: i16 },
    MouseWheel { dx: i16, dy: i16 },
    /// `EICV`: server refuses our protocol version.
    Incompatible { major: i16, minor: i16 },
    /// `EBSY`: a client with our name is already connected.
    Busy,
    /// `EUNK`: our name is not a screen in the server's layout.
    UnknownClient,
    /// `EBAD`: server considers us to have violated the protocol.
    Bad,
    Other { code: [u8; 4], body: &'a [u8] },
}

struct Reader<'a> {
    buf: &'a [u8],
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], ParseError> {
        if self.buf.len() < n {
            return Err(ParseError::Truncated);
        }
        let (head, tail) = self.buf.split_at(n);
        self.buf = tail;
        Ok(head)
    }

    fn u8(&mut self) -> Result<u8, ParseError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, ParseError> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    fn i16(&mut self) -> Result<i16, ParseError> {
        Ok(self.u16()? as i16)
    }

    fn u32(&mut self) -> Result<u32, ParseError> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
}

/// Parses one message payload (without its length prefix).
pub fn parse(payload: &[u8]) -> Result<Message<'_>, ParseError> {
    // The hello is the only message without a 4-character code: 7 bytes of
    // protocol name followed by the version.
    if payload.len() == 11 && (payload.starts_with(b"Synergy") || payload.starts_with(b"Barrier")) {
        let mut r = Reader { buf: &payload[7..] };
        let mut protocol = [0; 7];
        protocol.copy_from_slice(&payload[..7]);
        return Ok(Message::Hello { protocol, major: r.i16()?, minor: r.i16()? });
    }

    let mut r = Reader { buf: payload };
    let code: [u8; 4] = r.take(4)?.try_into().unwrap();
    let msg = match &code {
        b"QINF" => Message::QueryInfo,
        b"CIAK" => Message::InfoAck,
        b"CALV" => Message::KeepAlive,
        b"CNOP" => Message::NoOp,
        b"CBYE" => Message::Close,
        b"CINN" => Message::Enter { x: r.i16()?, y: r.i16()?, seq: r.u32()?, mask: r.u16()? },
        b"COUT" => Message::Leave,
        b"CROP" => Message::ResetOptions,
        b"DKDN" => Message::KeyDown { id: r.u16()?, mask: r.u16()?, button: r.u16()? },
        b"DKRP" => Message::KeyRepeat { id: r.u16()?, mask: r.u16()?, count: r.u16()?, button: r.u16()? },
        b"DKUP" => Message::KeyUp { id: r.u16()?, mask: r.u16()?, button: r.u16()? },
        b"DMDN" => Message::MouseDown { button: r.u8()? },
        b"DMUP" => Message::MouseUp { button: r.u8()? },
        b"DMMV" => Message::MouseMove { x: r.i16()?, y: r.i16()? },
        b"DMRM" => Message::MouseRelMove { dx: r.i16()?, dy: r.i16()? },
        b"DMWM" => Message::MouseWheel { dx: r.i16()?, dy: r.i16()? },
        b"EICV" => Message::Incompatible { major: r.i16()?, minor: r.i16()? },
        b"EBSY" => Message::Busy,
        b"EUNK" => Message::UnknownClient,
        b"EBAD" => Message::Bad,
        _ => Message::Other { code, body: r.buf },
    };
    Ok(msg)
}

/// Writes length-prefixed messages into a caller-provided buffer. Every
/// encoder returns the total number of bytes written, prefix included, or
/// `None` if the buffer is too small.
struct Writer<'a> {
    buf: &'a mut [u8],
    pos: usize,
}

impl<'a> Writer<'a> {
    fn new(buf: &'a mut [u8]) -> Option<Self> {
        if buf.len() < FRAME_HEADER_LEN {
            return None;
        }
        Some(Writer { buf, pos: FRAME_HEADER_LEN })
    }

    fn bytes(&mut self, b: &[u8]) -> Option<()> {
        let end = self.pos + b.len();
        self.buf.get_mut(self.pos..end)?.copy_from_slice(b);
        self.pos = end;
        Some(())
    }

    fn i16(&mut self, v: i16) -> Option<()> {
        self.bytes(&v.to_be_bytes())
    }

    fn string(&mut self, s: &[u8]) -> Option<()> {
        self.bytes(&(s.len() as u32).to_be_bytes())?;
        self.bytes(s)
    }

    fn finish(self) -> usize {
        let len = (self.pos - FRAME_HEADER_LEN) as u32;
        self.buf[..FRAME_HEADER_LEN].copy_from_slice(&len.to_be_bytes());
        self.pos
    }
}

/// Reply to [`Message::Hello`]: protocol name, our version, and the screen
/// name that must match a screen in the server's layout.
pub fn encode_hello_back(buf: &mut [u8], protocol: &[u8; 7], name: &str) -> Option<usize> {
    let mut w = Writer::new(buf)?;
    w.bytes(protocol)?;
    w.i16(crate::PROTOCOL_MAJOR)?;
    w.i16(crate::PROTOCOL_MINOR)?;
    w.string(name.as_bytes())?;
    Some(w.finish())
}

/// Screen geometry, sent in reply to [`Message::QueryInfo`]. `cursor_*` is
/// where we believe the cursor currently is.
pub fn encode_info(buf: &mut [u8], width: i16, height: i16, cursor_x: i16, cursor_y: i16) -> Option<usize> {
    let mut w = Writer::new(buf)?;
    w.bytes(b"DINF")?;
    // x, y, w, h, obsolete warp size, cursor x, cursor y
    for v in [0, 0, width, height, 0, cursor_x, cursor_y] {
        w.i16(v)?;
    }
    Some(w.finish())
}

/// Reply to [`Message::KeepAlive`]; the server drops clients that stop echoing.
pub fn encode_keep_alive(buf: &mut [u8]) -> Option<usize> {
    let mut w = Writer::new(buf)?;
    w.bytes(b"CALV")?;
    Some(w.finish())
}

/// Reads the length prefix of a frame.
pub fn frame_len(header: [u8; FRAME_HEADER_LEN]) -> usize {
    u32::from_be_bytes(header) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hello() {
        let msg = parse(b"Synergy\x00\x01\x00\x08").unwrap();
        assert_eq!(msg, Message::Hello { protocol: *b"Synergy", major: 1, minor: 8 });
    }

    #[test]
    fn parses_key_down() {
        let msg = parse(b"DKDN\x00\x61\x00\x01\x00\x01").unwrap();
        assert_eq!(msg, Message::KeyDown { id: 0x61, mask: 1, button: 1 });
    }

    #[test]
    fn parses_negative_wheel() {
        let msg = parse(b"DMWM\x00\x00\xff\x88").unwrap();
        assert_eq!(msg, Message::MouseWheel { dx: 0, dy: -120 });
    }

    #[test]
    fn truncated_is_an_error() {
        assert_eq!(parse(b"DMMV\x00"), Err(ParseError::Truncated));
    }

    #[test]
    fn unknown_code_is_other() {
        let msg = parse(b"LSYNxyz").unwrap();
        assert_eq!(msg, Message::Other { code: *b"LSYN", body: b"xyz" });
    }

    #[test]
    fn encodes_hello_back() {
        let mut buf = [0; 64];
        let n = encode_hello_back(&mut buf, b"Synergy", "laptop").unwrap();
        assert_eq!(&buf[..n], b"\x00\x00\x00\x15Synergy\x00\x01\x00\x06\x00\x00\x00\x06laptop");
    }

    #[test]
    fn encodes_info() {
        let mut buf = [0; 64];
        let n = encode_info(&mut buf, 1920, 1080, 960, 540).unwrap();
        assert_eq!(frame_len(buf[..4].try_into().unwrap()), 18);
        assert_eq!(&buf[4..8], b"DINF");
        assert_eq!(n, 22);
        assert_eq!(&buf[12..16], &[0x07, 0x80, 0x04, 0x38]);
    }

    #[test]
    fn encoder_rejects_small_buffer() {
        let mut buf = [0; 8];
        assert_eq!(encode_hello_back(&mut buf, b"Synergy", "laptop"), None);
    }
}
