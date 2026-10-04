//! The in-kernel Messenger echo benchmark behind `PERF:ipc_rt`.
//!
//! One kernel-task round trip through the real channel code: `begin_call`
//! queues the request, `recv` takes it on the server side, `reply` answers and
//! `await_reply` returns the reply. Both ends live in the kernel task, so
//! there is no context switch and no user copy: this measures the fabric's
//! own cost (validation, copies, allocations, locks), the floor under a real
//! cross-task call. Each round trip runs with interrupts off, like a syscall.

use alloc::vec::Vec;

use libmessenger::{flags, Encoder, Header, Parcel, VERSION};

use crate::ipc::channels;

/// Round trips per run (warm-up excluded).
const ROUNDS: usize = 2000;
const WARMUP: usize = 50;

fn parcel(method: u32, parcel_flags: u16) -> Option<Vec<u8>> {
    let mut body = Encoder::new();
    body.string(1, "perf").ok()?;
    let parcel = Parcel {
        header: Header {
            version: VERSION,
            flags: parcel_flags,
            interface_id: 0x7065_7266,
            method,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body: body.finish(),
        handles: Vec::new(),
        buffers: Vec::new(),
    };
    let mut bytes = Vec::new();
    parcel.encode(&mut bytes).ok()?;
    Some(bytes)
}

/// One echo; `None` if any step failed.
fn round_trip(client: u64, server: u64, request: &[u8], reply: &[u8]) -> Option<()> {
    let txn = channels::begin_call(client, 1, request, None).ok()?;
    let message = channels::recv(server, None).ok()?;
    channels::reply(message.txn?, reply).ok()?;
    channels::await_reply(txn).ok()?;
    Some(())
}

/// Run the benchmark, handing each round trip's cycles to `record`.
pub fn run(mut record: impl FnMut(u64)) {
    let (Some(request), Some(reply)) = (parcel(1, flags::SYNC), parcel(2, 0)) else {
        return;
    };
    let Ok((client, server)) = channels::create() else {
        crate::serial_println!("PERF:ipc_rt:SKIP no channel");
        return;
    };
    let mut failed = false;
    for round in 0..WARMUP + ROUNDS {
        let outcome = x86_64::instructions::interrupts::without_interrupts(|| {
            let start = super::rdtsc();
            let done = round_trip(client, server, &request, &reply);
            (done, super::rdtsc().wrapping_sub(start))
        });
        match outcome {
            (Some(()), cycles) if round >= WARMUP => record(cycles),
            (Some(()), _) => {}
            (None, _) => {
                failed = true;
                break;
            }
        }
    }
    let _ = channels::close_endpoint(client);
    let _ = channels::close_endpoint(server);
    if failed {
        crate::serial_println!("PERF:ipc_rt:SKIP a round trip failed");
    }
}
