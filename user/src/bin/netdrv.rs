//! `netdrv` (`/system/bin/netdrv`): the virtio-net userspace driver
//! (`docs/networking-plan.md`, stage N1; the NIC half of driver stage D5).
//!
//! The driver is an ordinary ring-3 program. It claims the virtio-net PCI
//! function through the device syscall (23), maps its BARs, allocates one DMA
//! block for both virtqueues and every packet slot, and drives the device with
//! the transport in `libs/virtio`, the wire definitions in `libs/virtio-net`
//! and the host-tested core in `libs/nicdrv`. It serves `os.lazy.net.nic.v1`
//! (`idl/net.midl`) under [`api::NAME`]: one client attaches two frame rings
//! and a notify endpoint, and the driver copies frames between those rings and
//! its own DMA slots, dropping and counting anything outside the frame-length
//! policy. The driver never parses a payload.
//!
//! Interrupts: the claim names the service endpoint, so the kernel's interrupt
//! messages and client calls arrive in the one receive loop. The line is shared
//! with the polled virtio-blk, so the claim opts in to sharing; a line that is
//! not routable (or `irq_mode=poll`) falls back to polling.
//!
//! Boot evidence (`demo=1`):
//!
//! 1. `NETDRV:CARD` describes the device;
//! 2. the driver broadcasts an ARP request for the gateway *through its own
//!    engine* and prints `NET:NIC:PASS` once the reply arrived, then
//!    `NET:IRQ:PASS delivered=N` (or `NETDRV:IRQ:POLLING`);
//! 3. it registers the service (`NETDRV:READY`) and spawns `nicctl`, real
//!    clients: info, an ARP exchange, a hostile-input probe and a soak.
//!
//! The host side (`tools/net/run.py`) captures the wire with QEMU's
//! `filter-dump`, so the markers alone are never the proof.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec;
use core::panic::PanicInfo;

use user::central;
use user::messenger::net::{self as api, wire};
use user::messenger::{self, errno, registry, services, Endpoint, Error as MsgError};
use user::sys;
use virtio_net::settings::Settings;

#[path = "netdrv/card.rs"]
mod card;
#[path = "netdrv/config.rs"]
mod config;
#[path = "netdrv/device.rs"]
mod device;
#[path = "netdrv/dma.rs"]
mod dma;
#[path = "netdrv/error.rs"]
mod error;
#[path = "netdrv/selftest.rs"]
mod selftest;
#[path = "netdrv/service.rs"]
mod service;

use card::Card;
use error::Error;
use service::Service;

/// The evidence clients `demo=1` runs, one after another, once the service is
/// up. See `user/src/bin/nicctl.rs`.
const DEMO_CLIENTS: [(&str, &[&str]); 4] = [
    (fhs::bin::NICCTL, &[]),
    (fhs::bin::NICCTL, &["arp"]),
    (fhs::bin::NICCTL, &["probe=1"]),
    (fhs::bin::NICCTL, &["soak=40"]),
];

/// Ticks the self-test waits for an interrupt message after its exchange.
const SETTLE_TICKS: u32 = 30;

/// The card name in the link topic (`system/net/{nic}/link`).
const CARD_NAME: &str = "virtio-net0";

/// Longest park in the serve loop, interrupts armed, nothing attached.
const IDLE_TICKS: u64 = 20;
/// Park while a client is attached and interrupts are armed.
const ATTACHED_TICKS: u64 = 2;

struct Args {
    /// Run the ARP self-test and then the evidence clients.
    demo: bool,
    /// Run the ARP self-test only (`netd` holds the one attachment, so no
    /// client can attach); implied by `demo`.
    selftest: bool,
    /// `irq=poll`: never arm the interrupt line, whatever `confd` says.
    poll: bool,
}

impl Args {
    fn from_service() -> Args {
        let mut buffer = [0u8; 128];
        let len = sys::service_args(&mut buffer).min(buffer.len());
        let text = core::str::from_utf8(&buffer[..len]).unwrap_or("");
        let has = |word: &str| text.split_whitespace().any(|part| part == word);
        Args {
            demo: has("demo=1"),
            selftest: has("demo=1") || has("selftest=1"),
            poll: has("irq=poll"),
        }
    }
}

/// `52:54:00:12:34:56`.
fn mac_text(mac: &[u8; 6]) -> String {
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    sys::write_str("netdrv: virtio-net driver\n");
    // The identity the kernel stamped on this task: `_net` with only
    // `CAP_DEV_CLAIM` under `init`, root when the kernel boots it directly.
    let mut cred = sys::Cred::default();
    match sys::cred_get(None, &mut cred) {
        Ok(()) => sys::write_str(&format!(
            "NETDRV:CRED uid={} caps={:#x}\n",
            cred.uid, cred.caps
        )),
        Err(errno) => sys::write_str(&format!("NETDRV:CRED unavailable (errno {errno})\n")),
    }
    let args = Args::from_service();
    match run(&args) {
        Ok(()) => sys::exit(0),
        Err(Error::NoDevice) => {
            sys::write_str("NETDRV:NODEV no virtio-net device on this machine\n");
            idle()
        }
        Err(error) => {
            sys::write_str(&format!("NET:NIC:FAIL {}\n", error.describe()));
            sys::exit(1)
        }
    }
}

/// With no device there is nothing to serve, and exiting would make a
/// supervisor restart a driver that can only fail the same way again: park
/// quietly instead.
fn idle() -> ! {
    loop {
        let _ = sys::wait(sys::clock() + 6000);
    }
}

fn run(args: &Args) -> Result<(), Error> {
    let (mut config, mut settings) = config::Config::load();
    if args.poll {
        settings.irq_mode = virtio_net::settings::IrqMode::Poll;
    }
    let fail = |error: MsgError| Error::Messenger(error.message());
    let (published, server) = messenger::create_pair().map_err(fail)?;
    let mut card = Card::open(&server, &settings)?;
    sys::write_str(&format!(
        "NETDRV:CARD mac={} link={} mtu={} rx_entries={} tx_entries={} irq={}\n",
        mac_text(&card.engine.mac()),
        card.engine.link(),
        card.mtu,
        card.queue_sizes.0,
        card.queue_sizes.1,
        if card.irq_armed() { "armed" } else { "polling" }
    ));
    if args.selftest {
        // Boot evidence (issue #481): `_net` cannot claim another class.
        user::dev::inspect::cross_class_probe("net");
        selftest_and_report(&mut card, &server);
    }
    registry::register(api::NAME, &published, &[api::INTERFACE], 0).map_err(fail)?;
    sys::write_str(&format!(
        "NETDRV:READY name={} interface={:#x}\n",
        api::NAME,
        api::INTERFACE
    ));
    serve(Service::new(card), &server, &mut config, settings, args)
}

/// Run the ARP self-test and print its verdict and the interrupt evidence.
fn selftest_and_report(card: &mut Card, server: &Endpoint) {
    match selftest::run(card, server) {
        Ok(()) => {}
        Err(error) => sys::write_str(&format!("NET:NIC:FAIL {}\n", error.describe())),
    }
    // Interrupt evidence: when the line was armed, the exchange must have
    // raised interrupts; an unroutable line just means polling alone. The
    // reply can be found by polling before its interrupt message has been
    // read, so give the message a few ticks to arrive.
    for _ in 0..SETTLE_TICKS {
        if !card.irq_armed() || card.engine.stats().interrupts > 0 {
            break;
        }
        wait_event(card, server);
    }
    let delivered = card.engine.stats().interrupts;
    match (card.irq_armed(), delivered) {
        (false, _) => {
            sys::write_str("NETDRV:IRQ:POLLING line not routable or disabled, polling only\n")
        }
        (true, 0) => sys::write_str("NET:IRQ:FAIL armed but no interrupt arrived\n"),
        (true, n) => sys::write_str(&format!("NET:IRQ:PASS delivered={n}\n")),
    }
}

/// Wait up to one tick on the service endpoint, handling an interrupt if that
/// is what arrived. Used while no client can be calling yet (the self-test).
fn wait_event(card: &mut Card, server: &Endpoint) {
    let mut buffer = [0u8; 256];
    if let Ok(message) = server.recv_with(&mut buffer, Some(sys::clock() + 1)) {
        if Card::is_interrupt(&message) {
            card.handle_interrupt();
        }
    }
}

/// Publish the retained link topic, connecting to the broker on first use.
fn publish_link(bus: &mut Option<central::Bus>, service: &Service) {
    if bus.is_none() {
        *bus = central::Bus::connect().ok();
    }
    let Some(bus) = bus.as_mut() else { return };
    let event = wire::LinkEvent {
        up: service.card.engine.link(),
        changes: service.card.engine.stats().link_changes,
    };
    let _ = wire::publish_system_net_link(bus, CARD_NAME, &event);
}

/// Serve `os.lazy.net.nic.v1` for the life of the driver.
fn serve(
    mut service: Service,
    server: &Endpoint,
    config: &mut config::Config,
    mut settings: Settings,
    args: &Args,
) -> Result<(), Error> {
    let fail = |error: MsgError| Error::Messenger(error.message());
    let mut bus = None;
    publish_link(&mut bus, &service);
    let mut published_changes = service.card.engine.stats().link_changes;
    let poll_ticks = u64::from(settings.poll_interval_ms).div_ceil(10).max(1);
    let irq = service.card.irq_armed();
    let mut next_refresh = sys::clock() + config::REFRESH_TICKS;
    let mut next_demo = if args.demo { 0 } else { DEMO_CLIENTS.len() };
    let mut demo_child: Option<u64> = None;
    // One receive buffer for the life of the service (per-call buffers of the
    // bump region are never reclaimed).
    let mut buffer = vec![0u8; messenger::DEFAULT_BUFFER];
    loop {
        let park = match (irq, service.busy()) {
            (true, true) => ATTACHED_TICKS,
            (true, false) => IDLE_TICKS,
            (false, _) => poll_ticks,
        };
        match server.recv_with(&mut buffer, Some(sys::clock() + park)) {
            Ok(message) if Card::is_interrupt(&message) => service.card.handle_interrupt(),
            Ok(message) => {
                let reply = service.dispatch(&message).unwrap_or_else(|error| {
                    services::error_reply(message.interface_id(), message.method(), error)
                });
                if let Some(txn) = message.txn {
                    server.reply_or_drop(txn, &reply).map_err(fail)?;
                }
            }
            Err(MsgError::Errno(code)) if code == -errno::ETIMEDOUT => {}
            Err(error) => return Err(fail(error)),
        }
        service.housekeeping()?;
        let changes = service.card.engine.stats().link_changes;
        if changes != published_changes {
            published_changes = changes;
            publish_link(&mut bus, &service);
        }
        if sys::clock() >= next_refresh {
            next_refresh = sys::clock() + config::REFRESH_TICKS;
            if let Some(mut new) = config.refresh() {
                // The command-line override outranks `confd`, so it never
                // looks like a setting change.
                if args.poll {
                    new.irq_mode = virtio_net::settings::IrqMode::Poll;
                }
                if settings.needs_restart(&new) {
                    sys::write_str("NETDRV:RESTART a restart-class setting changed\n");
                    return Ok(());
                }
                service
                    .card
                    .engine
                    .set_max_frame(usize::from(new.mtu) + virtio_net::ETH_HEADER);
                settings = new;
            }
        }
        run_demo(&mut demo_child, &mut next_demo);
    }
}

/// Start the evidence clients one at a time and reap them.
fn run_demo(child: &mut Option<u64>, next: &mut usize) {
    if child.is_some() {
        if let Some((pid, status)) = sys::wait(sys::clock()) {
            sys::write_str(&format!("NETDRV:DEMO:EXIT pid={pid} status={status}\n"));
            *child = None;
        }
    } else if *next < DEMO_CLIENTS.len() {
        let (program, args) = DEMO_CLIENTS[*next];
        let pid = sys::spawn_native(program, args);
        match pid {
            Some(pid) => sys::write_str(&format!("NETDRV:DEMO:SPAWN pid={pid}\n")),
            None => sys::write_str(&format!(
                "NETDRV:DEMO:SPAWN failed ({} missing?)\n",
                fhs::bin::NICCTL
            )),
        }
        *child = pid;
        // A client that cannot start ends the sequence: the harness then
        // reports the missing marker instead of waiting for a later one.
        *next = if pid.is_some() {
            *next + 1
        } else {
            DEMO_CLIENTS.len()
        };
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
