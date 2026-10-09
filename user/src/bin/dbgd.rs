//! `dbgd` (`/system/bin/dbgd`): the remote inspection service of a LazyOS
//! box (docs/dbgd-plan.md, issue #701).
//!
//! A PC with no serial port can be read over its network card: `dbgd`
//! listens on one TCP port and answers newline-delimited JSON-RPC from an
//! authenticated peer with the state the machine already has: the kernel
//! boot log (live), the task table, memory and fabric counters, the PCI
//! inventory and `devd`'s driver view, `usbd`'s controller snapshot, a few
//! allowlisted files and the `HW:*` verdicts. v1 is read-only.
//!
//! It is **off unless the image turns it on**: the binary is built into an
//! image only by `LAZYOS_DBGD`, runs only with `diag.dbg=1` in
//! `/boot/lazyos.cfg`, and refuses to run without a pre-shared key
//! (`diag.dbg.key`). The handshake, framing, method table and path allowlist
//! are `libs/dbgwire` (host-tested and fuzzed); this crate is the glue to
//! the system's data sources.
//!
//! Serial lines: `DBGD:OFF <why>`, `DBGD:READY port=<p> peer=<ip|any>`,
//! `DBGD:AUDIT ...` and `DBGD:SECURITY ...` (`audit.rs`).

#![no_std]
#![no_main]

extern crate alloc;

#[path = "dbgd/audit.rs"]
mod audit;
#[path = "dbgd/handlers.rs"]
mod handlers;
#[path = "dbgd/session.rs"]
mod session;

use alloc::format;
use alloc::rc::Rc;
use core::panic::PanicInfo;

use dbgwire::config::{self, Config, Refusal};
use user::messenger::netsock::Client;
use user::messenger::netstack::Client as Stack;
use user::messenger::netstd::{is_timeout, TcpListener};
use user::sys;

/// Ticks to wait for `netd` to register at boot.
const STACK_TICKS: u64 = 3000;
/// Milliseconds one `accept` waits.
const ACCEPT_MS: u32 = 1000;
/// Connections the listener queues.
const BACKLOG: u32 = 2;
/// Bytes of `lazyos.cfg` read (the kernel caps the file at 4 KiB).
const CFG_LIMIT: usize = 4096;

#[no_mangle]
pub extern "C" fn _start() -> ! {
    let config = match load_config() {
        Ok(config) => config,
        Err(Refusal::Disabled) => {
            sys::write_str("DBGD:OFF diag.dbg is not set in lazyos.cfg\n");
            sys::exit(0)
        }
        Err(why) => {
            // A bad line is a mistake to fix in the image, not a reason to
            // restart in a loop: say it and stay off.
            sys::write_str(&format!("DBGD:OFF {}\n", why.text()));
            sys::exit(0)
        }
    };
    sys::exit(serve(&config))
}

fn load_config() -> Result<Config, Refusal> {
    let bytes = user::files::read_up_to(fhs::boot::LAZYOS_CFG_PATH, CFG_LIMIT)
        .map_err(|_| Refusal::Disabled)?;
    config::parse(core::str::from_utf8(&bytes).map_err(|_| Refusal::Disabled)?)
}

/// Listen and serve; returns an exit status for `init`'s restart policy.
fn serve(config: &Config) -> u32 {
    let deadline = sys::clock() + STACK_TICKS;
    loop {
        if Stack::connect().is_ok() {
            break;
        }
        if sys::clock() >= deadline {
            sys::write_str("DBGD:FAIL no network stack\n");
            return 1;
        }
        sys::nap();
    }
    let sockets = match Client::connect() {
        Ok(client) => Rc::new(client),
        Err(error) => {
            sys::write_str(&format!("DBGD:FAIL sockets: {}\n", error.message()));
            return 1;
        }
    };
    let listener = match TcpListener::bind(&sockets, config.port, BACKLOG) {
        Ok(listener) => listener,
        Err(error) => {
            sys::write_str(&format!(
                "DBGD:FAIL listen on {}: {}\n",
                config.port,
                error.message()
            ));
            return 1;
        }
    };
    let peer = match config.peer {
        Some(ip) => format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]),
        None => alloc::string::String::from("any"),
    };
    sys::write_str(&format!("DBGD:READY port={} peer={peer}\n", config.port));
    let mut shared = session::Shared::new();
    loop {
        match listener.accept(ACCEPT_MS) {
            Ok((stream, from)) => session::run(&stream, from, config, &mut shared),
            Err(error) if is_timeout(&error) => {}
            Err(error) => {
                sys::write_str(&format!("DBGD:FAIL accept: {}\n", error.message()));
                return 1;
            }
        }
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::write_str("DBGD:FAIL panic\n");
    sys::exit(1)
}
