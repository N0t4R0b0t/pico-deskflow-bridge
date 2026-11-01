//! Firmware updates from the settings page (Pico 2 W only: the Pico W's 2 MiB
//! has no room for two images).
//!
//! The upload goes into the update slot; on the next boot the bootloader
//! swaps it in. The new firmware then has to confirm itself once it has
//! proven it works, or the following boot swaps the old one back. The
//! watchdog covers a firmware that hangs instead.

use deskflow_proto::uf2::{self, FAMILY_RP2350_ARM_S, PAGE_LEN, Reader};
use embassy_boot_rp::{AlignedBuffer, FirmwareUpdater, FirmwareUpdaterConfig, State};
use embassy_rp::Peri;
use embassy_rp::peripherals::WATCHDOG;
use embassy_rp::watchdog::Watchdog;
use embassy_time::{Duration, Instant, Ticker, Timer};
use embedded_storage_async::nor_flash::{NorFlash, ReadNorFlash};
use portable_atomic::{AtomicBool, Ordering};

use crate::status;
use crate::storage::{Area, SharedFlash};

const SECTOR: usize = 4096;
const FLASH_BASE: u32 = 0x1000_0000;
const RAM: core::ops::Range<u32> = 0x2000_0000..0x2008_0000;
const WATCHDOG_TIMEOUT: Duration = Duration::from_secs(8);
/// An update that hasn't proven itself by now is swapped back.
const CONFIRM_DEADLINE: Duration = Duration::from_secs(300);

/// `FirmwareUpdater` panics on a buffer of any other length. Async flash reads
/// are 4 bytes even though writes can be 1.
const STATE_WORD: usize = {
    let (read, write) = (<Area as ReadNorFlash>::READ_SIZE, <Area as NorFlash>::WRITE_SIZE);
    if read > write { read } else { write }
};
pub type StateBuffer = AlignedBuffer<STATE_WORD>;

pub const fn state_buffer() -> StateBuffer {
    AlignedBuffer([0; STATE_WORD])
}

static UPLOADING: AtomicBool = AtomicBool::new(false);

unsafe extern "C" {
    static __bootloader_state_start: u32;
    static __bootloader_state_end: u32;
    static __bootloader_active_start: u32;
    static __bootloader_active_end: u32;
    static __bootloader_dfu_start: u32;
    static __bootloader_dfu_end: u32;
}

/// A range the linker script defines as the addresses of two symbols.
fn area(flash: &'static SharedFlash, start: *const u32, end: *const u32) -> Area {
    Area::new(flash, start as u32, end as u32 - start as u32)
}

fn state_area(flash: &'static SharedFlash) -> Area {
    area(flash, &raw const __bootloader_state_start, &raw const __bootloader_state_end)
}

fn dfu_area(flash: &'static SharedFlash) -> Area {
    area(flash, &raw const __bootloader_dfu_start, &raw const __bootloader_dfu_end)
}

fn active() -> (u32, u32) {
    let start = &raw const __bootloader_active_start as u32;
    (FLASH_BASE + start, &raw const __bootloader_active_end as u32 - start)
}

/// An upload in progress. Only one at a time.
pub struct Upload<'b> {
    updater: FirmwareUpdater<'b, Area, Area>,
    reader: Reader,
    sector: [u8; SECTOR],
    /// The slot offset of `sector`, once it holds something.
    sector_at: Option<u32>,
    checked: bool,
}

impl Drop for Upload<'_> {
    fn drop(&mut self) {
        UPLOADING.store(false, Ordering::Relaxed);
    }
}

impl<'b> Upload<'b> {
    /// `None` if an upload is already running.
    pub fn start(flash: &'static SharedFlash, aligned: &'b mut StateBuffer) -> Option<Self> {
        if UPLOADING.swap(true, Ordering::Relaxed) {
            return None;
        }
        let (base, size) = active();
        let config = FirmwareUpdaterConfig { dfu: dfu_area(flash), state: state_area(flash) };
        Some(Upload {
            updater: FirmwareUpdater::new(config, &mut aligned.0),
            reader: Reader::new(base, size, FAMILY_RP2350_ARM_S),
            sector: [0xFF; SECTOR],
            sector_at: None,
            checked: false,
        })
    }

    /// Writes out the sector in hand. One at a time: erasing the whole slot
    /// up front would stall USB for many seconds.
    async fn flush(&mut self) -> Result<(), &'static str> {
        let Some(at) = self.sector_at.take() else { return Ok(()) };
        self.updater.write_firmware(at as usize, &self.sector).await.map_err(|e| {
            log::error!("update: writing failed: {:?}", e);
            "couldn't write flash"
        })?;
        self.sector = [0xFF; SECTOR];
        Ok(())
    }

    /// Takes the next bytes of the file.
    pub async fn feed(&mut self, mut bytes: &[u8]) -> Result<(), &'static str> {
        while !bytes.is_empty() {
            let n = self.reader.fill(bytes);
            bytes = &bytes[n..];
            let page = self.reader.take_page().map_err(uf2::Uf2Error::message)?;
            let Some(page) = page else { continue };
            let (offset, data): (u32, [u8; PAGE_LEN]) = (page.offset, *page.data);
            if !self.checked {
                let (base, size) = active();
                if offset != 0 || !uf2::looks_bootable(&data, base, size, RAM) {
                    return Err(uf2::Uf2Error::WrongImage.message());
                }
                self.checked = true;
            }
            let sector_at = offset - offset % SECTOR as u32;
            if self.sector_at.is_some_and(|at| at != sector_at) {
                self.flush().await?;
            }
            self.sector_at = Some(sector_at);
            let within = (offset - sector_at) as usize;
            self.sector[within..within + PAGE_LEN].copy_from_slice(&data);
        }
        Ok(())
    }

    /// Finishes the upload; the next boot swaps it in.
    pub async fn finish(&mut self) -> Result<(), &'static str> {
        if !self.checked {
            return Err(uf2::Uf2Error::WrongImage.message());
        }
        self.flush().await?;
        log::info!("update: {} KB received", self.reader.image_len() / 1024);
        self.updater.mark_updated().await.map_err(|e| {
            log::error!("update: couldn't mark the update: {:?}", e);
            "couldn't write flash"
        })
    }
}

/// Keeps the watchdog the bootloader started from firing, so only a hang does.
#[embassy_executor::task]
pub async fn watchdog_task(watchdog: Peri<'static, WATCHDOG>) -> ! {
    let mut watchdog = Watchdog::new(watchdog);
    watchdog.start(WATCHDOG_TIMEOUT);
    let mut ticker = Ticker::every(Duration::from_secs(2));
    loop {
        ticker.next().await;
        watchdog.feed(WATCHDOG_TIMEOUT);
    }
}

/// After an update: confirms it once the settings page has been used or
/// Deskflow connected, either of which proves the USB network and the firmware
/// work, or restarts into the old firmware.
#[embassy_executor::task]
pub async fn confirm_task(flash: &'static SharedFlash) {
    let mut aligned = state_buffer();
    let config = FirmwareUpdaterConfig { dfu: dfu_area(flash), state: state_area(flash) };
    let mut updater = FirmwareUpdater::new(config, &mut aligned.0);
    match updater.get_state().await {
        Ok(State::Swap) => {}
        Ok(_) => return,
        Err(e) => {
            log::error!("update: can't read the state: {:?}", e);
            return;
        }
    }
    log::info!("update: running a new firmware, not confirmed yet");
    let deadline = Instant::now() + CONFIRM_DEADLINE;
    while !status::deskflow_connected() && !status::web_served() {
        if Instant::now() > deadline {
            log::error!("update: not confirmed in time; going back");
            crate::usb::restart().await;
        }
        Timer::after_secs(1).await;
    }
    match updater.mark_booted().await {
        Ok(()) => log::info!("update: confirmed"),
        Err(e) => log::error!("update: couldn't confirm: {:?}", e),
    }
}
