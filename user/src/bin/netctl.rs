//! `netctl` (`NETCTL.ELF`): the network stack's control tool and evidence
//! client (`docs/networking-plan.md`, stage N2).
//!
//! Usage: `netctl [if | addr | route | stats | renew | reattach]`; with no
//! argument it prints everything. `renew` drops the DHCP lease and asks for a
//! new one; `reattach` rebuilds the stack's attachment to the NIC driver (a
//! recovery call, and how the soak exercises that path). The other modes are
//! the boot demo's evidence, each printing one marker:
//!
//! * `probe=1` sends malformed and out-of-contract requests to `netd`
//!   (`NETCTL:PROBE:PASS`);
//! * `soak=<n>` pings the gateway `n` times, renewing the lease and
//!   reattaching the NIC along the way, and checks the fabric snapshot and the
//!   counters for leaks (`NETCTL:SOAK:PASS`).
//!
//! What crossed the wire is judged by the host from the packet capture
//! (`tools/net/run.py --netd`); these markers only say the guest is done.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use core::panic::PanicInfo;

use user::messenger::netstack::{wire, Client};
use user::sys;

#[path = "netctl/common.rs"]
mod common;
#[path = "netctl/probe.rs"]
mod probe;
#[path = "netctl/soak.rs"]
mod soak;

use common::{connect, fail, ip_text, mac_text, wait_for_address};

enum Mode {
    Show,
    Interfaces,
    Addr,
    Route,
    Stats,
    Renew,
    Reattach,
    Probe,
    Soak(u32),
}

fn parse_args() -> Result<Mode, String> {
    let mut buffer = [0u8; 128];
    let len = sys::service_args(&mut buffer).min(buffer.len());
    let text = core::str::from_utf8(&buffer[..len]).unwrap_or("");
    let mut mode = Mode::Show;
    for part in text.split_whitespace() {
        mode = match part.split_once('=') {
            Some(("probe", "1")) => Mode::Probe,
            // Bounded so a hostile argument cannot hold the stack for minutes.
            Some(("soak", value)) => Mode::Soak(value.parse().unwrap_or(0).clamp(1, 200)),
            _ => match part {
                "if" | "interfaces" => Mode::Interfaces,
                "addr" | "address" => Mode::Addr,
                "route" => Mode::Route,
                "stats" => Mode::Stats,
                "renew" => Mode::Renew,
                "reattach" => Mode::Reattach,
                "-h" | "--help" | "help" => {
                    sys::write_str(
                        "usage: netctl [if | addr | route | stats | renew | reattach]\n",
                    );
                    sys::exit(0);
                }
                other => return Err(format!("unknown argument {other:?} (try `netctl help`)")),
            },
        };
    }
    Ok(mode)
}

fn show_interfaces(client: &Client) -> Result<(), String> {
    for i in client.interfaces().map_err(fail("interfaces"))? {
        let mode = if i.mode == wire::CONFIG_MODE_STATIC {
            "static"
        } else {
            "dhcp"
        };
        let dhcp = match i.dhcp {
            wire::DHCP_STATE_BOUND => "bound",
            wire::DHCP_STATE_DISCOVERING => "discovering",
            _ => "off",
        };
        sys::write_str(&format!(
            "{}: mac {} mtu {} link {} config {mode} dhcp {dhcp}\n",
            i.name,
            mac_text(&i.mac),
            i.mtu,
            if i.link { "up" } else { "down" }
        ));
    }
    Ok(())
}

fn show_addresses(client: &Client) -> Result<(), String> {
    let list = client.addresses().map_err(fail("addresses"))?;
    if list.is_empty() {
        sys::write_str("no address\n");
    }
    for a in list {
        let source = if a.source == wire::ADDR_SOURCE_STATIC {
            "static"
        } else {
            "dhcp"
        };
        sys::write_str(&format!(
            "{}: inet {}/{} ({source}, lease {} s)\n",
            a.interface,
            ip_text(&a.addr),
            a.prefix_len,
            a.lease_secs
        ));
    }
    Ok(())
}

fn show_routes(client: &Client) -> Result<(), String> {
    for r in client.routes().map_err(fail("routes"))? {
        let gateway = r.gateway.iter().any(|b| *b != 0);
        let dest = if r.prefix_len == 0 {
            String::from("default")
        } else {
            format!("{}/{}", ip_text(&r.dest), r.prefix_len)
        };
        if gateway {
            sys::write_str(&format!(
                "{dest} via {} dev {}\n",
                ip_text(&r.gateway),
                r.interface
            ));
        } else {
            sys::write_str(&format!("{dest} dev {} scope link\n", r.interface));
        }
    }
    Ok(())
}

fn show_stats(client: &Client) -> Result<(), String> {
    let s = client.stats().map_err(fail("stats"))?;
    sys::write_str(&format!(
        "rx       {} frames, {} bytes, {} bad lengths\ntx       {} frames, {} bytes, {} dropped\n",
        s.rx_frames, s.rx_bytes, s.rx_bad_length, s.tx_frames, s.tx_bytes, s.tx_dropped
    ));
    sys::write_str(&format!(
        "dhcp     {} leases, {} lost\nping     {} sent, {} answered, {} timed out\nnic      {} resets\n",
        s.leases, s.lease_losses, s.pings_sent, s.pings_answered, s.pings_timed_out, s.nic_resets
    ));
    Ok(())
}

fn run(mode: Mode) -> Result<(), String> {
    match mode {
        Mode::Probe => {
            return probe::run().map(|n| sys::write_str(&format!("NETCTL:PROBE:PASS checks={n}\n")))
        }
        Mode::Soak(n) => {
            return soak::run(n)
                .map(|n| sys::write_str(&format!("NETCTL:SOAK:PASS iterations={n}\n")))
        }
        _ => {}
    }
    let client = connect()?;
    match mode {
        Mode::Interfaces => show_interfaces(&client),
        Mode::Addr => show_addresses(&client),
        Mode::Route => show_routes(&client),
        Mode::Stats => show_stats(&client),
        Mode::Renew => {
            client.renew().map_err(fail("renew"))?;
            wait_for_address(&client)?;
            show_addresses(&client)
        }
        Mode::Reattach => client.reattach().map_err(fail("reattach")),
        Mode::Show => {
            show_interfaces(&client)?;
            show_addresses(&client)?;
            show_routes(&client)?;
            show_stats(&client)?;
            let addresses = client.addresses().map_err(fail("addresses"))?;
            let routes = client.routes().map_err(fail("routes"))?;
            let address = addresses
                .first()
                .map(|a| format!("{}/{}", ip_text(&a.addr), a.prefix_len))
                .unwrap_or_default();
            let gateway = routes
                .iter()
                .find(|r| r.prefix_len == 0)
                .map(|r| ip_text(&r.gateway))
                .unwrap_or_default();
            sys::write_str(&format!(
                "NETCTL:INFO:PASS addr={address} gateway={gateway}\n"
            ));
            Ok(())
        }
        Mode::Probe | Mode::Soak(_) => Ok(()),
    }
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    let outcome = parse_args().and_then(run);
    match outcome {
        Ok(()) => sys::exit(0),
        Err(message) => {
            sys::write_str(&format!("NETCTL:FAIL {message}\n"));
            sys::exit(1)
        }
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
