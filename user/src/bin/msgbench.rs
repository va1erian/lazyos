//! `msgbench` (`/system/bin/msgbench`): the cross-process Messenger benchmark
//! (docs/performance-plan.md P6 step 0).
//!
//! ```text
//! msgbench [rounds]          client: start a server child, measure, report
//! msgbench serve <name>      server: resolve <name>, answer until the peer goes
//! ```
//!
//! The client creates a channel, publishes the server side under a private
//! name, and starts a copy of itself as the server, which resolves the name.
//! Both ends are real user processes, so every round trip is two syscalls on
//! each side and two context switches: the figure `docs/messenger.md` sets a
//! target for (under 10 us median, over 200k messages per second).
//!
//! The requests are `os.lazy.echo.v1` messages (`idl/echo.midl`), encoded
//! once and sent with the raw `messenger` syscall, so the user side adds no
//! encode, decode or allocation per message: what is measured is the kernel.
//!
//! * `msg_rt`: one `Ping` call, `rdtsc` around the `OP_CALL` syscall.
//! * `msg_tput`: bursts of [`BURST`] one-way `Notify` sends closed by one
//!   `Ping` call (which returns once the server drained the burst, FIFO):
//!   messages delivered per second, and calls per second without the sends.
//!
//! Cycles are converted with the TSC rate measured against the monotonic
//! clock over the run. Output, parsed by `tools/perf/run.py`:
//!
//! ```text
//! PERF:msg_rt:n=<n> p50_us=<f> p90_us=<f> p99_us=<f> max_us=<f> mean_us=<f>
//! PERF:msg_tput:msgs_per_s=<n> calls_per_s=<n>
//! MSGBENCH:DONE
//! ```

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::panic::PanicInfo;

use libmessenger::{flags, Header, Parcel, VERSION};
use messenger_generated::os_lazy_echo_v1 as echo;
use user::messenger::{self, errno, op, registry, MsgArgs, MsgResult};
use user::sys;

/// Timed round trips by default (after [`WARMUP`]).
const ROUNDS: usize = 20_000;
const WARMUP: usize = 500;
/// One-way messages per burst in the throughput pass, under the endpoint
/// inbox depth (64) so a burst never meets a full queue.
const BURST: usize = 32;
/// Bursts in the throughput pass.
const BURSTS: usize = 2_000;
/// The name the server side is published under while the server resolves it.
const NAME: &str = "perf.msgbench";

#[no_mangle]
pub extern "C" fn _start() -> ! {
    let mut args = sys::args().skip(1);
    let status = match args.next() {
        Some("serve") => serve(args.next().unwrap_or(NAME)),
        other => {
            let rounds = other.and_then(|text| text.parse().ok()).unwrap_or(ROUNDS);
            match client(rounds.max(1)) {
                Ok(()) => 0,
                Err(message) => {
                    sys::write_str(&format!("MSGBENCH:FAIL:{message}\n"));
                    1
                }
            }
        }
    };
    sys::exit(status)
}

/// The raw `messenger` syscall: the status (0 or `-errno`).
fn msg(op: u64, args: &MsgArgs, result: &mut MsgResult) -> i64 {
    // SAFETY: every `MsgArgs` here points at buffers this function's callers
    // own for the call, with their real lengths.
    match unsafe { lazyos_sys::msg::messenger(op, args, result) } {
        Ok(()) => 0,
        Err(code) => code,
    }
}

fn rdtsc() -> u64 {
    // SAFETY: `rdtsc` only reads the time-stamp counter.
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// An encoded `os.lazy.echo.v1` parcel.
fn parcel(method: u32, parcel_flags: u16, body: Vec<u8>) -> Vec<u8> {
    let parcel = Parcel {
        header: Header {
            version: VERSION,
            flags: parcel_flags,
            interface_id: echo::INTERFACE_ID,
            method,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body,
        handles: Vec::new(),
        buffers: Vec::new(),
    };
    let mut bytes = Vec::new();
    let _ = parcel.encode(&mut bytes);
    bytes
}

fn ping_request() -> Vec<u8> {
    parcel(echo::METHOD_PING, flags::SYNC, Vec::new())
}

fn ping_reply() -> Vec<u8> {
    let body = echo::encode_ping_reply(&echo::PingReply { alive: true }).unwrap_or_default();
    parcel(echo::METHOD_PING, 0, body)
}

fn notify() -> Vec<u8> {
    let event = echo::Event {
        topic: String::from("perf"),
        at: 0,
    };
    let body = echo::encode_notify_args(&echo::NotifyArgs { event }).unwrap_or_default();
    parcel(echo::METHOD_NOTIFY, 0, body)
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

/// Answer every `Ping` with the pre-encoded reply and drop every `Notify`,
/// until the client's side closes.
fn serve(name: &str) -> u32 {
    let Ok(endpoint) = registry::resolve(name) else {
        sys::write_str("MSGBENCH:FAIL:server could not resolve its name\n");
        return 1;
    };
    let reply = ping_reply();
    let mut buf = [0u8; 256];
    loop {
        let args = MsgArgs {
            handle: endpoint.handle(),
            buf_ptr: buf.as_mut_ptr() as u64,
            buf_cap: buf.len() as u64,
            ..MsgArgs::default()
        };
        let mut result = MsgResult::default();
        let status = msg(op::RECV, &args, &mut result);
        if status == -errno::EPIPE {
            return 0;
        }
        if status != 0 {
            sys::write_str(&format!("MSGBENCH:FAIL:server recv {status}\n"));
            return 1;
        }
        if result.value == 0 {
            continue; // one-way: nothing to answer
        }
        let answer = MsgArgs {
            txn_id: result.value,
            parcel_ptr: reply.as_ptr() as u64,
            parcel_len: reply.len() as u64,
            ..MsgArgs::default()
        };
        let status = msg(op::REPLY, &answer, &mut MsgResult::default());
        if status != 0 && status != -errno::ENOENT {
            sys::write_str(&format!("MSGBENCH:FAIL:server reply {status}\n"));
            return 1;
        }
    }
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// The client's channel end and the reusable request and reply buffers.
struct Client {
    handle: u64,
    ping: Vec<u8>,
    notify: Vec<u8>,
    buf: Vec<u8>,
}

impl Client {
    fn call(&mut self) -> Result<(), String> {
        let args = MsgArgs {
            handle: self.handle,
            parcel_ptr: self.ping.as_ptr() as u64,
            parcel_len: self.ping.len() as u64,
            buf_ptr: self.buf.as_mut_ptr() as u64,
            buf_cap: self.buf.len() as u64,
            ..MsgArgs::default()
        };
        match msg(op::CALL, &args, &mut MsgResult::default()) {
            0 => Ok(()),
            status => Err(format!("call {status}")),
        }
    }

    fn send(&mut self) -> Result<(), String> {
        let args = MsgArgs {
            handle: self.handle,
            parcel_ptr: self.notify.as_ptr() as u64,
            parcel_len: self.notify.len() as u64,
            ..MsgArgs::default()
        };
        match msg(op::SEND, &args, &mut MsgResult::default()) {
            0 => Ok(()),
            status => Err(format!("send {status}")),
        }
    }
}

fn client(rounds: usize) -> Result<(), String> {
    let (mine, theirs) = messenger::create_pair().map_err(|_| "create_pair")?;
    registry::register(NAME, &theirs, &[echo::INTERFACE_ID], 0).map_err(|_| "register")?;
    let program = fhs::bin::MSGBENCH;
    let child = sys::spawn_native(program, &["serve", NAME]);
    let mut client = Client {
        handle: mine.handle(),
        ping: ping_request(),
        notify: notify(),
        buf: vec![0u8; 256],
    };
    let outcome = match child {
        Some(_) => {
            // The first answered call proves the server holds its own handle:
            // the name and this task's handle to that side can go.
            let first = client.call();
            let _ = registry::unregister(NAME);
            let _ = theirs.release();
            first.and_then(|()| measure(&mut client, rounds))
        }
        None => Err(String::from("spawn")),
    };
    // Closing this side ends the server's `recv` with `-EPIPE`.
    let _ = mine.close();
    if child.is_some() {
        let _ = sys::wait(sys::clock() + 500);
    }
    outcome
}

fn measure(client: &mut Client, rounds: usize) -> Result<(), String> {
    let start_ns = sys::monotonic_ns();
    let start_tsc = rdtsc();
    let mut samples = Vec::with_capacity(rounds);
    for round in 0..WARMUP + rounds {
        let before = rdtsc();
        client.call()?;
        let cycles = rdtsc().wrapping_sub(before);
        if round >= WARMUP {
            samples.push(cycles);
        }
    }
    let calls_end = rdtsc();
    for _ in 0..BURSTS {
        for _ in 0..BURST {
            client.send()?;
        }
        client.call()?;
    }
    let end_tsc = rdtsc();
    let elapsed_ns = sys::monotonic_ns().saturating_sub(start_ns).max(1);
    // TSC cycles per microsecond, from the whole run against the clock.
    let per_us = (end_tsc.wrapping_sub(start_tsc) as u128 * 1000 / elapsed_ns as u128).max(1);
    let us = |cycles: u64| Micros((cycles as u128 * 10 / per_us) as u64);
    samples.sort_unstable();
    let pick = |permille: usize| samples[((samples.len() - 1) * permille) / 1000];
    let sum: u128 = samples.iter().map(|&value| value as u128).sum();
    let mean = (sum / samples.len() as u128) as u64;
    let max = samples.last().copied().unwrap_or(0);
    sys::write_str(&format!(
        "PERF:msg_rt:n={} p50_us={} p90_us={} p99_us={} max_us={} mean_us={}\n",
        samples.len(),
        us(pick(500)),
        us(pick(900)),
        us(pick(990)),
        us(max),
        us(mean),
    ));
    let call_cycles = calls_end.wrapping_sub(start_tsc).max(1) as u128;
    let calls_per_s = (WARMUP + rounds) as u128 * per_us * 1_000_000 / call_cycles;
    let burst_cycles = end_tsc.wrapping_sub(calls_end).max(1) as u128;
    let messages = (BURSTS * (BURST + 1)) as u128;
    let msgs_per_s = messages * per_us * 1_000_000 / burst_cycles;
    sys::write_str(&format!(
        "PERF:msg_tput:msgs_per_s={msgs_per_s} calls_per_s={calls_per_s}\nMSGBENCH:DONE\n"
    ));
    Ok(())
}

/// Tenths of a microsecond, shown as microseconds with one decimal.
struct Micros(u64);

impl core::fmt::Display for Micros {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}.{}", self.0 / 10, self.0 % 10)
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
