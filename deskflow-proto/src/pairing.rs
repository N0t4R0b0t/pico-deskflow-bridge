//! The paired laptops and which one has the keyboard and mouse.

use core::fmt;

use heapless::Vec;

/// Laptops remembered at once.
pub const MAX_LAPTOPS: usize = 4;

/// Adds `item` to `list`, in the place of an existing entry that is the `same`
/// one, so numbers stay put for the hotkeys. A full list drops its oldest
/// entry, which is returned.
pub fn remember<T, const N: usize>(list: &mut Vec<T, N>, item: T, same: impl Fn(&T, &T) -> bool) -> Option<T> {
    if let Some(old) = list.iter_mut().find(|old| same(old, &item)) {
        *old = item;
        return None;
    }
    let evicted = if list.is_full() { Some(list.remove(0)) } else { None };
    // There's room now.
    let _ = list.push(item);
    evicted
}

/// What the BLE task is asked to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// Give the keyboard and mouse to this laptop.
    Switch(u8),
    /// Move on to the next paired laptop.
    Next,
    Forget(u8),
    /// Let any laptop connect for a while, so a new one can pair.
    Pair,
}

/// What the page shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Laptops {
    /// Bluetooth addresses, as sent over the air (least significant byte first).
    pub addresses: Vec<[u8; 6], MAX_LAPTOPS>,
    pub active: Option<usize>,
    pub connected: bool,
    pub pairing: bool,
}

impl Laptops {
    pub fn write_json(&self, w: &mut impl fmt::Write) -> fmt::Result {
        w.write_str(r#"{"list":["#)?;
        for (i, a) in self.addresses.iter().enumerate() {
            if i > 0 {
                w.write_str(",")?;
            }
            let active = self.active == Some(i);
            write!(
                w,
                r#"{{"address":"{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}","active":{},"connected":{}}}"#,
                a[5],
                a[4],
                a[3],
                a[2],
                a[1],
                a[0],
                active,
                active && self.connected,
            )?;
        }
        write!(w, r#"],"pairing":{}}}"#, self.pairing)
    }
}

/// The slot after `active`, wrapping around.
pub fn next_slot(active: Option<usize>, count: usize) -> Option<usize> {
    if count == 0 {
        return None;
    }
    Some(active.map_or(0, |i| (i + 1) % count))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn same(a: &(u8, u8), b: &(u8, u8)) -> bool {
        a.0 == b.0
    }

    #[test]
    fn numbers_stay_put() {
        let mut list: Vec<(u8, u8), 3> = Vec::new();
        assert_eq!(remember(&mut list, (1, 0), same), None);
        assert_eq!(remember(&mut list, (2, 0), same), None);
        // Re-pairing laptop 1 replaces its old keys where they are.
        assert_eq!(remember(&mut list, (1, 9), same), None);
        assert_eq!(&list[..], &[(1, 9), (2, 0)]);
        assert_eq!(remember(&mut list, (3, 0), same), None);
        assert_eq!(remember(&mut list, (4, 0), same), Some((1, 9)));
        assert_eq!(&list[..], &[(2, 0), (3, 0), (4, 0)]);
    }

    #[test]
    fn next_wraps_around() {
        assert_eq!(next_slot(None, 0), None);
        assert_eq!(next_slot(None, 2), Some(0));
        assert_eq!(next_slot(Some(0), 2), Some(1));
        assert_eq!(next_slot(Some(1), 2), Some(0));
        assert_eq!(next_slot(Some(0), 1), Some(0));
    }

    #[test]
    fn json() {
        let mut l = Laptops::default();
        let mut out = alloc::string::String::new();
        l.write_json(&mut out).unwrap();
        assert_eq!(out, r#"{"list":[],"pairing":false}"#);

        l.addresses.push([0x66, 0x55, 0x44, 0x33, 0x22, 0x11]).unwrap();
        l.addresses.push([1, 2, 3, 4, 5, 6]).unwrap();
        l.active = Some(1);
        l.connected = true;
        l.pairing = true;
        out.clear();
        l.write_json(&mut out).unwrap();
        assert_eq!(
            out,
            r#"{"list":[{"address":"11:22:33:44:55:66","active":false,"connected":false},{"address":"06:05:04:03:02:01","active":true,"connected":true}],"pairing":true}"#
        );
    }
}
