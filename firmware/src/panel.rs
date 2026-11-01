//! The optional buttons and LEDs, and the settings page's virtual ones: GPIO
//! glue around `deskflow_proto::panel`. Default wiring:
//!
//! ```text
//! GP13, GP14, GP15   button to GND (internal pull-ups, so nothing fitted
//!                    reads as not pressed)
//! GP16..GP19         330 ohm, LED, GND: Deskflow, Bluetooth, Activity, Attention
//! ```

use deskflow_proto::hid::Report;
use deskflow_proto::pairing::Command;
use deskflow_proto::panel::{self, Action, BUTTONS, Button, ButtonActions, Inputs, LEDS, Press};
use deskflow_proto::settings::Settings;
use deskflow_proto::typing::RELEASE;
use deskflow_proto::web::DeskflowState;
use embassy_futures::select::select;
use embassy_rp::Peri;
use embassy_rp::gpio::{Input, Level, Output, Pin, Pull};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Instant, Ticker, Timer};

use crate::{Outgoing, REPORTS, ble, clipboard, status, usb};

const TICK: Duration = Duration::from_millis(20);
/// How long the Activity LED stays on after input.
const ACTIVITY_MS: u64 = 80;
/// How long buttons 1 and 3 must be held at power-on for a factory reset.
const FACTORY_RESET_HOLD: Duration = Duration::from_secs(3);
/// Between pressing and releasing a key the bridge sends itself.
const KEY_MS: u64 = 30;
/// HID usage of L, and the left GUI modifier bit.
const USAGE_L: u8 = 0x0F;
const MOD_LEFT_GUI: u8 = 0x08;

/// Presses of the page's virtual buttons.
pub static VIRTUAL_PRESSES: Channel<CriticalSectionRawMutex, (u8, Press), 4> = Channel::new();

pub struct Pins {
    buttons: [Input<'static>; BUTTONS],
    leds: [Output<'static>; LEDS],
}

fn button(pin: Peri<'static, impl Pin>) -> Input<'static> {
    Input::new(pin, Pull::Up)
}

fn led(pin: Peri<'static, impl Pin>) -> Output<'static> {
    Output::new(pin, Level::Low)
}

impl Pins {
    pub fn new(
        buttons: (Peri<'static, impl Pin>, Peri<'static, impl Pin>, Peri<'static, impl Pin>),
        leds: (Peri<'static, impl Pin>, Peri<'static, impl Pin>, Peri<'static, impl Pin>, Peri<'static, impl Pin>),
    ) -> Self {
        Pins {
            buttons: [button(buttons.0), button(buttons.1), button(buttons.2)],
            leds: [led(leds.0), led(leds.1), led(leds.2), led(leds.3)],
        }
    }

    /// Whether buttons 1 and 3 stay held, which asks for a factory reset. An
    /// accident needs two buttons held from power-on for seconds.
    pub async fn factory_reset_held(&self) -> bool {
        // Let the pull-ups settle.
        Timer::after_millis(5).await;
        let deadline = Instant::now() + FACTORY_RESET_HOLD;
        while Instant::now() < deadline {
            if self.buttons[0].is_high() || self.buttons[2].is_high() {
                return false;
            }
            Timer::after_millis(20).await;
        }
        true
    }
}

#[embassy_executor::task]
pub async fn task(mut pins: Pins, settings: &'static Settings) -> ! {
    let mut buttons = [Button::new(); BUTTONS];
    let mut ticker = Ticker::every(TICK);
    let mut seen = status::activity();
    let mut active_at = 0;
    loop {
        if let embassy_futures::select::Either::Second((index, press)) =
            select(ticker.next(), VIRTUAL_PRESSES.receive()).await
            && let Some(&actions) = settings.buttons.get(index as usize)
        {
            run(actions, press).await;
        }
        let now = Instant::now().as_millis();
        for (i, button) in buttons.iter_mut().enumerate() {
            let actions = settings.buttons[i];
            if let Some(press) = button.update(now, pins.buttons[i].is_low(), actions.hold_ms()) {
                run(actions, press).await;
            }
        }

        let activity = status::activity();
        if activity != seen {
            seen = activity;
            active_at = now;
        }
        let deskflow = status::deskflow();
        let leds = panel::leds(
            now,
            Inputs {
                deskflow_connected: status::deskflow_connected(),
                deskflow_connecting: deskflow == DeskflowState::Connecting,
                ble_connected: status::ble_connected(),
                ble_pairing: status::ble_pairing(),
                activity: active_at != 0 && now - active_at < ACTIVITY_MS,
                attention: status::attention(),
            },
        );
        for (pin, on) in pins.leds.iter_mut().zip(leds) {
            pin.set_level(on.into());
        }
        status::set_leds(leds);
    }
}

async fn run(actions: ButtonActions, press: Press) {
    let action = match press {
        Press::Short => actions.short,
        Press::Long => actions.long,
    };
    log::info!("panel: {}", action.name());
    if let Some(usage) = action.consumer_usage() {
        REPORTS.send(Outgoing::typed(Report::Consumer(usage.to_le_bytes()))).await;
        Timer::after_millis(KEY_MS).await;
        REPORTS.send(Outgoing::typed(Report::Consumer([0; 2]))).await;
        return;
    }
    match action {
        Action::TypeClipboard => clipboard::request_typing(),
        Action::StopTyping => clipboard::stop_typing(),
        Action::NextLaptop => {
            let _ = ble::COMMANDS.try_send(Command::Next);
        }
        Action::ReleaseAllKeys => {
            REPORTS.send(Outgoing::typed(RELEASE)).await;
            REPORTS.send(Outgoing::typed(Report::Consumer([0; 2]))).await;
        }
        Action::LockLaptop => {
            let press = Report::Keyboard([MOD_LEFT_GUI, 0, USAGE_L, 0, 0, 0, 0, 0]);
            REPORTS.send(Outgoing::typed(press)).await;
            Timer::after_millis(KEY_MS).await;
            REPORTS.send(Outgoing::typed(RELEASE)).await;
        }
        Action::Restart => usb::restart().await,
        Action::None | Action::PlayPause | Action::Mute => {}
    }
}
