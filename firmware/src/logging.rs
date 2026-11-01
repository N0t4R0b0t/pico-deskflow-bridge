//! The logger: every record goes to the USB serial port and to a RAM ring
//! that the web page reads.

use core::cell::RefCell;
use core::fmt::Write as _;

use deskflow_proto::logring::LogRing;
use embassy_rp::usb::Driver;
use embassy_rp::peripherals::USB;
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_time::Instant;
use embassy_usb::class::cdc_acm::CdcAcmClass;
use embassy_usb_logger::{DummyHandler, UsbLogger};
use log::{LevelFilter, Log, Metadata, Record};

const LEVEL: LevelFilter = LevelFilter::Info;
const RING_SIZE: usize = 8 * 1024;

static RING: Mutex<CriticalSectionRawMutex, RefCell<LogRing<RING_SIZE>>> = Mutex::new(RefCell::new(LogRing::new()));
static USB_LOG: UsbLogger<1024, DummyHandler> = UsbLogger::with_custom_style(|record, w| {
    let _ = write!(w, "{}\r\n", record.args());
});

struct Tee;

impl Log for Tee {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= LEVEL
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        USB_LOG.log(record);

        let ms = Instant::now().as_millis();
        let mut line = heapless::String::<256>::new();
        let fits = writeln!(line, "[{:>5}.{:03} {:<5}] {}", ms / 1000, ms % 1000, record.level(), record.args()).is_ok();
        if !fits {
            // Keep what fit, and still end the line.
            line.truncate(line.len().min(252));
            let _ = line.push_str("…\n");
        }
        RING.lock(|ring| ring.borrow_mut().push(line.as_bytes()));
    }

    fn flush(&self) {}
}

static TEE: Tee = Tee;

/// Installs the logger. Call before anything logs.
pub fn init() {
    // SAFETY: called once, from `main` before any other task runs.
    unsafe {
        let _ = log::set_logger_racy(&TEE);
        log::set_max_level_racy(LEVEL);
    }
}

/// Drains the log into the USB serial port. Never returns.
pub async fn run_usb(class: CdcAcmClass<'static, Driver<'static, USB>>) -> ! {
    USB_LOG.create_future_from_class(class).await
}

/// Log bytes from position `since`; see [`LogRing::read`].
pub fn read(since: u32, out: &mut [u8]) -> (usize, u32) {
    RING.lock(|ring| ring.borrow().read(since, out))
}
