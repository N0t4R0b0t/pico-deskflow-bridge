//! Composite USB device: a CDC-NCM network adapter (macOS, Linux and Windows
//! 11 drive it natively, so nothing gets installed on the Mac) plus a CDC-ACM
//! serial port carrying the firmware log.

use embassy_executor::Spawner;
use embassy_net::{Ipv4Cidr, Stack, StackResources, StaticConfigV4};
use embassy_rp::Peri;
use embassy_rp::interrupt::typelevel::Binding;
use embassy_rp::peripherals::USB;
use embassy_rp::usb::{Driver, InterruptHandler};
use embassy_time::Timer;
use embassy_usb::class::cdc_acm::{self, CdcAcmClass};
use embassy_usb::class::cdc_ncm::embassy_net::{Device, Runner, State as NetState};
use embassy_usb::class::cdc_ncm::{self, CdcNcmClass};
use embassy_usb::{Builder, Config, UsbDevice};
use static_cell::StaticCell;

use crate::config;

type UsbDriver = Driver<'static, USB>;

const MTU: usize = 1514;

#[embassy_executor::task]
async fn usb_task(mut device: UsbDevice<'static, UsbDriver>) -> ! {
    device.run().await
}

#[embassy_executor::task]
async fn ncm_task(runner: Runner<'static, UsbDriver, MTU>) -> ! {
    runner.run().await
}

#[embassy_executor::task]
async fn net_task(mut runner: embassy_net::Runner<'static, Device<'static, MTU>>) -> ! {
    runner.run().await
}

#[embassy_executor::task]
async fn log_task(class: CdcAcmClass<'static, UsbDriver>) -> ! {
    crate::logging::run_usb(class).await
}

/// Drops off the bus, as if unplugged. A CPU reset alone doesn't: the host
/// wouldn't notice the device restarting and would keep its network interface
/// and address, which no longer match.
fn disconnect() {
    embassy_rp::pac::USB.sie_ctrl().modify(|w| w.set_pullup_en(false));
}

/// Restarts the device, cleanly for the host.
pub async fn restart() -> ! {
    log::info!("restarting");
    // Let any response and the log line get out first.
    Timer::after_millis(300).await;
    disconnect();
    // Long enough for the host to see the device gone.
    Timer::after_millis(500).await;
    cortex_m::peripheral::SCB::sys_reset()
}

/// Brings up USB and the network stack; returns the stack once tasks run.
pub fn start(
    spawner: Spawner,
    usb: Peri<'static, USB>,
    irqs: impl Binding<embassy_rp::interrupt::typelevel::USBCTRL_IRQ, InterruptHandler<USB>> + 'static,
    seed: u64,
) -> Stack<'static> {
    let driver = Driver::new(usb, irqs);

    // pid.codes test VID/PID; fine for a personal device.
    let mut usb_config = Config::new(0x1209, 0x0001);
    usb_config.manufacturer = Some("pico-deskflow-bridge");
    usb_config.product = Some("Deskflow Bridge");
    usb_config.serial_number = Some("0001");
    usb_config.max_power = 100;
    usb_config.max_packet_size_0 = 64;
    // Two multi-interface functions need IADs.
    usb_config.composite_with_iads = true;
    usb_config.device_class = 0xEF;
    usb_config.device_sub_class = 0x02;
    usb_config.device_protocol = 0x01;

    static CONFIG_DESC: StaticCell<[u8; 256]> = StaticCell::new();
    static BOS_DESC: StaticCell<[u8; 256]> = StaticCell::new();
    static CONTROL_BUF: StaticCell<[u8; 128]> = StaticCell::new();
    let mut builder = Builder::new(
        driver,
        usb_config,
        &mut CONFIG_DESC.init([0; 256])[..],
        &mut BOS_DESC.init([0; 256])[..],
        &mut [],
        &mut CONTROL_BUF.init([0; 128])[..],
    );

    // MAC the Mac's network adapter reports, and ours on the other end.
    // Locally administered (bit 1 of the first octet set).
    let host_mac = [0x02, 0xDF, 0x00, 0x00, 0x00, 0x02];
    let our_mac = [0x02, 0xDF, 0x00, 0x00, 0x00, 0x01];

    static NCM_STATE: StaticCell<cdc_ncm::State> = StaticCell::new();
    let ncm = CdcNcmClass::new(&mut builder, NCM_STATE.init(cdc_ncm::State::new()), host_mac, 64);

    static ACM_STATE: StaticCell<cdc_acm::State> = StaticCell::new();
    let acm = CdcAcmClass::new(&mut builder, ACM_STATE.init(cdc_acm::State::new()), 64);

    spawner.spawn(usb_task(builder.build()).unwrap());
    spawner.spawn(log_task(acm).unwrap());

    static NET_STATE: StaticCell<NetState<MTU, 4, 4>> = StaticCell::new();
    let (ncm_runner, device) = ncm.into_embassy_net_device::<MTU, 4, 4>(NET_STATE.init(NetState::new()), our_mac);
    spawner.spawn(ncm_task(ncm_runner).unwrap());

    let net_config = embassy_net::Config::ipv4_static(StaticConfigV4 {
        address: Ipv4Cidr::new(config::PICO_IP, config::PREFIX_LEN),
        gateway: None,
        dns_servers: Default::default(),
    });
    // DHCP server, Deskflow client, the web servers, and one spare.
    static RESOURCES: StaticCell<StackResources<{ 3 + crate::web::SERVERS }>> = StaticCell::new();
    let (stack, runner) = embassy_net::new(device, net_config, RESOURCES.init(StackResources::new()), seed);
    spawner.spawn(net_task(runner).unwrap());
    stack
}
