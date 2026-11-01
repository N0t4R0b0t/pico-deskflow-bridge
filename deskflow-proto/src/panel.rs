//! The optional buttons and LEDs. The physical ones and the settings page's
//! virtual ones share this model: LED patterns, press timing and actions.
//!
//! Nothing here needs hardware to be fitted: unconnected inputs read as not
//! pressed, and the page does everything the buttons do.

use core::fmt;

pub const BUTTONS: usize = 3;
pub const LEDS: usize = 4;
/// A press counts as long after this.
pub const LONG_PRESS_MS: u64 = 1_000;
/// Restarting takes a longer hold, so it doesn't happen by accident.
pub const RESTART_HOLD_MS: u64 = 5_000;
/// Readings a change has to survive, at one reading per tick.
const DEBOUNCE_READINGS: u8 = 2;

const CONSUMER_PLAY_PAUSE: u16 = 0x00CD;
const CONSUMER_MUTE: u16 = 0x00E2;

/// What a press does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    None,
    TypeClipboard,
    StopTyping,
    NextLaptop,
    ReleaseAllKeys,
    /// Super+L.
    LockLaptop,
    PlayPause,
    Mute,
    Restart,
}

impl Action {
    pub const ALL: [Action; 9] = [
        Action::None,
        Action::TypeClipboard,
        Action::StopTyping,
        Action::NextLaptop,
        Action::ReleaseAllKeys,
        Action::LockLaptop,
        Action::PlayPause,
        Action::Mute,
        Action::Restart,
    ];

    /// The name in forms and JSON.
    pub const fn name(self) -> &'static str {
        match self {
            Action::None => "none",
            Action::TypeClipboard => "type",
            Action::StopTyping => "stop",
            Action::NextLaptop => "next",
            Action::ReleaseAllKeys => "release",
            Action::LockLaptop => "lock",
            Action::PlayPause => "playpause",
            Action::Mute => "mute",
            Action::Restart => "restart",
        }
    }

    pub fn from_name(name: &str) -> Option<Action> {
        Action::ALL.into_iter().find(|a| a.name() == name)
    }

    /// The consumer-control usage this sends, for the media actions.
    pub const fn consumer_usage(self) -> Option<u16> {
        match self {
            Action::PlayPause => Some(CONSUMER_PLAY_PAUSE),
            Action::Mute => Some(CONSUMER_MUTE),
            _ => None,
        }
    }
}

/// What one button does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ButtonActions {
    pub short: Action,
    pub long: Action,
}

impl ButtonActions {
    /// How long a press has to last to be long.
    pub const fn hold_ms(self) -> u64 {
        match self.long {
            Action::Restart => RESTART_HOLD_MS,
            _ => LONG_PRESS_MS,
        }
    }
}

pub const DEFAULT_BUTTONS: [ButtonActions; BUTTONS] = [
    ButtonActions { short: Action::TypeClipboard, long: Action::StopTyping },
    ButtonActions { short: Action::NextLaptop, long: Action::ReleaseAllKeys },
    ButtonActions { short: Action::LockLaptop, long: Action::Restart },
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Press {
    Short,
    Long,
}

/// Debounces one button and tells short presses from long ones. A long press
/// fires while still held, a short one on release.
#[derive(Clone, Copy, Debug, Default)]
pub struct Button {
    reading: bool,
    same: u8,
    pressed: bool,
    pressed_at: u64,
    long_fired: bool,
}

impl Button {
    pub const fn new() -> Self {
        Button { reading: false, same: 0, pressed: false, pressed_at: 0, long_fired: false }
    }

    pub fn is_pressed(&self) -> bool {
        self.pressed
    }

    /// Feeds one reading.
    pub fn update(&mut self, now_ms: u64, reading: bool, hold_ms: u64) -> Option<Press> {
        if reading != self.reading {
            self.reading = reading;
            self.same = 0;
        }
        self.same = self.same.saturating_add(1);
        if self.same >= DEBOUNCE_READINGS && reading != self.pressed {
            self.pressed = reading;
            if reading {
                self.pressed_at = now_ms;
                self.long_fired = false;
            } else if !self.long_fired {
                return Some(Press::Short);
            }
        }
        if self.pressed && !self.long_fired && now_ms - self.pressed_at >= hold_ms {
            self.long_fired = true;
            return Some(Press::Long);
        }
        None
    }
}

/// What the LEDs show.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Inputs {
    pub deskflow_connected: bool,
    pub deskflow_connecting: bool,
    pub ble_connected: bool,
    pub ble_pairing: bool,
    /// Input or typing just now.
    pub activity: bool,
    /// A problem the log explains.
    pub attention: bool,
}

const BLINK_MS: u64 = 250;

/// The LEDs, in order: Deskflow, Bluetooth, Activity, Attention.
pub fn leds(now_ms: u64, inputs: Inputs) -> [bool; LEDS] {
    let blink = (now_ms / BLINK_MS).is_multiple_of(2);
    [
        inputs.deskflow_connected || (inputs.deskflow_connecting && blink),
        inputs.ble_connected || (inputs.ble_pairing && blink),
        inputs.activity,
        inputs.attention && blink,
    ]
}

/// The LEDs and press timing, for the page's virtual panel.
pub fn write_json(w: &mut impl fmt::Write, leds: [bool; LEDS], buttons: &[ButtonActions; BUTTONS]) -> fmt::Result {
    write!(
        w,
        r#"{{"leds":[{},{},{},{}],"hold_ms":[{},{},{}]}}"#,
        leds[0],
        leds[1],
        leds[2],
        leds[3],
        buttons[0].hold_ms(),
        buttons[1].hold_ms(),
        buttons[2].hold_ms(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs readings a tick of 20 ms apart, collecting presses.
    fn run(b: &mut Button, now: &mut u64, reading: bool, ticks: u64, hold: u64) -> std::vec::Vec<Press> {
        let mut out = std::vec::Vec::new();
        for _ in 0..ticks {
            *now += 20;
            out.extend(b.update(*now, reading, hold));
        }
        out
    }

    #[test]
    fn short_press_fires_on_release() {
        let (mut b, mut t) = (Button::new(), 0);
        assert!(run(&mut b, &mut t, true, 5, 1000).is_empty());
        assert!(b.is_pressed());
        assert_eq!(run(&mut b, &mut t, false, 5, 1000), [Press::Short]);
        assert!(run(&mut b, &mut t, false, 50, 1000).is_empty());
    }

    #[test]
    fn long_press_fires_once_while_held() {
        let (mut b, mut t) = (Button::new(), 0);
        assert_eq!(run(&mut b, &mut t, true, 100, 1000), [Press::Long]);
        // Releasing doesn't add a short press.
        assert!(run(&mut b, &mut t, false, 10, 1000).is_empty());
        assert_eq!(run(&mut b, &mut t, true, 100, 1000), [Press::Long]);
    }

    #[test]
    fn the_restart_hold_is_longer() {
        let (mut b, mut t) = (Button::new(), 0);
        let hold = DEFAULT_BUTTONS[2].hold_ms();
        assert_eq!(hold, RESTART_HOLD_MS);
        assert!(run(&mut b, &mut t, true, 200, hold).is_empty());
        assert_eq!(run(&mut b, &mut t, true, 100, hold), [Press::Long]);
        assert_eq!(DEFAULT_BUTTONS[0].hold_ms(), LONG_PRESS_MS);
    }

    #[test]
    fn bounces_and_floating_inputs_do_nothing() {
        let (mut b, mut t) = (Button::new(), 0);
        // One-reading blips, alternating.
        for i in 0..50 {
            assert!(run(&mut b, &mut t, i % 2 == 0, 1, 1000).is_empty());
        }
        assert!(!b.is_pressed());
        // Nothing fitted: never pressed.
        let mut b = Button::new();
        assert!(run(&mut b, &mut t, false, 1000, 1000).is_empty());
    }

    #[test]
    fn a_bouncy_release_is_one_press() {
        let (mut b, mut t) = (Button::new(), 0);
        run(&mut b, &mut t, true, 5, 1000);
        let mut out = run(&mut b, &mut t, false, 1, 1000);
        out.extend(run(&mut b, &mut t, true, 1, 1000));
        out.extend(run(&mut b, &mut t, false, 5, 1000));
        assert_eq!(out, [Press::Short]);
    }

    #[test]
    fn led_patterns() {
        let idle = Inputs::default();
        assert_eq!(leds(0, idle), [false; 4]);
        let on = Inputs { deskflow_connected: true, ble_connected: true, activity: true, ..idle };
        assert_eq!(leds(0, on), [true, true, true, false]);
        assert_eq!(leds(BLINK_MS, on), [true, true, true, false]);
        let waiting = Inputs { deskflow_connecting: true, ble_pairing: true, attention: true, ..idle };
        assert_eq!(leds(0, waiting), [true, true, false, true]);
        assert_eq!(leds(BLINK_MS, waiting), [false, false, false, false]);
    }

    #[test]
    fn action_names_round_trip() {
        for a in Action::ALL {
            assert_eq!(Action::from_name(a.name()), Some(a));
        }
        assert_eq!(Action::from_name("explode"), None);
    }

    #[test]
    fn json() {
        let mut out = std::string::String::new();
        write_json(&mut out, [true, false, false, true], &DEFAULT_BUTTONS).unwrap();
        assert_eq!(out, r#"{"leds":[true,false,false,true],"hold_ms":[1000,1000,5000]}"#);
    }
}
