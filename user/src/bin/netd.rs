//! `netd` (`NETD.ELF`): the network stack service
//! (`docs/networking-plan.md`, stage N2).
//!
//! `netd` runs smoltcp (through `libs/netstack`) in an ordinary ring-3 process
//! with **no capabilities**: it holds no device authority and no DMA, it is the
//! only client of the NIC driver (`os.lazy.net.nic.v1`), and it parses every
//! frame the network sends it, so a bug in a parser is a restart of this
//! process and never a compromise of the device. Under `init` it runs as the
//! `_netd` user.
//!
//! **One wait.** The loop has a single blocking call: `recv` on the service
//! endpoint with the deadline smoltcp's `poll_delay` gives. The NIC driver's
//! `Notify` messages are posted into that same endpoint (the notify endpoint
//! handed to it is a handle to this one), so client calls, NIC wake-ups and
//! timers all come through one place. A `Ping` call is parked and answered
//! when the result is in.
//!
//! **State** lives on retained topics (`system/net/eth0/addr`,
//! `system/events/network/up`); `confd` supplies the configuration under
//! `sys/net/eth0/` (DHCP by default) and is a soft dependency.
//!
//! Boot evidence (`demo=1`): `NETD:ADDR ...` when DHCP completes, then
//! `netctl`, `ping`, a hostile-input probe and a soak run as real clients. The
//! host side (`tools/net/run.py --netd`) judges the packet capture, never these
//! markers.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::vec;
use core::panic::PanicInfo;

use netstack::{RingDevice, Stack};
use user::messenger::net::wire as nic_wire;
use user::messenger::netstack::{self as api};
use user::messenger::{self, errno, registry, services, Error as MsgError};
use user::sys;

#[path = "netd/config.rs"]
mod config;
#[path = "netd/nic.rs"]
mod nic;
#[path = "netd/service.rs"]
mod service;

use nic::{Nic, DRIVER_INTERFACE};
use service::{Netd, IFNAME};

/// The registered service name.
const NAME: &str = api::NAME;

/// A locally administered placeholder until the NIC driver says what the
/// card's address is.
const PLACEHOLDER_MAC: [u8; 6] = [0x02, 0x4C, 0x5A, 0x00, 0x00, 0x01];

/// Longest park in the loop with nothing scheduled, ticks.
const IDLE_TICKS: u64 = 100;
/// Ticks the demo waits for an address before starting its clients anyway.
const DEMO_ADDRESS_TICKS: u64 = 2000;

/// The clients `demo=1` runs, one after another. See `netctl.rs` and `ping.rs`.
const DEMO_CLIENTS: [&[u8]; 4] = [
    b"NETCTL.ELF\0",
    b"PING.ELF 10.0.2.2 4\0",
    b"NETCTL.ELF probe=1\0",
    b"NETCTL.ELF soak=40\0",
];

struct Args {
    demo: bool,
}

impl Args {
    fn from_service() -> Args {
        let mut buffer = [0u8; 128];
        let len = sys::service_args(&mut buffer).min(buffer.len());
        let text = core::str::from_utf8(&buffer[..len]).unwrap_or("");
        Args {
            demo: text.split_whitespace().any(|part| part == "demo=1"),
        }
    }
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    sys::write_str("netd: network stack\n");
    // The identity the kernel stamped on this task: `_netd` with no
    // capabilities under `init`, root when the kernel boots it directly.
    let mut cred = sys::Cred::default();
    match sys::cred_get(None, &mut cred) {
        Ok(()) => sys::write_str(&format!(
            "NETD:CRED uid={} caps={:#x}\n",
            cred.uid, cred.caps
        )),
        Err(errno) => sys::write_str(&format!("NETD:CRED unavailable (errno {errno})\n")),
    }
    let args = Args::from_service();
    match run(&args) {
        Ok(()) => sys::exit(0),
        Err(message) => {
            sys::write_str(&format!("NETD:FAIL {message}\n"));
            sys::exit(1)
        }
    }
}

/// Eight random bytes for smoltcp's sequence numbers and transaction ids,
/// from the kernel CSPRNG (native syscall 26).
fn seed() -> Result<u64, alloc::string::String> {
    let mut bytes = [0u8; 8];
    sys::random(&mut bytes).map_err(|code| format!("no entropy (errno {})", -code))?;
    Ok(u64::from_le_bytes(bytes))
}

fn run(args: &Args) -> Result<(), alloc::string::String> {
    let (mut config, mut mode) = config::Config::load(IFNAME);
    let fail = |error: MsgError| alloc::string::String::from(error.message());
    let (published, server) = messenger::create_pair().map_err(fail)?;
    registry::register(NAME, &published, &[api::INTERFACE], 0).map_err(fail)?;
    sys::write_str(&format!(
        "NETD:READY name={NAME} interface={:#x}\n",
        api::INTERFACE
    ));

    let now_ms = sys::clock() as i64 * 10;
    let stack = Stack::new(
        RingDevice::detached(1514),
        PLACEHOLDER_MAC,
        seed()?,
        now_ms,
        &mode,
    );
    let mut netd = Netd::new(stack, Nic::new(), mode);
    let mut next_refresh = sys::clock() + config::REFRESH_TICKS;
    let started = sys::clock();
    let mut next_demo = if args.demo { 0 } else { DEMO_CLIENTS.len() };
    let mut demo_child: Option<u64> = None;
    let mut attach_errors = 0u32;
    // One receive buffer for the life of the service (per-call buffers of the
    // bump region are never reclaimed).
    let mut buffer = vec![0u8; messenger::DEFAULT_BUFFER];

    loop {
        let tick = sys::clock();
        let now_ms = tick as i64 * 10;

        // The NIC: rebuild the attachment when asked to, when the ring broke,
        // or when the driver went quiet; attach when it is time to.
        if netd.reattach {
            netd.reattach = false;
            netd.nic.detach(&mut netd.stack, "reattach requested");
            netd.nic_resets += 1;
        }
        if netd.nic.attached() && (netd.stack.device().is_poisoned() || netd.nic.silent(tick)) {
            let why = if netd.stack.device().is_poisoned() {
                "ring corrupt"
            } else {
                "driver silent"
            };
            netd.nic.detach(&mut netd.stack, why);
            netd.nic_resets += 1;
        }
        if netd.nic.should_try(tick) {
            match netd.nic.attach(&mut netd.stack, tick) {
                Ok(()) => {
                    attach_errors = 0;
                    if let Some(card) = netd.nic.card {
                        sys::write_str(&format!(
                            "NETD:NIC:ATTACHED mac={} mtu={} link={}\n",
                            mac_text(&card.mac),
                            card.mtu,
                            card.link
                        ));
                    }
                }
                Err(message) => {
                    // Say it once, then every tenth time: a missing driver is
                    // a normal state, not a stream of log lines.
                    if attach_errors.is_multiple_of(10) {
                        sys::write_str(&format!("NETD:NIC:WAIT {message}\n"));
                    }
                    attach_errors += 1;
                }
            }
        }

        // The work: the stack, the answers it produced, what it announces.
        netd.stack.poll(now_ms);
        netd.finish_pings(&server);
        netd.publish_if_changed();
        if netd.stack.device_mut().take_tx_notify() {
            netd.nic.kick(&mut netd.stack);
        }

        // Ask the driver to wake us, then look once more so a frame that
        // landed in between is not missed.
        netd.stack.device_mut().arm_rx();
        let pending = netd.stack.device_mut().rx_pending();

        if tick >= next_refresh {
            next_refresh = tick + config::REFRESH_TICKS;
            if let Some(new) = config.refresh() {
                if new != mode {
                    sys::write_str("NETD:RESTART the configuration changed\n");
                    return Ok(());
                }
                mode = new;
            }
        }
        run_demo(
            &netd,
            &mut demo_child,
            &mut next_demo,
            tick.saturating_sub(started) >= DEMO_ADDRESS_TICKS,
        );

        // The single wait.
        let wait = match netd.stack.poll_delay_ms(now_ms) {
            Some(ms) => ms.div_ceil(10).min(IDLE_TICKS),
            None => IDLE_TICKS,
        };
        let message = if pending || wait == 0 {
            server.poll_recv_with(&mut buffer).map_err(fail)?
        } else {
            match server.recv_with(&mut buffer, Some(tick + wait)) {
                Ok(message) => Some(message),
                Err(MsgError::Errno(code)) if code == -errno::ETIMEDOUT => None,
                Err(error) => return Err(fail(error)),
            }
        };
        let Some(message) = message else { continue };

        if message.interface_id() == DRIVER_INTERFACE {
            netd.nic.heard(sys::clock());
            // A link change is worth a look at the card; everything else in a
            // notice is only a reason to run the loop again.
            if message.method() == nic_wire::METHOD_NOTIFY {
                if let Ok(args) = nic_wire::decode_notify_args(&message.parcel.body) {
                    if args.events & (1 << nic_wire::NOTIFY_BIT_LINK_CHANGE) != 0 {
                        netd.nic.refresh_link();
                    }
                }
            }
            continue;
        }
        let reply = netd.dispatch(&message, sys::clock() as i64 * 10);
        // A one-way message has nobody to answer, and a parked call is
        // answered later.
        if let Some(txn) = message.txn {
            let reply = match reply {
                Ok(Some(reply)) => reply,
                Ok(None) => continue,
                Err(error) => {
                    services::error_reply(message.interface_id(), message.method(), error)
                }
            };
            server.reply_or_drop(txn, &reply).map_err(fail)?;
        }
    }
}

/// `52:54:00:12:34:56`.
fn mac_text(mac: &[u8; 6]) -> alloc::string::String {
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

/// Start the evidence clients one at a time once there is an address (or
/// after a generous wait) and reap them.
fn run_demo(netd: &Netd, child: &mut Option<u64>, next: &mut usize, waited_enough: bool) {
    if child.is_some() {
        if let Some((pid, status)) = sys::wait(sys::clock()) {
            sys::write_str(&format!("NETD:DEMO:EXIT pid={pid} status={status}\n"));
            *child = None;
        }
    } else if *next < DEMO_CLIENTS.len() && (netd.stack.state().addr.is_some() || waited_enough) {
        let pid = sys::spawn(DEMO_CLIENTS[*next]);
        match pid {
            Some(pid) => sys::write_str(&format!("NETD:DEMO:SPAWN pid={pid}\n")),
            None => sys::write_str("NETD:DEMO:SPAWN failed (client missing?)\n"),
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
