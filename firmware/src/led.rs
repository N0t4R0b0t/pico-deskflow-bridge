//! The Pico W's LED (wired to the CYW43439, not the RP2040) shows the
//! connection state; see `deskflow_proto::indicator` for the patterns.

use deskflow_proto::indicator::{Indicator, Link};
use embassy_time::{Duration, Instant, Ticker};

use crate::status;

const LED_GPIO: u8 = 0;

#[embassy_executor::task]
pub async fn task(mut control: cyw43::Control<'static>) -> ! {
    let mut indicator = Indicator::new();
    let mut lit = None;
    let mut seen = status::activity();
    let mut ticker = Ticker::every(Duration::from_millis(20));
    loop {
        let activity = status::activity();
        let link = Link::from(status::deskflow_connected(), status::ble_connected());
        let on = indicator.update(Instant::now().as_millis(), link, activity != seen);
        seen = activity;
        // Each change is a command over the chip's SPI bus; only send changes.
        if lit != Some(on) {
            control.gpio_set(LED_GPIO, on).await;
            lit = Some(on);
        }
        ticker.next().await;
    }
}
