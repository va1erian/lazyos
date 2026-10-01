//! `nslookup` (`NSLOOKUP.ELF`): look a host name up through the stack service
//! (`docs/networking-plan.md`, stage N3).
//!
//! Usage: `nslookup <name>`. One `Resolve` call on `os.lazy.net.stack.v1`: `netd`
//! asks the resolver DHCP (or the static configuration) gave and answers when
//! the reply arrives. Prints the addresses found, then one marker:
//! `NSLOOKUP:PASS` (at least one address), `NSLOOKUP:NXDOMAIN` (the resolver
//! answered that the name has none: the exchange worked) or `NSLOOKUP:FAIL`.
//! What crossed the wire is judged by the host from the packet capture.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use core::panic::PanicInfo;

use user::messenger::netstack::Client;
use user::messenger::{errno, Error as MsgError};
use user::sys;

/// How long one lookup may take, milliseconds.
const TIMEOUT_MS: u32 = 5000;
/// Ticks to wait for `netd` to register at boot.
const CONNECT_TICKS: u64 = 500;
/// Ticks to wait for an address (and so a resolver) after boot.
const ADDRESS_TICKS: u64 = 1500;

fn connect() -> Result<Client, String> {
    let deadline = sys::clock() + CONNECT_TICKS;
    loop {
        match Client::connect() {
            Ok(client) => return Ok(client),
            Err(error) if sys::clock() >= deadline => {
                return Err(format!("no network stack: {}", error.message()))
            }
            Err(_) => {
                let _ = sys::wait(sys::clock() + 1);
            }
        }
    }
}

/// Wait for the stack to hold an address: a lookup before that is
/// `ENETUNREACH`, which at boot only means DHCP is not done yet.
fn wait_for_address(client: &Client) {
    let deadline = sys::clock() + ADDRESS_TICKS;
    while sys::clock() < deadline {
        if client.addresses().is_ok_and(|list| !list.is_empty()) {
            return;
        }
        let _ = sys::wait(sys::clock() + 1);
    }
}

enum Outcome {
    Found(usize),
    NoSuchName,
}

fn run() -> Result<Outcome, String> {
    let mut buffer = [0u8; 256];
    let len = sys::service_args(&mut buffer).min(buffer.len());
    let text = core::str::from_utf8(&buffer[..len]).unwrap_or("");
    let Some(name) = text.split_whitespace().next() else {
        return Err(String::from("usage: nslookup <name>"));
    };
    let client = connect()?;
    wait_for_address(&client);
    sys::write_str(&format!("Looking up {name}\n"));
    match client.resolve(name, TIMEOUT_MS) {
        Ok(addrs) => {
            for a in &addrs {
                sys::write_str(&format!(
                    "Name:    {name}\nAddress: {}.{}.{}.{}\n",
                    a[0], a[1], a[2], a[3]
                ));
            }
            Ok(Outcome::Found(addrs.len()))
        }
        Err(MsgError::Errno(code)) if code == -errno::ENOENT => {
            sys::write_str(&format!("** can't find {name}: NXDOMAIN\n"));
            Ok(Outcome::NoSuchName)
        }
        Err(MsgError::Errno(code)) if code == -errno::EINVAL => {
            Err(format!("{name}: not a host name"))
        }
        Err(MsgError::Errno(code)) if code == -errno::ETIMEDOUT => {
            Err(format!("{name}: the resolver did not answer"))
        }
        Err(MsgError::Errno(-101)) => Err(String::from("no resolver (no address yet?)")),
        Err(other) => Err(format!("{name}: {}", other.message())),
    }
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    match run() {
        Ok(Outcome::Found(n)) if n > 0 => {
            sys::write_str(&format!("NSLOOKUP:PASS addresses={n}\n"));
            sys::exit(0)
        }
        Ok(Outcome::Found(_)) | Ok(Outcome::NoSuchName) => {
            sys::write_str("NSLOOKUP:NXDOMAIN\n");
            sys::exit(1)
        }
        Err(message) => {
            sys::write_str(&format!("NSLOOKUP:FAIL {message}\n"));
            sys::exit(1)
        }
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
