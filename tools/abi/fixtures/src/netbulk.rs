//! `netbulk` — bulk TCP throughput over the Linux ABI's `AF_INET` shim
//! (docs/performance-plan.md P0 and P4).
//!
//! ```text
//! netbulk-linux <a.b.c.d> <port> <bytes> [seed] [rounds]
//! ```
//!
//! Talks to the host's bulk server (`tools/net/bulkpeers.py`, run by
//! `tools/net/bulk.py`): each round connects twice, once to send `bytes`
//! (`PUT`, the server checks every byte and answers `OK`) and once to receive
//! them (`GET`, checked here). The stream is [`word`]s of a counter mixed with
//! the seed, so a lost, duplicated or reordered byte is a mismatch at a known
//! offset. Prints, per transfer,
//! `NETBULK:linux:<PUT|GET>:PASS bytes=<n> ms=<t> mbps=<x> connect_us=<c>`
//! (or `:FAIL:<why>`) and finally `NETBULK:linux:done`.

use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::time::Instant;

/// Bytes per `write` and `read` call.
const IO: usize = 256 * 1024;
const MIX: u64 = 0x9E37_79B9_7F4A_7C15;

/// Word `j` of the stream with `seed`; the stream is its little-endian bytes.
fn word(j: u64, seed: u64) -> u64 {
    j.wrapping_mul(MIX).wrapping_add(seed)
}

/// Fill `out` with the stream's bytes starting at byte `at`.
fn fill(out: &mut [u8], at: u64, seed: u64) {
    let mut i = 0;
    while i < out.len() {
        let pos = at + i as u64;
        let bytes = word(pos / 8, seed).to_le_bytes();
        let off = (pos % 8) as usize;
        let take = (8 - off).min(out.len() - i);
        out[i..i + take].copy_from_slice(&bytes[off..off + take]);
        i += take;
    }
}

/// The stream offset of the first byte of `got` (stream bytes from `at`)
/// that is not the stream's, if any.
fn mismatch(got: &[u8], at: u64, seed: u64) -> Option<u64> {
    let mut i = 0;
    while i < got.len() {
        let pos = at + i as u64;
        let bytes = word(pos / 8, seed).to_le_bytes();
        let off = (pos % 8) as usize;
        let take = (8 - off).min(got.len() - i);
        if got[i..i + take] != bytes[off..off + take] {
            return Some(pos);
        }
        i += take;
    }
    None
}

struct Report {
    bytes: u64,
    micros: u128,
    connect_us: u128,
}

fn connect(to: SocketAddr) -> Result<(TcpStream, u128), String> {
    let start = Instant::now();
    let stream = TcpStream::connect(to).map_err(|e| format!("connect: {e}"))?;
    Ok((stream, start.elapsed().as_micros()))
}

fn put(to: SocketAddr, bytes: u64, seed: u64) -> Result<Report, String> {
    let (mut stream, connect_us) = connect(to)?;
    stream
        .write_all(format!("PUT {bytes} {seed}\n").as_bytes())
        .map_err(|e| format!("header: {e}"))?;
    let mut buf = vec![0u8; IO];
    let start = Instant::now();
    let mut sent = 0u64;
    while sent < bytes {
        let n = (bytes - sent).min(IO as u64) as usize;
        fill(&mut buf[..n], sent, seed);
        stream
            .write_all(&buf[..n])
            .map_err(|e| format!("write at {sent}: {e}"))?;
        sent += n as u64;
    }
    stream
        .shutdown(Shutdown::Write)
        .map_err(|e| format!("shutdown: {e}"))?;
    let mut answer = String::new();
    stream
        .read_to_string(&mut answer)
        .map_err(|e| format!("answer: {e}"))?;
    let micros = start.elapsed().as_micros();
    if !answer.starts_with(&format!("OK {bytes}")) {
        return Err(format!("server said {:?}", answer.trim()));
    }
    Ok(Report {
        bytes,
        micros,
        connect_us,
    })
}

fn get(to: SocketAddr, bytes: u64, seed: u64) -> Result<Report, String> {
    let (mut stream, connect_us) = connect(to)?;
    stream
        .write_all(format!("GET {bytes} {seed}\n").as_bytes())
        .map_err(|e| format!("header: {e}"))?;
    let mut buf = vec![0u8; IO];
    let start = Instant::now();
    let mut got = 0u64;
    loop {
        let n = stream
            .read(&mut buf)
            .map_err(|e| format!("read at {got}: {e}"))?;
        if n == 0 {
            break;
        }
        if got + n as u64 > bytes {
            return Err(format!("more than {bytes} bytes"));
        }
        if let Some(at) = mismatch(&buf[..n], got, seed) {
            return Err(format!("byte {at} differs"));
        }
        got += n as u64;
    }
    let micros = start.elapsed().as_micros();
    if got != bytes {
        return Err(format!("received {got} of {bytes} bytes"));
    }
    Ok(Report {
        bytes,
        micros,
        connect_us,
    })
}

fn print(kind: &str, outcome: Result<Report, String>) -> bool {
    match outcome {
        Ok(r) => {
            let mbps = r.bytes as f64 / r.micros.max(1) as f64;
            println!(
                "NETBULK:linux:{kind}:PASS bytes={} ms={} mbps={mbps:.2} connect_us={}",
                r.bytes,
                r.micros / 1000,
                r.connect_us
            );
            true
        }
        Err(why) => {
            println!("NETBULK:linux:{kind}:FAIL:{why}");
            false
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let parsed = (|| {
        let ip: std::net::Ipv4Addr = args.get(1)?.parse().ok()?;
        let port: u16 = args.get(2)?.parse().ok()?;
        let bytes: u64 = args.get(3)?.parse().ok()?;
        let seed: u64 = args.get(4).map_or(Some(1), |s| s.parse().ok())?;
        let rounds: u32 = args.get(5).map_or(Some(1), |s| s.parse().ok())?;
        Some((SocketAddr::from((ip, port)), bytes, seed, rounds))
    })();
    let Some((to, bytes, seed, rounds)) = parsed else {
        println!("usage: netbulk-linux <a.b.c.d> <port> <bytes> [seed] [rounds]");
        println!("NETBULK:linux:FAIL:usage");
        std::process::exit(2);
    };
    let mut ok = true;
    for round in 0..rounds {
        let seed = seed.wrapping_add(u64::from(round) * 2);
        ok &= print("PUT", put(to, bytes, seed));
        ok &= print("GET", get(to, bytes, seed + 1));
    }
    println!("NETBULK:linux:done ok={ok}");
    std::process::exit(if ok { 0 } else { 1 });
}
