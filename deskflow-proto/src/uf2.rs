//! Reading a `.uf2` as it's uploaded to the settings page, one block at a
//! time, without keeping the file. Only pages inside the firmware slot are
//! used, so the file with the bootloader in it works too.

pub const BLOCK_LEN: usize = 512;
pub const PAGE_LEN: usize = 256;
pub const FAMILY_RP2350_ARM_S: u32 = 0xe48b_ff59;

const MAGIC_START0: u32 = 0x0A32_4655;
const MAGIC_START1: u32 = 0x9E5D_5157;
const MAGIC_END: u32 = 0x0AB1_6F30;
const FLAG_NOT_MAIN_FLASH: u32 = 0x0000_0001;
const FLAG_FAMILY_ID_PRESENT: u32 = 0x0000_2000;

/// Why an upload was refused, for the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Uf2Error {
    NotUf2,
    /// A page out of order: flash is written front to back.
    OutOfOrder,
    /// No page for the firmware slot: a build from before the bootloader, or
    /// for another board.
    WrongImage,
}

impl Uf2Error {
    pub const fn message(self) -> &'static str {
        match self {
            Uf2Error::NotUf2 => "that isn't a UF2 file",
            Uf2Error::OutOfOrder => "the UF2 file's pages are out of order",
            Uf2Error::WrongImage => "that UF2 file isn't an update for this bridge",
        }
    }
}

/// One page for the firmware slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Page<'a> {
    /// From the start of the slot.
    pub offset: u32,
    pub data: &'a [u8; PAGE_LEN],
}

fn word(block: &[u8; BLOCK_LEN], at: usize) -> u32 {
    u32::from_le_bytes([block[at], block[at + 1], block[at + 2], block[at + 3]])
}

/// Reads the pages for the slot at `base`, `size` bytes long.
pub struct Reader {
    base: u32,
    size: u32,
    family: u32,
    block: [u8; BLOCK_LEN],
    filled: usize,
    next_offset: u32,
}

impl Reader {
    pub const fn new(base: u32, size: u32, family: u32) -> Self {
        Reader { base, size, family, block: [0; BLOCK_LEN], filled: 0, next_offset: 0 }
    }

    /// Takes bytes until a block is complete; returns how many it took.
    pub fn fill(&mut self, bytes: &[u8]) -> usize {
        let n = (BLOCK_LEN - self.filled).min(bytes.len());
        self.block[self.filled..self.filled + n].copy_from_slice(&bytes[..n]);
        self.filled += n;
        n
    }

    /// The page in a complete block, if it's one for the slot.
    pub fn take_page(&mut self) -> Result<Option<Page<'_>>, Uf2Error> {
        if self.filled < BLOCK_LEN {
            return Ok(None);
        }
        self.filled = 0;
        let b = &self.block;
        if word(b, 0) != MAGIC_START0 || word(b, 4) != MAGIC_START1 || word(b, 508) != MAGIC_END {
            return Err(Uf2Error::NotUf2);
        }
        let flags = word(b, 8);
        let target = word(b, 12);
        let family = word(b, 28);
        if flags & FLAG_NOT_MAIN_FLASH != 0
            || flags & FLAG_FAMILY_ID_PRESENT == 0
            || family != self.family
            || word(b, 16) as usize != PAGE_LEN
            || !target.is_multiple_of(PAGE_LEN as u32)
            || target < self.base
            || target - self.base >= self.size
        {
            return Ok(None);
        }
        let offset = target - self.base;
        if offset < self.next_offset {
            return Err(Uf2Error::OutOfOrder);
        }
        self.next_offset = offset + PAGE_LEN as u32;
        let data = b[32..32 + PAGE_LEN].try_into().map_err(|_| Uf2Error::NotUf2)?;
        Ok(Some(Page { offset, data }))
    }

    /// Bytes of the slot covered so far.
    pub fn image_len(&self) -> u32 {
        self.next_offset
    }
}

/// Whether the slot's first page is a vector table for code in the slot: its
/// stack pointer in RAM, its reset vector inside the slot.
pub fn looks_bootable(first_page: &[u8; PAGE_LEN], base: u32, size: u32, ram: core::ops::Range<u32>) -> bool {
    let word = |i: usize| u32::from_le_bytes([first_page[i], first_page[i + 1], first_page[i + 2], first_page[i + 3]]);
    let (sp, reset) = (word(0), word(4));
    (ram.start..=ram.end).contains(&sp) && reset >= base && reset - base < size
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: u32 = 0x1002_1000;
    const SIZE: u32 = 0x1000;

    fn block(target: u32, flags: u32, family: u32, fill: u8) -> [u8; BLOCK_LEN] {
        let mut b = [0u8; BLOCK_LEN];
        let words = [MAGIC_START0, MAGIC_START1, flags, target, PAGE_LEN as u32, 0, 1, family];
        for (i, w) in words.iter().enumerate() {
            b[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
        }
        b[32..32 + PAGE_LEN].fill(fill);
        b[508..].copy_from_slice(&MAGIC_END.to_le_bytes());
        b
    }

    fn good(target: u32, fill: u8) -> [u8; BLOCK_LEN] {
        block(target, FLAG_FAMILY_ID_PRESENT, FAMILY_RP2350_ARM_S, fill)
    }

    fn reader() -> Reader {
        Reader::new(BASE, SIZE, FAMILY_RP2350_ARM_S)
    }

    /// Feeds `bytes` in chunks, collecting (offset, first byte) of each page.
    fn feed(r: &mut Reader, bytes: &[u8], chunk: usize) -> Result<std::vec::Vec<(u32, u8)>, Uf2Error> {
        let mut pages = std::vec::Vec::new();
        for mut part in bytes.chunks(chunk) {
            while !part.is_empty() {
                let n = r.fill(part);
                part = &part[n..];
                if let Some(p) = r.take_page()? {
                    pages.push((p.offset, p.data[0]));
                }
            }
        }
        Ok(pages)
    }

    #[test]
    fn reads_pages_in_any_chunking() {
        let mut file = std::vec::Vec::new();
        file.extend_from_slice(&good(BASE, 1));
        file.extend_from_slice(&good(BASE + 256, 2));
        file.extend_from_slice(&good(BASE + 1024, 3));
        for chunk in [1, 7, 512, 513, 1024, 4096] {
            let mut r = reader();
            assert_eq!(feed(&mut r, &file, chunk).unwrap(), [(0, 1), (256, 2), (1024, 3)]);
            assert_eq!(r.image_len(), 1280);
        }
    }

    #[test]
    fn skips_what_is_not_for_the_slot() {
        let mut file = std::vec::Vec::new();
        // The bootloader, the RP2350-E10 block, another family, RAM, past the end.
        file.extend_from_slice(&good(0x1000_0000, 9));
        file.extend_from_slice(&block(0x10FF_FF00, FLAG_FAMILY_ID_PRESENT, 0xE48B_FF57, 9));
        file.extend_from_slice(&block(BASE, FLAG_FAMILY_ID_PRESENT, 0xE48B_FF56, 9));
        file.extend_from_slice(&block(BASE, FLAG_FAMILY_ID_PRESENT | FLAG_NOT_MAIN_FLASH, FAMILY_RP2350_ARM_S, 9));
        file.extend_from_slice(&good(BASE + SIZE, 9));
        file.extend_from_slice(&block(BASE, 0, FAMILY_RP2350_ARM_S, 9));
        let mut r = reader();
        assert!(feed(&mut r, &file, 512).unwrap().is_empty());
        assert_eq!(r.image_len(), 0);
    }

    #[test]
    fn refuses_garbage_and_disorder() {
        let mut r = reader();
        assert_eq!(feed(&mut r, &[0x42; 512], 512), Err(Uf2Error::NotUf2));
        let mut bad_end = good(BASE, 1);
        bad_end[511] = 0;
        assert_eq!(feed(&mut reader(), &bad_end, 512), Err(Uf2Error::NotUf2));
        let mut file = std::vec::Vec::new();
        file.extend_from_slice(&good(BASE + 256, 1));
        file.extend_from_slice(&good(BASE, 2));
        assert_eq!(feed(&mut reader(), &file, 512), Err(Uf2Error::OutOfOrder));
    }

    #[test]
    fn a_short_tail_is_no_page() {
        let mut r = reader();
        assert!(feed(&mut r, &good(BASE, 1)[..511], 512).unwrap().is_empty());
    }

    #[test]
    fn bootable_check() {
        let ram = 0x2000_0000..0x2008_0000;
        let mut page = [0u8; PAGE_LEN];
        page[..4].copy_from_slice(&0x2004_0000u32.to_le_bytes());
        page[4..8].copy_from_slice(&(BASE + 0x200 + 1).to_le_bytes());
        assert!(looks_bootable(&page, BASE, SIZE, ram.clone()));
        // Linked for the start of flash, as builds without the bootloader are.
        page[4..8].copy_from_slice(&0x1000_0201u32.to_le_bytes());
        assert!(!looks_bootable(&page, BASE, SIZE, ram.clone()));
        assert!(!looks_bootable(&[0xFF; PAGE_LEN], BASE, SIZE, ram));
    }
}
