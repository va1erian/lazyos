//! `nc` (`/system/bin/nc`): connect to a TCP or UDP port, or listen on one
//! (`docs/networking-plan.md`, stage N3).
//!
//! ```text
//! nc [-u] [-w secs] [-n] [-x] [-g bytes] <host> <port> [text...]
//! nc [-u] -l [-w secs] [-x] <port> [text...]
//! nc -i <host> <port>
//! ```
//!
//! The text after the port (arguments are whitespace-split and rejoined with
//! single spaces) is sent followed by a newline (`-n`: without); whatever comes
//! back is written to the console until the peer closes or nothing arrives for
//! `-w` seconds (default 3). `-l` accepts one connection first. `-i` is a line
//! relay: each line typed is sent and the replies shown (an empty line ends
//! it); native programs have only a blocking `read_char`, so there is no
//! streaming both ways at once.
//!
//! Evidence modes: `-g <bytes>` sends that many deterministic bytes instead of
//! text; `-x` expects the peer to echo exactly what was sent (or, with `-l`,
//! echoes what it receives) and decides the verdict on it. The markers are
//! `NC:LISTENING port=<p>`, then `NC:PASS ...` or `NC:FAIL ...`; the host
//! judges the bytes from the capture and its own server.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::panic::PanicInfo;

use user::messenger::netsock::{Addr, Client};
use user::messenger::netstack::Client as Stack;
use user::messenger::netstd::{is_timeout, TcpListener, TcpStream, UdpSocket};
use user::messenger::Error as MsgError;
use user::sys;

/// Ticks to wait for `netd` to register at boot.
const CONNECT_TICKS: u64 = 500;
/// Ticks to wait for an address after boot.
const ADDRESS_TICKS: u64 = 1500;
/// Milliseconds a connection attempt may take.
const CONNECT_MS: u32 = 8000;
/// Milliseconds one receive waits before the idle clock advances.
const SLICE_MS: u32 = 100;
/// Largest `-g` payload.
const MAX_GENERATED: usize = 4 * 1024 * 1024;

struct Options {
    udp: bool,
    listen: bool,
    interactive: bool,
    newline: bool,
    expect_echo: bool,
    idle_secs: u32,
    generate: Option<usize>,
    host: String,
    port: u16,
    text: String,
}

fn usage() -> String {
    String::from("usage: nc [-u] [-l] [-i] [-n] [-x] [-w secs] [-g bytes] <host> <port> [text...]")
}

fn parse() -> Result<Options, String> {
    let mut buffer = [0u8; 1024];
    let len = sys::service_args(&mut buffer).min(buffer.len());
    let line = core::str::from_utf8(&buffer[..len]).map_err(|_| usage())?;
    let mut opts = Options {
        udp: false,
        listen: false,
        interactive: false,
        newline: true,
        expect_echo: false,
        idle_secs: 3,
        generate: None,
        host: String::new(),
        port: 0,
        text: String::new(),
    };
    let mut words = line.split_whitespace().peekable();
    while let Some(word) = words.peek().copied() {
        if !word.starts_with('-') || word.len() < 2 {
            break;
        }
        words.next();
        match word {
            "-u" => opts.udp = true,
            "-l" => opts.listen = true,
            "-i" => opts.interactive = true,
            "-n" => opts.newline = false,
            "-x" => opts.expect_echo = true,
            "-w" => {
                let value = words.next().and_then(|v| v.parse::<u32>().ok());
                opts.idle_secs = value.ok_or_else(usage)?.clamp(1, 600);
            }
            "-g" => {
                let value = words.next().and_then(|v| v.parse::<usize>().ok());
                opts.generate = Some(value.ok_or_else(usage)?.clamp(1, MAX_GENERATED));
            }
            _ => return Err(usage()),
        }
    }
    if !opts.listen {
        opts.host = String::from(words.next().ok_or_else(usage)?);
    }
    let port = words.next().ok_or_else(usage)?;
    opts.port = port.parse().ok().filter(|p| *p != 0).ok_or_else(usage)?;
    let mut text = String::new();
    for word in words {
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str(word);
    }
    opts.text = text;
    Ok(opts)
}

fn describe(error: &MsgError) -> String {
    use user::messenger::netsock::errno as e;
    match error {
        MsgError::Errno(code) if *code == -e::ECONNREFUSED => String::from("connection refused"),
        MsgError::Errno(code) if *code == -e::ENETUNREACH => String::from("network is unreachable"),
        MsgError::Errno(code) if *code == -e::ECONNRESET => String::from("connection reset"),
        MsgError::Errno(code) if *code == -user::messenger::errno::ETIMEDOUT => {
            String::from("timed out")
        }
        other => String::from(other.message()),
    }
}

fn fail(what: &'static str) -> impl Fn(MsgError) -> String {
    move |error| format!("{what}: {}", describe(&error))
}

/// `netd`, once it has registered and has an address.
fn connect() -> Result<(Rc<Client>, Stack), String> {
    let deadline = sys::clock() + CONNECT_TICKS;
    let stack = loop {
        match Stack::connect() {
            Ok(stack) => break stack,
            Err(error) if sys::clock() >= deadline => {
                return Err(format!("no network stack: {}", error.message()))
            }
            Err(_) => {
                let _ = sys::wait(sys::clock() + 1);
            }
        }
    };
    let deadline = sys::clock() + ADDRESS_TICKS;
    while sys::clock() < deadline && !stack.addresses().is_ok_and(|a| !a.is_empty()) {
        let _ = sys::wait(sys::clock() + 1);
    }
    let sockets = Client::connect().map_err(fail("sockets"))?;
    Ok((Rc::new(sockets), stack))
}

/// Deterministic bytes (an xorshift stream), so the peer's echo is checkable
/// without storing the payload twice.
struct Pattern(u64);

impl Pattern {
    fn new() -> Pattern {
        Pattern(0x9E37_79B9_7F4A_7C15)
    }

    fn fill(&mut self, out: &mut [u8]) {
        for byte in out {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            *byte = (self.0 >> 24) as u8;
        }
    }
}

/// What to send and, for `-x`, what must come back.
fn payload(opts: &Options) -> Vec<u8> {
    if let Some(n) = opts.generate {
        let mut data = alloc::vec![0u8; n];
        Pattern::new().fill(&mut data);
        return data;
    }
    let mut data = Vec::from(opts.text.as_bytes());
    if !opts.text.is_empty() && opts.newline {
        data.push(b'\n');
    }
    data
}

struct Tally {
    sent: usize,
    received: usize,
    /// Bytes received that differ from the echo expected (`-x`).
    wrong: usize,
}

fn show(data: &[u8], quiet: bool) {
    if !quiet {
        sys::write(data);
    }
}

/// Send `data` and read replies at the same time, so a peer that echoes as it
/// reads can never wedge both sides on full buffers.
fn exchange_stream(stream: &TcpStream, data: &[u8], opts: &Options) -> Result<Tally, String> {
    let quiet = opts.generate.is_some();
    let mut tally = Tally {
        sent: 0,
        received: 0,
        wrong: 0,
    };
    let mut expect = Pattern::new();
    let mut idle_ms = 0u32;
    let limit_ms = opts.idle_secs * 1000;
    while idle_ms < limit_ms {
        let mut progressed = false;
        if tally.sent < data.len() {
            let end = (tally.sent + 4096).min(data.len());
            match stream.write_some(&data[tally.sent..end], 10) {
                Ok(n) => {
                    tally.sent += n;
                    progressed = true;
                }
                Err(error) if is_timeout(&error) => {}
                Err(error) => return Err(describe(&error)),
            }
        }
        let wait = if tally.sent < data.len() {
            10
        } else {
            SLICE_MS
        };
        match stream.read(16 * 1024, wait) {
            Ok(chunk) if chunk.is_empty() => break,
            Ok(chunk) => {
                progressed = true;
                if opts.expect_echo {
                    tally.wrong += check_echo(&chunk, tally.received, data, opts, &mut expect);
                }
                tally.received += chunk.len();
                show(&chunk, quiet);
            }
            Err(error) if is_timeout(&error) => {}
            Err(error) => return Err(describe(&error)),
        }
        if progressed {
            idle_ms = 0;
        } else {
            idle_ms += wait;
        }
        if opts.expect_echo && tally.sent == data.len() && tally.received >= data.len() {
            break;
        }
    }
    Ok(tally)
}

/// How many bytes of `chunk` (which starts `at` bytes into the echo) differ
/// from what was sent.
fn check_echo(chunk: &[u8], at: usize, sent: &[u8], opts: &Options, expect: &mut Pattern) -> usize {
    if opts.generate.is_none() {
        let want = sent.get(at..).unwrap_or(&[]);
        return chunk
            .iter()
            .enumerate()
            .filter(|(i, b)| want.get(*i) != Some(b))
            .count();
    }
    // The generated stream is regenerated in step with what has been read.
    let mut want = alloc::vec![0u8; chunk.len()];
    expect.fill(&mut want);
    if at + chunk.len() > sent.len() {
        return chunk.len();
    }
    want.iter().zip(chunk).filter(|(a, b)| a != b).count()
}

fn run_client(opts: &Options, sockets: &Rc<Client>, stack: &Stack) -> Result<Tally, String> {
    let ip = stack
        .lookup_host(&opts.host, 5000)
        .map_err(|e| format!("{}: cannot resolve ({})", opts.host, describe(&e)))?;
    let to = Addr::new(ip, opts.port);
    let data = payload(opts);
    if opts.udp {
        return run_udp(opts, sockets, to, &data);
    }
    let stream = TcpStream::connect(sockets, to, CONNECT_MS).map_err(fail("connect"))?;
    sys::write_str(&format!(
        "NC:CONNECTED {}.{}.{}.{}:{}\n",
        ip[0], ip[1], ip[2], ip[3], opts.port
    ));
    if opts.interactive {
        return run_interactive(&stream);
    }
    exchange_stream(&stream, &data, opts)
}

fn run_udp(opts: &Options, sockets: &Rc<Client>, to: Addr, data: &[u8]) -> Result<Tally, String> {
    let socket = UdpSocket::bind(sockets, 0).map_err(fail("bind"))?;
    let mut tally = Tally {
        sent: 0,
        received: 0,
        wrong: 0,
    };
    if !data.is_empty() {
        for part in data.chunks(1400) {
            socket.send_to(part, to).map_err(fail("sendto"))?;
            tally.sent += part.len();
        }
    }
    let mut idle_ms = 0;
    while idle_ms < opts.idle_secs * 1000 {
        match socket.recv_from(2048, SLICE_MS) {
            Ok((chunk, from)) if from == to => {
                if opts.expect_echo {
                    let at = tally.received;
                    let want = data.get(at..).unwrap_or(&[]);
                    tally.wrong += chunk
                        .iter()
                        .enumerate()
                        .filter(|(i, b)| want.get(*i) != Some(b))
                        .count();
                }
                tally.received += chunk.len();
                show(&chunk, opts.generate.is_some());
                idle_ms = 0;
                if opts.expect_echo && tally.received >= data.len() {
                    break;
                }
            }
            Ok(_) => {}
            Err(error) if is_timeout(&error) => idle_ms += SLICE_MS,
            Err(error) => return Err(describe(&error)),
        }
    }
    Ok(tally)
}

/// Read one line from the keyboard or the redirected stdin. `None` for an
/// empty line (the end of the session).
fn read_line() -> Option<String> {
    let mut line = String::new();
    loop {
        let code = sys::read_char();
        match code as u8 {
            b'\n' | b'\r' => break,
            byte if byte >= 0x20 && line.len() < 1024 => line.push(char::from(byte)),
            _ => {}
        }
    }
    (!line.is_empty()).then_some(line)
}

fn run_interactive(stream: &TcpStream) -> Result<Tally, String> {
    let mut tally = Tally {
        sent: 0,
        received: 0,
        wrong: 0,
    };
    loop {
        let mut chunk = Vec::new();
        // Show whatever the server said first (a greeting) or since the last line.
        while let Ok(data) = stream.read(16 * 1024, 300) {
            if data.is_empty() {
                return Ok(tally);
            }
            tally.received += data.len();
            chunk = data;
            sys::write(&chunk);
            if chunk.len() < 16 * 1024 {
                break;
            }
        }
        let _ = chunk;
        let Some(mut line) = read_line() else {
            return Ok(tally);
        };
        line.push('\n');
        stream
            .write_all(line.as_bytes(), 5000)
            .map_err(fail("send"))?;
        tally.sent += line.len();
    }
}

fn run_listener(opts: &Options, sockets: &Rc<Client>) -> Result<Tally, String> {
    let listener = TcpListener::bind(sockets, opts.port, 1).map_err(fail("listen"))?;
    sys::write_str(&format!("NC:LISTENING port={}\n", opts.port));
    let accept_ms = opts.idle_secs.saturating_mul(1000).min(60_000);
    let (stream, peer) = listener.accept(accept_ms).map_err(fail("accept"))?;
    sys::write_str(&format!(
        "NC:ACCEPTED {}.{}.{}.{}:{}\n",
        peer.ip[0], peer.ip[1], peer.ip[2], peer.ip[3], peer.port
    ));
    let mut tally = Tally {
        sent: 0,
        received: 0,
        wrong: 0,
    };
    let greeting = payload(opts);
    if !greeting.is_empty() && !opts.expect_echo {
        stream.write_all(&greeting, 5000).map_err(fail("send"))?;
        tally.sent += greeting.len();
    }
    let mut idle_ms = 0;
    while idle_ms < opts.idle_secs * 1000 {
        match stream.read(16 * 1024, SLICE_MS) {
            Ok(chunk) if chunk.is_empty() => break,
            Ok(chunk) => {
                idle_ms = 0;
                tally.received += chunk.len();
                if opts.expect_echo {
                    // An echo server: every byte goes straight back.
                    stream.write_all(&chunk, 5000).map_err(fail("echo"))?;
                    tally.sent += chunk.len();
                } else {
                    show(&chunk, false);
                }
            }
            Err(error) if is_timeout(&error) => idle_ms += SLICE_MS,
            Err(error) => return Err(describe(&error)),
        }
    }
    Ok(tally)
}

fn run() -> Result<(Tally, bool), String> {
    let opts = parse()?;
    let (sockets, stack) = connect()?;
    let tally = if opts.listen {
        if opts.udp {
            return Err(String::from("nc: -l with -u is not supported"));
        }
        run_listener(&opts, &sockets)?
    } else {
        run_client(&opts, &sockets, &stack)?
    };
    let ok = if opts.listen && opts.expect_echo {
        tally.received > 0 && tally.received == tally.sent
    } else if opts.expect_echo {
        tally.wrong == 0 && tally.received == tally.sent
    } else {
        true
    };
    Ok((tally, ok))
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    match run() {
        Ok((tally, true)) => {
            sys::write_str(&format!(
                "NC:PASS sent={} received={}\n",
                tally.sent, tally.received
            ));
            sys::exit(0)
        }
        Ok((tally, false)) => {
            sys::write_str(&format!(
                "NC:FAIL sent={} received={} wrong={}\n",
                tally.sent, tally.received, tally.wrong
            ));
            sys::exit(1)
        }
        Err(message) => {
            sys::write_str(&format!("NC:FAIL {message}\n"));
            sys::exit(1)
        }
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
