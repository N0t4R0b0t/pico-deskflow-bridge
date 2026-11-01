//! Reading the Bluetooth controller's "packets sent" reports, so moves can
//! wait for the radio instead of guessing its speed (see the firmware's
//! `flow`).

const NUMBER_OF_COMPLETED_PACKETS: u8 = 0x13;

/// The packets an HCI event reports as sent, if it's that report. `event` is
/// the event code, its length byte and as many parameters as were kept:
/// a handle count, then a 2-byte handle and 2-byte packet count for each.
pub fn completed_packets(event: &[u8]) -> Option<u32> {
    let [NUMBER_OF_COMPLETED_PACKETS, _, handles, entries @ ..] = event else {
        return None;
    };
    let (entries, _) = entries.as_chunks::<4>();
    let counted = entries.iter().take(*handles as usize);
    Some(counted.map(|e| u32::from(u16::from_le_bytes([e[2], e[3]]))).sum())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_completed_packets() {
        assert_eq!(completed_packets(&[0x13, 5, 1, 0x40, 0x00, 2, 0]), Some(2));
        assert_eq!(completed_packets(&[0x13, 9, 2, 0x40, 0, 1, 0, 0x41, 0, 3, 0]), Some(4));
        assert_eq!(completed_packets(&[0x13, 5, 1, 0x40, 0x00, 0x00, 0x01]), Some(256));
        assert_eq!(completed_packets(&[0x13, 1, 0]), Some(0));
    }

    #[test]
    fn ignores_everything_else() {
        assert_eq!(completed_packets(&[0x0E, 4, 1, 0x40, 0x00, 2]), None);
        assert_eq!(completed_packets(&[0x13]), None);
        assert_eq!(completed_packets(&[]), None);
        // Cut short, or claiming more than it holds: count what's there.
        assert_eq!(completed_packets(&[0x13, 9, 2, 0x40, 0, 1, 0, 0x41]), Some(1));
        // Trailing bytes beyond the handle count don't count.
        assert_eq!(completed_packets(&[0x13, 9, 1, 0x40, 0, 1, 0, 0x41, 0, 3, 0]), Some(1));
    }
}
