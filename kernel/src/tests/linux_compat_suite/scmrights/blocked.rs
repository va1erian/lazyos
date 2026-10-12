//! A send that waited for room re-checks for cycles when it resumes (review
//! of #771): the pair it carries may have come to carry one of its own ends
//! while it slept, and the check at `sendmsg` time alone would let the two
//! pairs hold each other.

use super::*;
use crate::ipc::credentials;
use crate::ipc::pipe::{Rights, Side, SocketPair};
use crate::task::wait::WaitQueue;
use crate::task::{Fd, PriorityClass, WaitKind};
use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

/// What the sender thread sends: the pair to send on and the end to carry.
static JOB: Mutex<Option<(Arc<SocketPair>, Fd)>> = Mutex::new(None);
/// The sender's outcome: 0 not yet, 1 sent, 2 refused as a cycle, 3 other.
static RESULT: AtomicU64 = AtomicU64::new(0);
/// The thread parks here once it is done.
static GATE: WaitQueue = WaitQueue::new(WaitKind::Sleep);

extern "C" fn sender() -> ! {
    let me = task::current();
    let job = JOB.lock().take();
    if let Some((pair, end)) = job {
        let uid = credentials::of(me).uid;
        let mut rights = Rights::new(alloc::vec![end], uid).ok();
        let code = match pair.write_rights(Side::A, b"x", false, None, &mut rights) {
            Ok(_) => 1,
            Err(pipe::Error::TooManyRefs) => 2,
            Err(_) => 3,
        };
        // A `-> !` function drops nothing on its own: the refused rights
        // (and the pair) go now, so nothing stays in flight.
        drop(rights);
        drop(pair);
        RESULT.store(code, Ordering::Release);
    }
    loop {
        GATE.wait(me, None);
    }
}

/// Fill pair A's A -> B direction, block a thread sending an end of pair B
/// through it, send an end of A through B meanwhile (A carries nothing yet,
/// so that is allowed), then drain A: the resumed send must be refused, and
/// nothing is left in flight once B's message is taken.
pub fn scmrights_blocked_send_revalidates() -> Result<(), String> {
    fresh()?;
    let pipes_before = pipe::Pipe::live();
    let a = SocketPair::new().ok_or("pair a")?;
    let b = SocketPair::new().ok_or("pair b")?;
    // The descriptors a process would hold: they carry the sides' reader and
    // writer references, without which a write sees no reader.
    let ends = [
        Fd::socket_side(Arc::clone(&a), Side::A),
        Fd::socket_side(Arc::clone(&a), Side::B),
        Fd::socket_side(Arc::clone(&b), Side::A),
        Fd::socket_side(Arc::clone(&b), Side::B),
    ];
    let chunk = [0u8; 4096];
    let mut filled = 0;
    loop {
        match a.write_until(Side::A, &chunk, true, None) {
            Ok(n) => filled += n,
            Err(pipe::Error::WouldBlock) => break,
            Err(error) => return Err(format!("fill: {error:?}")),
        }
    }
    check!(filled > 0, "nothing was queued on A");

    *JOB.lock() = Some((Arc::clone(&a), ends[2].clone()));
    RESULT.store(0, Ordering::Release);
    task::set_blocked(false);
    let thread = task::kthread::spawn_kernel_thread("scmsend", sender, PriorityClass::Realtime)
        .map_err(|error| format!("spawn: {error}"))?;
    task::switch::yield_now();
    check!(
        RESULT.load(Ordering::Acquire) == 0,
        "the send did not block on the full queue: {}",
        RESULT.load(Ordering::Acquire)
    );
    check!(JOB.lock().is_none(), "the thread did not take its job");

    // Meanwhile an end of A travels through B: A carries nothing yet.
    let uid = credentials::of(task::current()).uid;
    let mut carry_a = Rights::new(alloc::vec![ends[1].clone()], uid).ok();
    check!(
        b.write_rights(Side::A, b"y", true, None, &mut carry_a)
            .is_ok(),
        "an end of A through B while A carries nothing"
    );
    check!(
        b.carries_rights() && !a.carries_rights(),
        "B should carry, A not yet"
    );

    // Drain A: the blocked send wakes, re-checks, and must refuse.
    let mut sink = [0u8; 4096];
    loop {
        match a.read_until(Side::B, &mut sink, true, None) {
            Ok(0) | Err(pipe::Error::WouldBlock) => break,
            Ok(_) => {}
            Err(error) => return Err(format!("drain: {error:?}")),
        }
    }
    task::switch::yield_now();
    check!(
        RESULT.load(Ordering::Acquire) == 2,
        "the resumed send: {} (1 sent, 2 refused, 3 other)",
        RESULT.load(Ordering::Acquire)
    );
    check!(!a.carries_rights(), "A came to carry B after all");

    // Take A's end out of B so nothing is in flight, then end the thread.
    let mut got: Vec<Fd> = Vec::new();
    let n = b
        .recv_rights(Side::B, &mut sink, (true, false), None, Some(&mut got))
        .map_err(|error| format!("receive from B: {error:?}"))?;
    check!(
        n == 1 && got.len() == 1,
        "B's message: {n} bytes, {} fds",
        got.len()
    );
    drop(got);
    task::harness::finish(thread, 0);
    GATE.notify_all();
    while task::reap_child().is_some() {}
    drop(ends);
    drop(a);
    drop(b);
    task::harness::reset();
    settled(pipes_before)
}
