//! `ping` (`/system/bin/ping`): ICMP echo through the stack service
//! (`docs/networking-plan.md`, stage N2).
//!
//! Usage: `ping <a.b.c.d> [count]` (default 4 requests, one per second, 56
//! bytes of payload). Each request is one `Ping` call on
//! `os.lazy.net.stack.v1`; `netd` answers it when the reply arrives or the
//! two-second timeout passes. A host name is resolved first (`Resolve`, stage
//! N3), so the host is a name or a dotted quad.
//!
//! The system clock ticks at 100 Hz, so round trips read in multiples of 10 ms
//! (`time=0 ms` means under 10). Prints `PING:PASS sent=N received=N` when
//! every request was answered and `PING:FAIL ...` otherwise; what crossed the
//! wire is judged by the host from the packet capture.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use core::panic::PanicInfo;

use user::messenger::netstack::Client;
use user::messenger::Error as MsgError;
use user::sys;

const DEFAULT_COUNT: u32 = 4;
const MAX_COUNT: u32 = 1000;
const PAYLOAD: u32 = 56;
const TIMEOUT_MS: u32 = 2000;
/// Ticks between requests.
const INTERVAL_TICKS: u64 = 100;
/// Ticks to wait for `netd` to register at boot.
const CONNECT_TICKS: u64 = 500;

fn parse_ipv4(text: &str) -> Option<[u8; 4]> {
    let mut octets = [0u8; 4];
    let mut parts = text.split('.');
    for octet in &mut octets {
        let part = parts.next()?;
        if part.is_empty() || part.len() > 3 || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *octet = part.parse::<u16>().ok().filter(|n| *n <= 255)? as u8;
    }
    parts.next().is_none().then_some(octets)
}

fn connect() -> Result<Client, String> {
    let deadline = sys::clock() + CONNECT_TICKS;
    loop {
        match Client::connect() {
            Ok(client) => return Ok(client),
            Err(error) if sys::clock() >= deadline => {
                return Err(format!("no network stack: {}", error.message()))
            }
            Err(_) => {
                sys::nap();
            }
        }
    }
}

fn describe(error: &MsgError) -> String {
    match error {
        MsgError::Errno(code) if *code == -110 => String::from("request timed out"),
        MsgError::Errno(code) if *code == -101 => {
            String::from("network is unreachable (no address yet)")
        }
        other => String::from(other.message()),
    }
}

fn run() -> Result<(u32, u32), String> {
    let mut buffer = [0u8; 128];
    let len = sys::service_args(&mut buffer).min(buffer.len());
    let text = core::str::from_utf8(&buffer[..len]).unwrap_or("");
    let mut words = text.split_whitespace();
    let Some(host) = words.next() else {
        return Err(String::from("usage: ping <host> [count]"));
    };
    let client = connect()?;
    let dst = match parse_ipv4(host) {
        Some(addr) => addr,
        None => client
            .lookup_host(host, TIMEOUT_MS)
            .map_err(|error| format!("{host}: cannot resolve ({})", describe(&error)))?,
    };
    let count = words
        .next()
        .and_then(|c| c.parse().ok())
        .unwrap_or(DEFAULT_COUNT)
        .clamp(1, MAX_COUNT);

    sys::write_str(&format!("PING {host}: {PAYLOAD} data bytes\n"));
    let mut received = 0;
    for seq in 1..=count {
        let started = sys::clock();
        match client.ping(dst, PAYLOAD, TIMEOUT_MS) {
            Ok(reply) => {
                received += 1;
                let from: [u8; 4] = reply.source.as_slice().try_into().unwrap_or([0; 4]);
                sys::write_str(&format!(
                    "{} bytes from {}.{}.{}.{}: seq={seq} time={} ms\n",
                    reply.bytes, from[0], from[1], from[2], from[3], reply.rtt_ms
                ));
            }
            Err(error) => sys::write_str(&format!("seq={seq}: {}\n", describe(&error))),
        }
        if seq < count {
            let wait_until = started + INTERVAL_TICKS;
            while sys::clock() < wait_until {
                let _ = sys::wait(wait_until);
            }
        }
    }
    let lost = count - received;
    sys::write_str(&format!(
        "--- {host} ping statistics ---\n{count} packets transmitted, {received} received, {}% packet loss\n",
        lost * 100 / count
    ));
    Ok((count, received))
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    match run() {
        Ok((sent, received)) if sent == received => {
            sys::write_str(&format!("PING:PASS sent={sent} received={received}\n"));
            sys::exit(0)
        }
        Ok((sent, received)) => {
            sys::write_str(&format!("PING:FAIL sent={sent} received={received}\n"));
            sys::exit(1)
        }
        Err(message) => {
            sys::write_str(&format!("PING:FAIL {message}\n"));
            sys::exit(1)
        }
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
