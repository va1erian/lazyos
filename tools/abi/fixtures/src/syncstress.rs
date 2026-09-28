//! `syncstress` — `std::sync` under contention: atomics, `Mutex`, `Condvar`,
//! `RwLock`, `mpsc`, `Barrier`, `Once`, `thread_local` and scoped threads.
//!
//! One wave of workers runs every primitive (a fresh wave per section would
//! exhaust the kernel's fixed task table), then a spawn-churn probe checks
//! that finished threads release their task slot.

mod common;

use std::cell::Cell;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Barrier, Condvar, Mutex, Once, RwLock};

// The bench captures at 8s and the BIOS+musl boot already costs ~4s under
// TCG; every futex park/wake round costs this kernel hundreds of ms of guest
// time, so the stress wave is kept at four workers. See the PR for the
// 8-thread run that took the deadline (~9s) and the thread-slot leak it found.
const THREADS: usize = 4;
const PRODUCERS: usize = THREADS / 2;
const ITERS: usize = 5;
/// A rendezvous with every worker costs the kernel one scheduling round per
/// waiter; two is enough to prove the barrier without eating the bench slot.
const BARRIER_WAITERS: usize = 2;
/// Scoped threads get their own small group so the main thread also joins.
const SCOPED_THREADS: usize = 1;
/// How many workers race on the `Once`.
const ONCE_RACERS: usize = 2;
/// Spawn attempts after the workers and the scope: the kernel does not reclaim
/// a finished thread's task slot, so the attempt past the free slots fails.
const CHURN_ATTEMPTS: usize = 16;

thread_local! {
    static TL: Cell<u64> = const { Cell::new(0) };
}

static ONCE: Once = Once::new();
static ONCE_RUNS: AtomicUsize = AtomicUsize::new(0);

struct Work {
    atom: AtomicUsize,
    mutex: Mutex<u64>,
    queue: Mutex<VecDeque<u64>>,
    condvar: Condvar,
    produced: AtomicUsize,
    consumed: AtomicU64,
    rw: RwLock<u64>,
    rw_range_bad: AtomicBool,
    barrier: Barrier,
    arrived: AtomicUsize,
    barrier_ok: [AtomicBool; THREADS],
    tls_ok: [AtomicBool; THREADS],
    done: AtomicUsize,
}

impl Work {
    fn new() -> Self {
        Work {
            atom: AtomicUsize::new(0),
            mutex: Mutex::new(0),
            queue: Mutex::new(VecDeque::new()),
            condvar: Condvar::new(),
            produced: AtomicUsize::new(0),
            consumed: AtomicU64::new(0),
            rw: RwLock::new(0),
            rw_range_bad: AtomicBool::new(false),
            barrier: Barrier::new(BARRIER_WAITERS),
            arrived: AtomicUsize::new(0),
            barrier_ok: std::array::from_fn(|_| AtomicBool::new(false)),
            tls_ok: std::array::from_fn(|_| AtomicBool::new(false)),
            done: AtomicUsize::new(0),
        }
    }
}

/// One worker's full script. Even workers produce/read and join the barrier;
/// odd workers consume/write.
fn worker(work: &Work, t: usize, tx: mpsc::Sender<u64>) {
    // Atomics.
    for _ in 0..ITERS {
        work.atom.fetch_add(1, Ordering::Relaxed);
    }

    // Mutex contention.
    for _ in 0..ITERS {
        *work.mutex.lock().unwrap() += 1;
    }

    // Condvar: even workers publish a batch and notify, odd workers wait for
    // the batch and drain it.
    if t % 2 == 0 {
        {
            let mut queue = work.queue.lock().unwrap();
            for i in 0..ITERS {
                queue.push_back((t * ITERS + i) as u64);
            }
        }
        work.produced.fetch_add(ITERS, Ordering::SeqCst);
        work.condvar.notify_all();
    } else {
        let mut sum = 0u64;
        let mut guard = work.queue.lock().unwrap();
        while work.produced.load(Ordering::SeqCst) < PRODUCERS * ITERS {
            guard = work.condvar.wait(guard).unwrap();
        }
        while let Some(value) = guard.pop_front() {
            sum += value;
        }
        drop(guard);
        work.consumed.fetch_add(sum, Ordering::SeqCst);
    }

    // RwLock: even workers read, odd workers write.
    if t % 2 == 0 {
        for _ in 0..ITERS {
            if *work.rw.read().unwrap() > (PRODUCERS * ITERS) as u64 {
                work.rw_range_bad.store(true, Ordering::SeqCst);
            }
        }
    } else {
        for _ in 0..ITERS {
            *work.rw.write().unwrap() += 1;
        }
    }

    // mpsc: every value must arrive once the senders drop.
    for i in 0..ITERS {
        let _ = tx.send((t * ITERS + i) as u64);
    }

    // Per-thread TLS.
    TL.with(|slot| slot.set(t as u64 + 1));
    let tls_ok = TL.with(|slot| slot.get()) == t as u64 + 1;
    work.tls_ok[t].store(tls_ok, Ordering::SeqCst);

    // Once: the initializer must run exactly once across the racers.
    if t < ONCE_RACERS {
        ONCE.call_once(|| {
            ONCE_RUNS.fetch_add(1, Ordering::SeqCst);
        });
    }

    // Barrier: the first workers rendezvous; none may pass before all arrive.
    if t < BARRIER_WAITERS {
        work.arrived.fetch_add(1, Ordering::SeqCst);
        work.barrier.wait();
        let all = work.arrived.load(Ordering::SeqCst) == BARRIER_WAITERS;
        work.barrier_ok[t].store(all, Ordering::SeqCst);
    }

    work.done.fetch_add(1, Ordering::SeqCst);
}

fn note(first: &mut Option<String>, reason: String) {
    if first.is_none() {
        *first = Some(reason);
    }
}

fn main() {
    let mut first: Option<String> = None;

    // One wave of workers, each running every primitive.
    let (tx, rx) = mpsc::channel::<u64>();
    let work = Arc::new(Work::new());
    let mut handles = Vec::new();
    for t in 0..THREADS {
        let work = Arc::clone(&work);
        let tx = tx.clone();
        match std::thread::Builder::new()
            .stack_size(64 * 1024)
            .spawn(move || worker(&work, t, tx))
        {
            Ok(handle) => handles.push(handle),
            Err(err) => note(&mut first, format!("worker {t} spawn: {err}")),
        }
    }
    drop(tx);
    // Wait for every worker's results, then join one handle: the rest detach.
    // (A join per thread would park this thread once per worker, which the
    // kernel's futex round-trip makes far more expensive than the work.)
    while work.done.load(Ordering::SeqCst) < THREADS {
        std::hint::spin_loop();
    }
    if let Some(handle) = handles.pop() {
        if handle.join().is_err() {
            note(&mut first, "a worker panicked".to_string());
        }
    }
    drop(handles);

    // Verify the computed results of every primitive. A missed worker-spawn
    // already failed the matching check here.
    if work.atom.load(Ordering::SeqCst) != THREADS * ITERS {
        note(&mut first, "atomic increments lost".to_string());
    }
    if *work.mutex.lock().unwrap() != (THREADS * ITERS) as u64 {
        note(&mut first, "mutex lost increments".to_string());
    }
    if work.produced.load(Ordering::SeqCst) != PRODUCERS * ITERS {
        note(&mut first, "condvar producers lost items".to_string());
    }
    if *work.rw.read().unwrap() != (PRODUCERS * ITERS) as u64 {
        note(&mut first, "rwlock lost writes".to_string());
    }
    if work.rw_range_bad.load(Ordering::SeqCst) {
        note(
            &mut first,
            "rwlock reader saw an out-of-range value".to_string(),
        );
    }
    if work.arrived.load(Ordering::SeqCst) != BARRIER_WAITERS {
        note(&mut first, "barrier lost a waiter".to_string());
    }
    if (0..BARRIER_WAITERS).any(|t| !work.barrier_ok[t].load(Ordering::SeqCst)) {
        note(&mut first, "barrier released a worker early".to_string());
    }
    if work.tls_ok.iter().any(|ok| !ok.load(Ordering::SeqCst)) {
        note(
            &mut first,
            "thread_local storage was not per-thread".to_string(),
        );
    }
    if TL.with(|slot| slot.get()) != 0 {
        note(
            &mut first,
            "main thread's thread_local was touched".to_string(),
        );
    }
    if ONCE_RUNS.load(Ordering::SeqCst) != 1 {
        note(
            &mut first,
            "Once initializer ran more than once".to_string(),
        );
    }

    let full_sum: u64 = (0..THREADS)
        .map(|t| (0..ITERS).map(|i| (t * ITERS + i) as u64).sum::<u64>())
        .sum();
    let cond_sum: u64 = (0..PRODUCERS)
        .map(|p| {
            (0..ITERS)
                .map(|i| ((p * 2) * ITERS + i) as u64)
                .sum::<u64>()
        })
        .sum();
    if work.consumed.load(Ordering::SeqCst) != cond_sum {
        note(
            &mut first,
            "condvar consumers summed wrong values".to_string(),
        );
    }
    let received: u64 = rx.iter().sum();
    if received != full_sum {
        note(&mut first, "channel lost messages".to_string());
    }

    // Scoped threads: borrow a local atomic and join before the scope returns.
    let scoped = AtomicU64::new(0);
    std::thread::scope(|scope| {
        let scoped = &scoped;
        for _ in 0..SCOPED_THREADS {
            std::thread::Builder::new()
                .stack_size(64 * 1024)
                .spawn_scoped(scope, || {
                    for _ in 0..ITERS {
                        scoped.fetch_add(1, Ordering::Relaxed);
                    }
                })
                .unwrap();
        }
    });
    if scoped.load(Ordering::Relaxed) != SCOPED_THREADS as u64 * ITERS as u64 {
        note(&mut first, "scoped threads lost increments".to_string());
    }

    // Spawn-churn probe: a program that starts many short-lived threads must
    // keep working. Each finished thread currently holds its task slot, so
    // this reports the exhausted-table gap instead of panicking.
    let mut churn = 0;
    loop {
        match std::thread::Builder::new().spawn(|| ()) {
            Ok(handle) => {
                drop(handle); // detach: the thread exits on its own
                churn += 1;
                if churn >= CHURN_ATTEMPTS {
                    break;
                }
            }
            Err(err) => {
                note(&mut first, format!("thread churn spawn {churn}: {err}"));
                break;
            }
        }
    }

    match first {
        Some(reason) => common::fail("syncstress", &reason),
        None => common::pass("syncstress"),
    }
}
