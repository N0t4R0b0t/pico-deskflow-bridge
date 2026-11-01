//! Pico 2 W / Pico W Deskflow bridge.
//!
//! ```text
//! Mac (Deskflow server) ──USB──▶ Pico (2) W ──BLE HID──▶ laptop
//!   CDC-NCM ethernet + CDC-ACM log          keyboard + absolute mouse
//! ```
//!
//! * `board`    — what differs between the Pico 2 W and the Pico W
//! * `usb`      — composite USB device: network adapter + serial log port
//! * `dhcp`     — hands the Mac its address on the USB link
//! * `deskflow` — TCP/TLS client; turns server messages into HID reports
//! * `ble`      — HID-over-GATT peripheral that sends those reports
//! * `flow`     — how many packets are still in the Bluetooth controller
//! * `clipboard` — types the server's clipboard on the laptop
//! * `web`      — configuration page with status, diagnostics and log
//! * `password` — the page's optional password
//! * `panel`    — the optional buttons and LEDs, and the page's virtual ones
//! * `led`      — connection state on the LED
//! * `storage`  — settings, BLE bond and TLS client key in flash
#![no_std]
#![no_main]

mod ble;
mod board;
mod clipboard;
mod config;
mod csprng;
mod deskflow;
mod dhcp;
mod flow;
mod led;
mod logging;
mod panel;
mod password;
mod status;
mod storage;
mod tls_offload;
#[cfg(feature = "pico2-w")]
mod update;
mod usb;
mod web;

use core::cell::Cell;
use core::mem::MaybeUninit;

use cyw43_pio::PioSpi;
use deskflow_proto::hid::Report;
use deskflow_proto::settings::Settings;
use embassy_executor::{Executor, Spawner};
use embassy_rp::flash::Flash;
use embassy_rp::gpio::{Level, Output};
use embassy_rp::multicore::{Stack, spawn_core1};
use embassy_rp::peripherals::{DMA_CH0, DMA_CH1, DMA_CH2, FLASH, PIO0, USB};
use embassy_rp::pio::Pio;
use embassy_rp::{bind_interrupts, dma, pio};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;
use embassy_time::Instant;
use embedded_alloc::LlffHeap;
use panic_halt as _;
use rand_chacha::rand_core::RngCore as _;
use static_cell::StaticCell;

bind_interrupts!(struct Irqs {
    PIO0_IRQ_0 => pio::InterruptHandler<PIO0>;
    USBCTRL_IRQ => embassy_rp::usb::InterruptHandler<USB>;
    DMA_IRQ_0 => dma::InterruptHandler<DMA_CH0>, dma::InterruptHandler<DMA_CH1>, dma::InterruptHandler<DMA_CH2>;
    #[cfg(feature = "pico2-w")]
    TRNG_IRQ => embassy_rp::trng::InterruptHandler<embassy_rp::peripherals::TRNG>;
});

/// HID reports from the Deskflow client to the BLE task. Keyboard reports
/// must not be dropped; mouse moves may be when the queue is full.
pub static REPORTS: Channel<CriticalSectionRawMutex, Outgoing, 32> = Channel::new();

/// The newest pointer report waiting for the radio, which is slower than
/// Deskflow's moves. It goes out after [`REPORTS`], so it's always the latest.
pub static MOTION: BlockingMutex<CriticalSectionRawMutex, Cell<Option<Outgoing>>> = BlockingMutex::new(Cell::new(None));
pub static MOTION_READY: Signal<CriticalSectionRawMutex, ()> = Signal::new();

#[derive(Clone, Copy)]
pub struct Outgoing {
    pub report: Report,
    /// When the input it came from arrived, for the latency diagnostics.
    /// `None` for typed clipboard text, which waits in the queue on purpose.
    pub since: Option<Instant>,
}

impl Outgoing {
    pub fn input(report: Report) -> Self {
        Outgoing { report, since: Some(Instant::now()) }
    }

    pub fn typed(report: Report) -> Self {
        Outgoing { report, since: None }
    }
}

/// Only RSA signature checks in the TLS handshake allocate (bignums); this
/// is ample for a 4096-bit server key.
#[global_allocator]
pub static HEAP: LlffHeap = LlffHeap::empty();
pub const HEAP_SIZE: usize = 32 * 1024;

/// Core 1 runs TLS only (see `tls_offload`). Its stack is a plain static
/// (flip-link only guards core 0's), so it's generous: the deepest path is
/// building the client certificate.
pub const CORE1_STACK_SIZE: usize = 32 * 1024;
pub static mut CORE1_STACK: Stack<CORE1_STACK_SIZE> = Stack::new();
static EXECUTOR1: StaticCell<Executor> = StaticCell::new();

#[embassy_executor::task]
async fn cyw43_task(runner: cyw43::Runner<'static, cyw43::SpiBus<Output<'static>, PioSpi<'static, PIO0, 0>>, cyw43::Cyw43439>) -> ! {
    runner.run().await
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    {
        static mut HEAP_MEM: [MaybeUninit<u8>; HEAP_SIZE] = [MaybeUninit::uninit(); HEAP_SIZE];
        // SAFETY: runs once, before anything allocates.
        unsafe { HEAP.init(&raw mut HEAP_MEM as usize, HEAP_SIZE) }
    }
    logging::init();
    let p = embassy_rp::init(board::config());

    #[cfg(feature = "pico-w")]
    let seed = board::entropy();
    #[cfg(feature = "pico2-w")]
    let seed = board::entropy(p.TRNG, Irqs);
    csprng::seed(seed);
    #[cfg(feature = "pico2-w")]
    spawner.spawn(update::watchdog_task(p.WATCHDOG).unwrap());

    // SAFETY: the only reference ever taken to CORE1_STACK.
    let core1_stack = unsafe { &mut *core::ptr::addr_of_mut!(CORE1_STACK) };
    spawn_core1(p.CORE1, core1_stack, move || {
        let executor = EXECUTOR1.init(Executor::new());
        executor.run(|spawner| spawner.spawn(tls_offload::core1_task().unwrap()))
    });

    // USB first so the log port appears as early as possible.
    let stack = usb::start(spawner, p.USB, Irqs, csprng::Rng.next_u64());
    spawner.spawn(dhcp::server_task(stack).unwrap());
    log::info!(
        "board: {}, {} MHz, firmware {}",
        board::NAME,
        embassy_rp::clocks::clk_sys_freq() / 1_000_000,
        env!("BUILD_ID")
    );

    let mut flash = Flash::<FLASH, _, { board::FLASH_SIZE }>::new(p.FLASH, p.DMA_CH2, Irqs);
    // A stable identity address, so the laptop recognises us after a reboot.
    let uid = board::chip_id(&mut flash);
    static FLASH_CELL: StaticCell<storage::SharedFlash> = StaticCell::new();
    let flash = FLASH_CELL.init(embassy_sync::mutex::Mutex::new(flash));
    let mut storage = storage::new(flash);

    let pins = panel::Pins::new(
        (p.PIN_13, p.PIN_14, p.PIN_15),
        (p.PIN_16, p.PIN_17, p.PIN_18, p.PIN_19),
    );
    if pins.factory_reset_held().await {
        log::warn!("factory reset: clearing settings, pairings and the password");
        if storage::factory_reset(&mut storage).await.is_err() {
            log::error!("factory reset failed");
        }
    }

    static SETTINGS: StaticCell<Settings> = StaticCell::new();
    let settings = SETTINGS.init(storage::load_settings(&mut storage, config::defaults()).await);
    log::info!(
        "settings: screen '{}' {}x{}, bluetooth name '{}'",
        settings.screen_name,
        settings.screen_width,
        settings.screen_height,
        settings.ble_name
    );

    static STORAGE: StaticCell<storage::SharedStorage> = StaticCell::new();
    let storage = STORAGE.init(storage::SharedStorage::new(storage));
    // The client creates its TLS identity itself, once USB is up: the
    // crypto blocks the executor long enough to make enumeration time out.
    spawner.spawn(deskflow::client_task(stack, settings, storage).unwrap());
    spawner.spawn(clipboard::typing_task(settings).unwrap());

    spawner.spawn(panel::task(pins, settings).unwrap());
    password::load(storage).await;
    spawner.spawn(password::bootsel_task(p.BOOTSEL, storage).unwrap());
    static WEB: StaticCell<web::Context> = StaticCell::new();
    let web = WEB.init(web::Context { settings, storage, flash });
    #[cfg(feature = "pico2-w")]
    spawner.spawn(update::confirm_task(flash).unwrap());
    for _ in 0..web::SERVERS {
        spawner.spawn(web::server_task(stack, web).unwrap());
    }
    log::info!("web: settings page at http://{}/", config::PICO_IP);

    let fw = cyw43::aligned_bytes!("../cyw43-firmware/43439A0.bin");
    let clm = cyw43::aligned_bytes!("../cyw43-firmware/43439A0_clm.bin");
    let btfw = cyw43::aligned_bytes!("../cyw43-firmware/43439A0_btfw.bin");
    let nvram = cyw43::aligned_bytes!("../cyw43-firmware/nvram_rp2040.bin");

    let pwr = Output::new(p.PIN_23, Level::Low);
    let cs = Output::new(p.PIN_25, Level::High);
    let mut pio = Pio::new(p.PIO0, Irqs);
    let spi = PioSpi::new(
        &mut pio.common,
        pio.sm0,
        board::CYW43_CLOCK_DIVIDER,
        pio.irq0,
        cs,
        p.PIN_24,
        p.PIN_29,
        dma::Channel::new(p.DMA_CH0, Irqs),
        dma::Channel::new(p.DMA_CH1, Irqs),
    );

    static STATE: StaticCell<cyw43::State> = StaticCell::new();
    let (_net, bt_device, mut control, runner) =
        cyw43::new_with_bluetooth(STATE.init(cyw43::State::new()), pwr, spi, fw, btfw, nvram).await;
    spawner.spawn(cyw43_task(runner).unwrap());
    control.init(clm).await;
    log::info!("cyw43 up");
    spawner.spawn(led::task(control).unwrap());

    ble::run(bt_device, storage, &settings.ble_name, uid).await
}
