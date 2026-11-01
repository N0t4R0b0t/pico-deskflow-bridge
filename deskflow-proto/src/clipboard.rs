//! Receiving the server's clipboard (`DCLP`).
//!
//! The laptop only sees a keyboard, so the clipboard can't be *set* there;
//! instead the text is kept and typed out on request (see
//! [`crate::typing`]). Only the main clipboard's text format is kept.
//!
//! Protocol 1.6 sends a clipboard as a stream of `DCLP` messages:
//! `id:u8 seq:u32 mark:u8 data:string`, where `mark` is
//!
//! * 1 (start): `data` is the total size, as decimal text;
//! * 2 (chunk): `data` is the next piece (up to 512 KiB) of the clipboard;
//! * 3 (end): `data` is empty.
//!
//! The reassembled clipboard is `count:u32` then, per format,
//! `format:u32 size:u32 bytes`; format 0 is UTF-8 text with LF newlines.
//! A chunk can be far larger than the device can hold, so [`Receiver`] is
//! fed a chunk's data in pieces and hands the text on as it goes; the caller
//! keeps as much as it has room for ([`utf8_prefix`] trims a cut-off
//! character).

use heapless::Vec;

/// `DCLP` body bytes before the data: id, sequence, mark, data length.
pub const CHUNK_HEADER_LEN: usize = 1 + 4 + 1 + 4;

/// The clipboard (as opposed to the X11 primary selection, id 1).
const CLIPBOARD_ID: u8 = 0;
const FORMAT_TEXT: u32 = 0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Start,
    Chunk,
    End,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChunkHeader {
    pub id: u8,
    pub mark: Mark,
    /// Length of the data that follows the header.
    pub len: u32,
}

/// Parses the start of a `DCLP` body (after the 4-byte code).
pub fn parse_chunk_header(body: &[u8]) -> Option<ChunkHeader> {
    let h: &[u8; CHUNK_HEADER_LEN] = body.get(..CHUNK_HEADER_LEN)?.try_into().ok()?;
    let mark = match h[5] {
        1 => Mark::Start,
        2 => Mark::Chunk,
        3 => Mark::End,
        _ => return None,
    };
    Some(ChunkHeader { id: h[0], mark, len: u32::from_be_bytes([h[6], h[7], h[8], h[9]]) })
}

/// The longest prefix of `bytes` that is valid UTF-8, for text cut off at a
/// buffer limit.
pub fn utf8_prefix(bytes: &[u8]) -> usize {
    match core::str::from_utf8(bytes) {
        Ok(_) => bytes.len(),
        Err(e) => e.valid_up_to(),
    }
}

/// Where we are in the reassembled clipboard.
#[derive(Clone, Copy, Debug)]
enum Parse {
    /// Collecting a 4-byte format count, or an 8-byte format header.
    Header { want: usize },
    /// Inside a format's data.
    Data { left: u32, text: bool },
    /// Done or malformed; ignore the rest.
    Skip,
}

/// What a `DCLP` frame means for the caller's copy of the text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// Nothing to do.
    None,
    /// A new clipboard is arriving: drop the old text.
    Started,
    /// The text is complete.
    Finished,
}

/// Reassembles `DCLP` streams, passing the text format's bytes on.
pub struct Receiver {
    /// Between a start and an end mark for the main clipboard.
    active: bool,
    /// The current frame's header.
    frame: Option<ChunkHeader>,
    parse: Parse,
    header: Vec<u8, 8>,
    formats_left: u32,
}

impl Default for Receiver {
    fn default() -> Self {
        Self::new()
    }
}

impl Receiver {
    pub const fn new() -> Self {
        Self { active: false, frame: None, parse: Parse::Header { want: 4 }, header: Vec::new(), formats_left: 0 }
    }

    /// Starts a `DCLP` frame; its data follows via [`Receiver::data`].
    pub fn begin(&mut self, header: ChunkHeader) -> Event {
        self.frame = Some(header);
        if header.id != CLIPBOARD_ID {
            return Event::None;
        }
        match header.mark {
            Mark::Start => {
                self.active = true;
                self.parse = Parse::Header { want: 4 };
                self.header.clear();
                self.formats_left = 0;
                Event::Started
            }
            Mark::Chunk | Mark::End if !self.active => {
                self.frame = None;
                Event::None
            }
            _ => Event::None,
        }
    }

    /// Feeds (part of) the current frame's data; text bytes go to `text`.
    pub fn data(&mut self, mut bytes: &[u8], mut text: impl FnMut(&[u8])) {
        let Some(ChunkHeader { id: CLIPBOARD_ID, mark: Mark::Chunk, .. }) = self.frame else {
            return;
        };
        while !bytes.is_empty() {
            match self.parse {
                Parse::Skip => return,
                Parse::Header { want } => {
                    let n = (want - self.header.len()).min(bytes.len());
                    let _ = self.header.extend_from_slice(&bytes[..n]);
                    bytes = &bytes[n..];
                    if self.header.len() == want {
                        self.parse = self.header_done();
                        self.header.clear();
                    }
                }
                Parse::Data { left, text: is_text } => {
                    let n = (left as usize).min(bytes.len());
                    if is_text {
                        text(&bytes[..n]);
                    }
                    bytes = &bytes[n..];
                    let left = left - n as u32;
                    self.parse = if left > 0 { Parse::Data { left, text: is_text } } else { self.next_format() };
                }
            }
        }
    }

    fn header_done(&mut self) -> Parse {
        let h = &self.header;
        if h.len() == 4 {
            self.formats_left = u32::from_be_bytes([h[0], h[1], h[2], h[3]]);
            return self.next_format();
        }
        let format = u32::from_be_bytes([h[0], h[1], h[2], h[3]]);
        let size = u32::from_be_bytes([h[4], h[5], h[6], h[7]]);
        let text = format == FORMAT_TEXT;
        if size == 0 { self.next_format() } else { Parse::Data { left: size, text } }
    }

    fn next_format(&mut self) -> Parse {
        if self.formats_left == 0 {
            return Parse::Skip;
        }
        self.formats_left -= 1;
        Parse::Header { want: 8 }
    }

    /// Ends the current frame.
    pub fn end(&mut self) -> Event {
        let Some(frame) = self.frame.take() else { return Event::None };
        if frame.id != CLIPBOARD_ID || frame.mark != Mark::End {
            return Event::None;
        }
        self.active = false;
        Event::Finished
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec as StdVec;

    fn header(id: u8, mark: u8, len: u32) -> StdVec<u8> {
        let mut h = alloc::vec![id, 0, 0, 0, 7, mark];
        h.extend_from_slice(&len.to_be_bytes());
        h
    }

    fn marshall(formats: &[(u32, &[u8])]) -> StdVec<u8> {
        let mut out = (formats.len() as u32).to_be_bytes().to_vec();
        for (format, data) in formats {
            out.extend_from_slice(&format.to_be_bytes());
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            out.extend_from_slice(data);
        }
        out
    }

    /// What a caller does: keep up to `max` bytes, cut at a character.
    struct Text {
        bytes: StdVec<u8>,
        max: usize,
        truncated: bool,
        finished: bool,
    }

    impl Text {
        fn new(max: usize) -> Self {
            Text { bytes: StdVec::new(), max, truncated: false, finished: false }
        }

        fn on(&mut self, event: Event) {
            match event {
                Event::Started => *self = Text::new(self.max),
                Event::Finished => {
                    let valid = utf8_prefix(&self.bytes);
                    self.truncated |= valid < self.bytes.len();
                    self.bytes.truncate(valid);
                    self.finished = true;
                }
                Event::None => {}
            }
        }

        fn push(&mut self, b: &[u8]) {
            let room = self.max - self.bytes.len();
            self.bytes.extend_from_slice(&b[..b.len().min(room)]);
            self.truncated |= b.len() > room;
        }

        fn str(&self) -> &str {
            core::str::from_utf8(&self.bytes).unwrap()
        }
    }

    /// Feeds a whole transfer, splitting chunk data into `piece`-byte reads.
    fn transfer(rx: &mut Receiver, out: &mut Text, id: u8, chunks: &[&[u8]], piece: usize) {
        let total: usize = chunks.iter().map(|c| c.len()).sum();
        let size = alloc::format!("{}", total);
        out.on(rx.begin(parse_chunk_header(&header(id, 1, size.len() as u32)).unwrap()));
        rx.data(size.as_bytes(), |b| out.push(b));
        out.on(rx.end());
        for chunk in chunks {
            out.on(rx.begin(parse_chunk_header(&header(id, 2, chunk.len() as u32)).unwrap()));
            for p in chunk.chunks(piece) {
                rx.data(p, |b| out.push(b));
            }
            out.on(rx.end());
        }
        out.on(rx.begin(parse_chunk_header(&header(id, 3, 0)).unwrap()));
        out.on(rx.end());
    }

    #[test]
    fn header_parsing() {
        assert_eq!(parse_chunk_header(&header(0, 2, 300)), Some(ChunkHeader { id: 0, mark: Mark::Chunk, len: 300 }));
        assert_eq!(parse_chunk_header(&header(0, 9, 0)), None);
        assert_eq!(parse_chunk_header(&[0, 1, 2]), None);
    }

    #[test]
    fn extracts_text_among_other_formats() {
        let data = marshall(&[(1, b"<b>hi</b>"), (0, "héllo\nworld".as_bytes()), (2, &[0xff; 40])]);
        for piece in [1, 3, 7, 1000] {
            let (mut rx, mut out) = (Receiver::new(), Text::new(64));
            transfer(&mut rx, &mut out, 0, &[&data], piece);
            assert!(out.finished);
            assert_eq!(out.str(), "héllo\nworld", "piece {}", piece);
            assert!(!out.truncated);
        }
    }

    #[test]
    fn data_split_across_chunks() {
        let data = marshall(&[(0, b"abcdefghij")]);
        let (a, b) = data.split_at(11);
        let (mut rx, mut out) = (Receiver::new(), Text::new(64));
        transfer(&mut rx, &mut out, 0, &[a, b], 2);
        assert_eq!(out.str(), "abcdefghij");
    }

    #[test]
    fn truncates_at_a_character_boundary() {
        let data = marshall(&[(0, "aaaé".as_bytes())]);
        let (mut rx, mut out) = (Receiver::new(), Text::new(4));
        transfer(&mut rx, &mut out, 0, &[&data], 100);
        assert_eq!(out.str(), "aaa");
        assert!(out.truncated);
        assert_eq!(utf8_prefix("é".as_bytes()), 2);
        assert_eq!(utf8_prefix(&"é".as_bytes()[..1]), 0);
    }

    #[test]
    fn ignores_the_selection_and_stray_chunks() {
        let data = marshall(&[(0, b"primary")]);
        let (mut rx, mut out) = (Receiver::new(), Text::new(64));
        transfer(&mut rx, &mut out, 1, &[&data], 100);
        assert!(!out.finished && out.bytes.is_empty());
        // Chunk and end without a start.
        assert_eq!(rx.begin(parse_chunk_header(&header(0, 2, 3)).unwrap()), Event::None);
        rx.data(b"abc", |b| out.push(b));
        assert_eq!(rx.end(), Event::None);
        rx.begin(parse_chunk_header(&header(0, 3, 0)).unwrap());
        assert_eq!(rx.end(), Event::None);
        assert!(out.bytes.is_empty());
    }

    #[test]
    fn no_text_format_gives_empty_text() {
        let data = marshall(&[(2, &[1, 2, 3])]);
        let (mut rx, mut out) = (Receiver::new(), Text::new(64));
        transfer(&mut rx, &mut out, 0, &[&data], 100);
        assert!(out.finished);
        assert_eq!(out.str(), "");
    }

    #[test]
    fn a_new_start_discards_a_partial_transfer() {
        let (mut rx, mut out) = (Receiver::new(), Text::new(64));
        out.on(rx.begin(parse_chunk_header(&header(0, 1, 2)).unwrap()));
        out.on(rx.end());
        out.on(rx.begin(parse_chunk_header(&header(0, 2, 6)).unwrap()));
        rx.data(&marshall(&[(0, b"old")])[..6], |b| out.push(b));
        out.on(rx.end());
        let data = marshall(&[(0, b"new")]);
        transfer(&mut rx, &mut out, 0, &[&data], 100);
        assert_eq!(out.str(), "new");
    }
}
