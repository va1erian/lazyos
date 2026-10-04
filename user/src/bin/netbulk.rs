//! `netbulk` (`/system/bin/netbulk`): bulk TCP throughput over the native
//! socket service `os.lazy.net.socket.v1` (docs/performance-plan.md P0, P4).
//!
//! ```text
//! netbulk <a.b.c.d> <port> <bytes> [seed] [rounds]
//! ```
//!
//! The native twin of the Linux fixture `netbulk-linux`
//! (`tools/abi/fixtures/src/netbulk.rs`), against the same host server
//! (`tools/net/bulkpeers.py`): per round a `PUT` (the server checks every
//! byte) and a `GET` (checked here) of `bytes`. The stream is little-endian
//! words `j * MIX + seed`, so a lost, duplicated or reordered byte is a
//! mismatch at a known offset. Prints
//! `NETBULK:native:<PUT|GET>:PASS bytes=<n> ms=<t> mbps=<x>` (or
//! `:FAIL:<why>`), then `NETBULK:native:done`. Native programs have only the
//! 100 Hz tick, so `ms` is good to 10 ms: use transfers of seconds.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec;
use core::panic::PanicInfo;

use user::messenger::netsock::{Addr, Client, MAX_CHUNK};
use user::messenger::netstd::{is_timeout, parse_ipv4, TcpStream};
use user::sys;

const MIX: u64 = 0x9E37_79B9_7F4A_7C15;
/// Milliseconds a connect, a send or a receive may wait.
const WAIT_MS: u32 = 30_000;

fn word(j: u64, seed: u64) -> [u8; 8] {
    j.wrapping_mul(MIX).wrapping_add(seed).to_le_bytes()
}

/// Fill `out` with the stream's bytes from offset `at`.
fn fill(out: &mut [u8], at: u64, seed: u64) {
    let mut i = 0;
    while i < out.len() {
        let pos = at + i as u64;
        let bytes = word(pos / 8, seed);
        let off = (pos % 8) as usize;
        let take = (8 - off).min(out.len() - i);
        out[i..i + take].copy_from_slice(&bytes[off..off + take]);
        i += take;
    }
}

/// The offset of the first byte of `got` (stream bytes from `at`) that is
/// not the stream's.
fn mismatch(got: &[u8], at: u64, seed: u64) -> Option<u64> {
    let mut i = 0;
    while i < got.len() {
        let pos = at + i as u64;
        let bytes = word(pos / 8, seed);
        let off = (pos % 8) as usize;
        let take = (8 - off).min(got.len() - i);
        if got[i..i + take] != bytes[off..off + take] {
            return Some(pos);
        }
        i += take;
    }
    None
}

fn err(what: &str) -> impl Fn(user::messenger::Error) -> String + '_ {
    move |e| format!("{what}: {}", e.message())
}

/// Read until the peer closes; every byte goes to `sink`.
fn read_all(
    stream: &TcpStream,
    mut sink: impl FnMut(&[u8]) -> Result<(), String>,
) -> Result<(), String> {
    loop {
        match stream.read(MAX_CHUNK, WAIT_MS) {
            Ok(data) if data.is_empty() => return Ok(()),
            Ok(data) => sink(&data)?,
            Err(e) if is_timeout(&e) => return Err(String::from("receive timed out")),
            Err(e) => return Err(err("recv")(e)),
        }
    }
}

fn put(client: &Rc<Client>, to: Addr, bytes: u64, seed: u64) -> Result<u64, String> {
    let stream = TcpStream::connect(client, to, WAIT_MS).map_err(err("connect"))?;
    stream
        .write_all(format!("PUT {bytes} {seed}\n").as_bytes(), WAIT_MS)
        .map_err(err("header"))?;
    let start = sys::clock();
    let mut buf = vec![0u8; MAX_CHUNK];
    let mut sent = 0u64;
    while sent < bytes {
        let n = (bytes - sent).min(MAX_CHUNK as u64) as usize;
        fill(&mut buf[..n], sent, seed);
        stream.write_all(&buf[..n], WAIT_MS).map_err(err("send"))?;
        sent += n as u64;
    }
    stream.shutdown_write().map_err(err("shutdown"))?;
    let mut answer = String::new();
    read_all(&stream, |data| {
        answer.push_str(core::str::from_utf8(data).unwrap_or("?"));
        Ok(())
    })?;
    let ticks = sys::clock() - start;
    if !answer.starts_with(&format!("OK {bytes}")) {
        return Err(format!("server said {:?}", answer.trim()));
    }
    Ok(ticks)
}

fn get(client: &Rc<Client>, to: Addr, bytes: u64, seed: u64) -> Result<u64, String> {
    let stream = TcpStream::connect(client, to, WAIT_MS).map_err(err("connect"))?;
    stream
        .write_all(format!("GET {bytes} {seed}\n").as_bytes(), WAIT_MS)
        .map_err(err("header"))?;
    let start = sys::clock();
    let mut got = 0u64;
    read_all(&stream, |data| {
        if got + data.len() as u64 > bytes {
            return Err(format!("more than {bytes} bytes"));
        }
        if let Some(at) = mismatch(data, got, seed) {
            return Err(format!("byte {at} differs"));
        }
        got += data.len() as u64;
        Ok(())
    })?;
    if got != bytes {
        return Err(format!("received {got} of {bytes} bytes"));
    }
    Ok(sys::clock() - start)
}

fn print(kind: &str, bytes: u64, outcome: Result<u64, String>) -> bool {
    match outcome {
        Ok(ticks) => {
            let ms = ticks.max(1) * 10;
            let kbps = bytes / ms;
            sys::write_str(&format!(
                "NETBULK:native:{kind}:PASS bytes={bytes} ms={ms} mbps={}.{:02}\n",
                kbps / 1000,
                (kbps % 1000) / 10
            ));
            true
        }
        Err(why) => {
            sys::write_str(&format!("NETBULK:native:{kind}:FAIL:{why}\n"));
            false
        }
    }
}

fn parse() -> Option<(Addr, u64, u64, u32)> {
    let mut args = sys::args().skip(1);
    let ip = parse_ipv4(args.next()?)?;
    let port: u16 = args.next()?.parse().ok()?;
    let bytes: u64 = args.next()?.parse().ok()?;
    let seed: u64 = args.next().map_or(Some(1), |s| s.parse().ok())?;
    let rounds: u32 = args.next().map_or(Some(1), |s| s.parse().ok())?;
    Some((Addr::new(ip, port), bytes, seed, rounds))
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    let Some((to, bytes, seed, rounds)) = parse() else {
        sys::write_str("usage: netbulk <a.b.c.d> <port> <bytes> [seed] [rounds]\n");
        sys::write_str("NETBULK:native:FAIL:usage\n");
        sys::exit(2)
    };
    let client = match Client::connect() {
        Ok(client) => Rc::new(client),
        Err(e) => {
            sys::write_str(&format!(
                "NETBULK:native:FAIL:no socket service: {}\n",
                e.message()
            ));
            sys::exit(1)
        }
    };
    let mut ok = true;
    for round in 0..rounds {
        let seed = seed.wrapping_add(u64::from(round) * 2);
        ok &= print("PUT", bytes, put(&client, to, bytes, seed));
        ok &= print("GET", bytes, get(&client, to, bytes, seed + 1));
    }
    sys::write_str(&format!("NETBULK:native:done ok={ok}\n"));
    sys::exit(if ok { 0 } else { 1 })
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::write_str("NETBULK:native:FAIL:panic\n");
    sys::exit(1)
}
