//! The tail of the log, kept in RAM for the web page.
//!
//! Positions are absolute byte counts since boot, so a reader can ask for
//! "everything after what I already have" and notice when it fell behind.

pub struct LogRing<const N: usize> {
    buf: [u8; N],
    /// Total bytes ever written; the ring holds the last `min(end, N)`.
    end: u32,
}

impl<const N: usize> Default for LogRing<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> LogRing<N> {
    pub const fn new() -> Self {
        Self { buf: [0; N], end: 0 }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        // Only the last N bytes of an oversized write can survive anyway.
        let skip = bytes.len().saturating_sub(N);
        self.end = self.end.wrapping_add(skip as u32);
        for &b in &bytes[skip..] {
            self.buf[self.end as usize % N] = b;
            self.end = self.end.wrapping_add(1);
        }
    }

    /// Position after the newest byte.
    pub fn end(&self) -> u32 {
        self.end
    }

    /// Copies bytes from position `since` into `out`, returning how many and
    /// the position to ask for next. If `since` has already been overwritten
    /// (or is in the future, i.e. from before a reboot), reading starts at
    /// the oldest byte still held.
    pub fn read(&self, since: u32, out: &mut [u8]) -> (usize, u32) {
        let oldest = self.end.saturating_sub(N as u32);
        let start = if since < oldest || since > self.end { oldest } else { since };
        let n = ((self.end - start) as usize).min(out.len());
        for (i, o) in out[..n].iter_mut().enumerate() {
            *o = self.buf[(start as usize + i) % N];
        }
        (n, start + n as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_all<const N: usize>(ring: &LogRing<N>, since: u32) -> (alloc::vec::Vec<u8>, u32) {
        let mut out = [0u8; 64];
        let (n, next) = ring.read(since, &mut out);
        (out[..n].to_vec(), next)
    }

    #[test]
    fn reads_incrementally() {
        let mut ring = LogRing::<8>::new();
        ring.push(b"abc");
        assert_eq!(read_all(&ring, 0), (b"abc".to_vec(), 3));
        assert_eq!(read_all(&ring, 3), (b"".to_vec(), 3));
        ring.push(b"de");
        assert_eq!(read_all(&ring, 3), (b"de".to_vec(), 5));
    }

    #[test]
    fn wraps_and_skips_lost_bytes() {
        let mut ring = LogRing::<8>::new();
        ring.push(b"0123456789");
        assert_eq!(ring.end(), 10);
        // Bytes 0 and 1 are gone; a reader at 0 gets the oldest held.
        assert_eq!(read_all(&ring, 0), (b"23456789".to_vec(), 10));
        assert_eq!(read_all(&ring, 7), (b"789".to_vec(), 10));
        ring.push(b"ab");
        assert_eq!(read_all(&ring, 10), (b"ab".to_vec(), 12));
    }

    #[test]
    fn oversized_write_keeps_the_tail() {
        let mut ring = LogRing::<4>::new();
        ring.push(b"xy");
        ring.push(b"abcdefgh");
        assert_eq!(ring.end(), 10);
        assert_eq!(read_all(&ring, 0), (b"efgh".to_vec(), 10));
    }

    #[test]
    fn position_from_before_a_reboot() {
        let mut ring = LogRing::<8>::new();
        ring.push(b"new");
        assert_eq!(read_all(&ring, 5000), (b"new".to_vec(), 3));
    }

    #[test]
    fn short_output_buffer() {
        let mut ring = LogRing::<8>::new();
        ring.push(b"abcdef");
        let mut out = [0u8; 4];
        assert_eq!(ring.read(0, &mut out), (4, 4));
        assert_eq!(&out, b"abcd");
        assert_eq!(ring.read(4, &mut out), (2, 6));
    }
}
