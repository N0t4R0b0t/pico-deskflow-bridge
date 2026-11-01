//! What differs between the supported boards, selected by exactly one of
//! the `pico2-w` (RP2350A, the default) and `pico-w` (RP2040) features.
//! Both have the same CYW43439 radio on the same pins (23 power, 24 data,
//! 25 chip select, 29 clock) and use the same radio firmware.

#[cfg(all(feature = "pico-w", feature = "pico2-w"))]
compile_error!("enable only one board feature: `pico-w` or `pico2-w`");
#[cfg(not(any(feature = "pico-w", feature = "pico2-w")))]
compile_error!("enable a board feature: `pico2-w` (default) or `pico-w`");

use crate::storage::StorageFlash;

#[cfg(feature = "pico-w")]
mod imp {
    use embassy_rp::clocks::RoscRng;

    use super::StorageFlash;

    pub const NAME: &str = "Pico W";
    pub const FLASH_SIZE: usize = 2 * 1024 * 1024;
    /// Clipboard text kept for typing; the RP2040 has 264 KiB of RAM.
    pub const CLIPBOARD_MAX: usize = 16 * 1024;
    /// PBKDF2 iterations for the settings page password (Cortex-M0+).
    pub const PBKDF2_ROUNDS: u32 = 3_000;
    pub use cyw43_pio::DEFAULT_CLOCK_DIVIDER as CYW43_CLOCK_DIVIDER;

    /// The stock clock.
    pub fn config() -> embassy_rp::config::Config {
        Default::default()
    }

    /// Seed material from the ring oscillator (the RP2040 has no TRNG).
    pub fn entropy() -> [u8; 32] {
        let mut seed = [0u8; 32];
        RoscRng.fill_bytes(&mut seed);
        seed
    }

    /// The SPI flash chip's unique ID.
    pub fn chip_id(flash: &mut StorageFlash) -> [u8; 8] {
        let mut uid = [0u8; 8];
        flash.blocking_unique_id(&mut uid).unwrap();
        uid
    }
}

#[cfg(feature = "pico2-w")]
mod imp {
    use embassy_rp::Peri;
    use embassy_rp::clocks::{ClockConfig, CoreVoltage};
    use embassy_rp::interrupt::typelevel::{Binding, TRNG_IRQ};
    use embassy_rp::peripherals::TRNG;
    use embassy_rp::trng::{self, Trng};
    use fixed::FixedU32;
    use fixed::types::extra::U8;

    use super::StorageFlash;

    pub const NAME: &str = "Pico 2 W";
    pub const FLASH_SIZE: usize = 4 * 1024 * 1024;
    /// Clipboard text kept for typing (of 512 KiB of RAM).
    pub const CLIPBOARD_MAX: usize = 64 * 1024;
    /// PBKDF2 iterations for the settings page password (Cortex-M33).
    pub const PBKDF2_ROUNDS: u32 = 10_000;
    /// Above the stock 150 MHz, because software TLS on core 1 limits how
    /// fast mouse moves can arrive. embassy calls this experimental on the
    /// RP2350; its 200 MHz example uses the same voltage. To go back to
    /// stock, use `Default::default()` here and `RM2_CLOCK_DIVIDER` below.
    const SYSTEM_HZ: u32 = 200_000_000;

    pub fn config() -> embassy_rp::config::Config {
        let mut clocks = ClockConfig::system_freq(SYSTEM_HZ).unwrap();
        clocks.core_voltage = CoreVoltage::V1_15;
        embassy_rp::config::Config::new(clocks)
    }

    /// The radio's SPI at the same 25 MHz as `RM2_CLOCK_DIVIDER` at 150 MHz,
    /// which embassy's and trouble's Pico 2 W examples use: the radio doesn't
    /// run reliably much faster here.
    pub const CYW43_CLOCK_DIVIDER: FixedU32<U8> = FixedU32::from_bits(0x0400);

    /// Seed material from the RP2350's hardware TRNG.
    pub fn entropy(trng: Peri<'static, TRNG>, irqs: impl Binding<TRNG_IRQ, trng::InterruptHandler<TRNG>> + 'static) -> [u8; 32] {
        let mut seed = [0u8; 32];
        Trng::new(trng, irqs, trng::Config::default()).blocking_fill_bytes(&mut seed);
        seed
    }

    /// The random chip ID programmed into OTP at the factory.
    pub fn chip_id(_flash: &mut StorageFlash) -> [u8; 8] {
        match embassy_rp::otp::get_chipid() {
            Ok(id) => id.to_le_bytes(),
            Err(e) => {
                log::warn!("board: can't read the chip ID ({:?}); the BLE address won't be unique", e);
                [0x5a; 8]
            }
        }
    }
}

pub use imp::*;
