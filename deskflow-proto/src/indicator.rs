//! Status LED patterns. The Pico W has a single LED, so the state is in the
//! rhythm:
//!
//! | State | LED |
//! |---|---|
//! | neither Deskflow nor the laptop connected | short blip every 2 s |
//! | one of them connected | slow blink (1 Hz) |
//! | both connected | on; flickers off while input is forwarded |

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    None,
    Partial,
    Full,
}

impl Link {
    pub fn from(deskflow: bool, ble: bool) -> Self {
        match (deskflow, ble) {
            (true, true) => Link::Full,
            (false, false) => Link::None,
            _ => Link::Partial,
        }
    }
}

const BLIP_PERIOD_MS: u64 = 2000;
const BLIP_MS: u64 = 80;
const BLINK_PERIOD_MS: u64 = 1000;
/// An activity flicker: this long off, then at least as long on, so a
/// continuous stream of input reads as a steady ~6 Hz flicker.
const FLICKER_OFF_MS: u64 = 60;
const FLICKER_GAP_MS: u64 = 100;

#[derive(Default)]
pub struct Indicator {
    /// Start of the current (or last) activity flicker.
    flicker_at: Option<u64>,
}

impl Indicator {
    pub const fn new() -> Self {
        Self { flicker_at: None }
    }

    /// Whether the LED should be lit at `now_ms`. `activity` is true when
    /// input was forwarded since the last call.
    pub fn update(&mut self, now_ms: u64, link: Link, activity: bool) -> bool {
        match link {
            Link::None => now_ms % BLIP_PERIOD_MS < BLIP_MS,
            Link::Partial => now_ms % BLINK_PERIOD_MS < BLINK_PERIOD_MS / 2,
            Link::Full => {
                let ready = self.flicker_at.is_none_or(|t| now_ms >= t + FLICKER_OFF_MS + FLICKER_GAP_MS);
                if activity && ready {
                    self.flicker_at = Some(now_ms);
                }
                !self.flicker_at.is_some_and(|t| now_ms < t + FLICKER_OFF_MS)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_blips() {
        let mut led = Indicator::new();
        assert!(led.update(0, Link::None, false));
        assert!(!led.update(100, Link::None, false));
        assert!(!led.update(1999, Link::None, false));
        assert!(led.update(2010, Link::None, false));
        // Activity doesn't matter when not fully connected.
        assert!(!led.update(2500, Link::None, true));
    }

    #[test]
    fn partial_blinks_at_1hz() {
        let mut led = Indicator::new();
        assert!(led.update(100, Link::Partial, false));
        assert!(!led.update(600, Link::Partial, false));
        assert!(led.update(1100, Link::Partial, false));
    }

    #[test]
    fn connected_is_solid_and_flickers_on_activity() {
        let mut led = Indicator::new();
        assert!(led.update(0, Link::Full, false));
        assert!(!led.update(10, Link::Full, true));
        assert!(!led.update(50, Link::Full, true));
        assert!(led.update(80, Link::Full, true));
        // Still within the gap: activity doesn't restart the flicker yet.
        assert!(led.update(150, Link::Full, true));
        assert!(!led.update(170, Link::Full, true));
        assert!(led.update(240, Link::Full, false));
        assert!(led.update(5000, Link::Full, false));
    }

    #[test]
    fn link_from_flags() {
        assert_eq!(Link::from(true, true), Link::Full);
        assert_eq!(Link::from(true, false), Link::Partial);
        assert_eq!(Link::from(false, true), Link::Partial);
        assert_eq!(Link::from(false, false), Link::None);
    }
}
