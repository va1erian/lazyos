//! `nicctl` (`NICCTL.ELF`): the NIC driver's control tool and evidence client.
//!
//! With no arguments it prints the card's MAC, MTU, link state and counters
//! (the stage D5 demo). The other modes exercise the driver the way the boot
//! demo's evidence needs, each printing one marker:
//!
//! * `arp` attaches rings, broadcasts an ARP request for the gateway and waits
//!   for the reply (`NICCTL:ARP:PASS`): a real client, real frames;
//! * `probe=1` sends malformed and hostile requests and oversize and undersize
//!   frames (`NICCTL:PROBE:PASS`); `role=intruder ring=<n>` is its second
//!   task, refused on the owner's ring (`NICCTL:INTRUDER:PASS`);
//! * `soak=<n>` runs `n` attach/exchange/detach cycles and checks the fabric
//!   snapshot for leaks (`NICCTL:SOAK:PASS`).
//!
//! What crossed the wire is judged by the host from QEMU's packet capture
//! (`tools/net/run.py`); these markers only say the guest is done.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use core::panic::PanicInfo;

use user::sys;

#[path = "nicctl/common.rs"]
mod common;
#[path = "nicctl/probe.rs"]
mod probe;
#[path = "nicctl/soak.rs"]
mod soak;

use common::{arp_exchange, connect, fail, mac_text, GATEWAY_IP};

/// What to do.
enum Mode {
    Show,
    Arp,
    Probe,
    Intruder(u32),
    Soak(u32),
}

fn parse_args() -> Mode {
    let mut buffer = [0u8; 128];
    let len = sys::service_args(&mut buffer).min(buffer.len());
    let text = core::str::from_utf8(&buffer[..len]).unwrap_or("");
    let mut mode = Mode::Show;
    let mut ring = 0;
    for part in text.split_whitespace() {
        match part.split_once('=') {
            Some(("probe", "1")) => mode = Mode::Probe,
            Some(("role", "intruder")) => mode = Mode::Intruder(0),
            Some(("ring", value)) => ring = value.parse().unwrap_or(0),
            // Bounded so a hostile argument cannot hold the card for minutes.
            Some(("soak", value)) => mode = Mode::Soak(value.parse().unwrap_or(0).clamp(1, 200)),
            _ if part == "arp" => mode = Mode::Arp,
            _ if matches!(part, "-h" | "--help" | "help") => {
                sys::write_str(
                    "usage: nicctl [arp | probe=1 | soak=<n>]   (no argument: show the card)\n",
                );
                sys::exit(0);
            }
            _ => {}
        }
    }
    if let Mode::Intruder(_) = mode {
        mode = Mode::Intruder(ring);
    }
    mode
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    let outcome = match parse_args() {
        Mode::Show => show(),
        Mode::Arp => arp(),
        Mode::Probe => probe::run()
            .map(|checks| sys::write_str(&format!("NICCTL:PROBE:PASS checks={checks}\n"))),
        Mode::Intruder(ring) => probe::run_intruder_role(ring)
            .map(|checks| sys::write_str(&format!("NICCTL:INTRUDER:PASS checks={checks}\n"))),
        Mode::Soak(n) => soak::run(n)
            .map(|done| sys::write_str(&format!("NICCTL:SOAK:PASS iterations={done}\n"))),
    };
    match outcome {
        Ok(()) => sys::exit(0),
        Err(message) => {
            sys::write_str(&format!("NICCTL:FAIL {message}\n"));
            sys::exit(1)
        }
    }
}

/// Print the card and its counters.
fn show() -> Result<(), alloc::string::String> {
    let client = connect()?;
    let info = client.info().map_err(fail("info"))?;
    let stats = client.stats().map_err(fail("stats"))?;
    sys::write_str(&format!(
        "mac      {}\nmtu      {} (largest frame {})\nlink     {}\nfeatures {:#x}\n",
        mac_text(&info.mac),
        info.mtu,
        info.max_frame,
        if info.link { "up" } else { "down" },
        info.features
    ));
    sys::write_str(&format!(
        "rx       {} frames, {} bytes, {} dropped\ntx       {} frames, {} bytes, {} dropped\n",
        stats.rx_frames,
        stats.rx_bytes,
        stats.rx_dropped,
        stats.tx_frames,
        stats.tx_bytes,
        stats.tx_dropped
    ));
    sys::write_str(&format!(
        "errors   {} runts, {} oversize, {} ring errors\nirq      {} interrupts, {} link changes\n",
        stats.runts, stats.oversize, stats.ring_errors, stats.interrupts, stats.link_changes
    ));
    sys::write_str(&format!(
        "NICCTL:INFO:PASS mac={} link={}\n",
        mac_text(&info.mac),
        info.link
    ));
    Ok(())
}

/// One ARP exchange with the gateway through a real attachment.
fn arp() -> Result<(), alloc::string::String> {
    let client = connect()?;
    let info = client.info().map_err(fail("info"))?;
    let mac: [u8; 6] = info
        .mac
        .as_slice()
        .try_into()
        .map_err(|_| alloc::string::String::from("bad MAC"))?;
    let mut attachment = client.attach(16).map_err(fail("attach"))?;
    let answered = arp_exchange(&client, &mut attachment, mac, GATEWAY_IP);
    let _ = client.detach(attachment.ring);
    attachment.close();
    let from = answered?;
    sys::write_str(&format!(
        "NICCTL:ARP:PASS gateway={} mac={}\n",
        "10.0.2.2",
        mac_text(&from)
    ));
    Ok(())
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
