//! Prompt interrupt delivery (docs/performance-plan.md, P1.2).
//!
//! The bottom half runs on the way out of a device interrupt that stopped
//! user code or a halted task, so a userspace claimant hears of its
//! interrupt at once instead of at the next syscall or kernel-task pass.
//! These tests drive a real interrupt source, the COM1 UART's
//! transmit-empty interrupt on line 4, through a synthetic claimed device
//! whose driver is a real kernel thread (`task::kthread`) parked in `recv`
//! on its interrupt endpoint, exactly like a userspace driver.

use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use super::fixture::*;
use super::*;
use crate::arch::io::{inb, outb};
use crate::dev::claims::CLAIMS;
use crate::dev::irq;
use crate::dev::syscall::*;
use crate::task::wait::WaitQueue;
use crate::task::{PriorityClass, WaitKind, WakeReason};

const COM1: u16 = 0x3F8;
const IER: u16 = COM1 + 1;
const IIR: u16 = COM1 + 2;
const IER_THRE: u8 = 0x02;
/// COM1's PIC line, claimed here as if it were the synthetic device's.
const UART_LINE: u8 = 4;
const SAMPLES: usize = 200;

/// The driver parks here until its claim exists.
static GATE: WaitQueue = WaitQueue::new(WaitKind::Sleep);
/// The kernel task parks here while the driver handles an interrupt.
static KERNEL: WaitQueue = WaitQueue::new(WaitKind::Sleep);
static ENDPOINT: AtomicU64 = AtomicU64::new(0);
static HANDLE: AtomicU64 = AtomicU64::new(0);
/// A noisy device: it asserts again the moment the driver has acked.
static NOISY: AtomicBool = AtomicBool::new(false);
/// Cycles the driver burns per interrupt (a slow claimant).
static SPIN: AtomicU64 = AtomicU64::new(0);
static STAMP_TSC: AtomicU64 = AtomicU64::new(0);
static STAMP_TICK: AtomicU64 = AtomicU64::new(0);
static LATENCY: [AtomicU64; SAMPLES] = [const { AtomicU64::new(0) }; SAMPLES];
static TAKEN: AtomicUsize = AtomicUsize::new(0);
static CROSSED: AtomicU64 = AtomicU64::new(0);
static ACKS: AtomicU64 = AtomicU64::new(0);
static ACK_FAILURES: AtomicU64 = AtomicU64::new(0);
/// Messages found queued behind the one just received (must stay 0).
static EXTRA: AtomicU64 = AtomicU64::new(0);
static RAISED_DELTA: AtomicU64 = AtomicU64::new(0);

fn reset_counters() {
    for value in [
        &STAMP_TSC,
        &STAMP_TICK,
        &CROSSED,
        &ACKS,
        &ACK_FAILURES,
        &EXTRA,
        &SPIN,
    ] {
        value.store(0, Ordering::Relaxed);
    }
    TAKEN.store(0, Ordering::Relaxed);
    NOISY.store(false, Ordering::Relaxed);
}

/// The driver: receive an interrupt message, handle the device, ack.
extern "C" fn driver() -> ! {
    let me = task::current();
    GATE.wait(me, None);
    loop {
        let endpoint = ENDPOINT.load(Ordering::Relaxed);
        if channels::recv(endpoint, None).is_err() {
            GATE.wait(me, None);
            continue;
        }
        let stamp = STAMP_TSC.swap(0, Ordering::Relaxed);
        if stamp != 0 {
            let index = TAKEN.fetch_add(1, Ordering::Relaxed);
            if let Some(slot) = LATENCY.get(index) {
                slot.store(crate::perf::rdtsc().wrapping_sub(stamp), Ordering::Relaxed);
            }
            if task::ticks() != STAMP_TICK.load(Ordering::Relaxed) {
                CROSSED.fetch_add(1, Ordering::Relaxed);
            }
        }
        if matches!(channels::try_recv(endpoint), Ok(Some(_))) {
            EXTRA.fetch_add(1, Ordering::Relaxed);
        }
        let spin = SPIN.load(Ordering::Relaxed);
        let start = crate::perf::rdtsc();
        while crate::perf::rdtsc().wrapping_sub(start) < spin {
            core::hint::spin_loop();
        }
        // SAFETY: COM1's IIR read acknowledges the transmit-empty interrupt,
        // and clearing IER stops it.
        unsafe {
            let _ = inb(IIR);
            outb(IER, 0);
        }
        if sys(OP_IRQ_ACK, HANDLE.load(Ordering::Relaxed), 0, 0, 0) < 0 {
            ACK_FAILURES.fetch_add(1, Ordering::Relaxed);
        }
        ACKS.fetch_add(1, Ordering::Relaxed);
        if NOISY.load(Ordering::Relaxed) {
            // SAFETY: re-enabling the transmit-empty interrupt with the
            // register empty raises it again at once, on the unmasked line.
            unsafe { outb(IER, IER_THRE) };
        }
        KERNEL.notify_one();
    }
}

/// A device on `line` claimed by a driver thread parked in `recv`; returns
/// the thread's slot and the device.
fn claimed_driver(fx: &Fixture, line: u8) -> Result<(usize, DeviceId), String> {
    let dev = add_device(Spec::nic(Some(line)))?;
    let slot = task::kthread::spawn_kernel_thread("irq-driver", driver, PriorityClass::Interactive)
        .map_err(|e| format!("spawn: {e}"))?;
    while !GATE.contains(slot) {
        task::switch::yield_now();
    }
    handles::reset_for_task(slot);
    crate::ipc::credentials::set(slot, driver_cred());
    enter(slot)?;
    let (endpoint, _peer) = irq_channel()?;
    let handle = expect_ok(claim_irq(dev, endpoint, false), "claim")?;
    expect_ok(sys(OP_IRQ_ENABLE, handle, 0, 0, 0), "irq_enable")?;
    leave(fx);
    ENDPOINT.store(endpoint, Ordering::Relaxed);
    HANDLE.store(handle, Ordering::Relaxed);
    GATE.notify_one();
    // Let it reach `recv` (it parks on the Messenger queue, not the gate).
    for _ in 0..8 {
        task::switch::yield_now();
    }
    check!(
        matches!(
            task::harness::state(slot),
            Some(crate::task::TaskState::Blocked { .. })
        ),
        "the driver is not parked in recv: {:?}",
        task::harness::state(slot)
    );
    Ok((slot, dev))
}

fn stop_driver(slot: usize) {
    task::harness::switch_current(task::KERNEL_TASK);
    channels::forget_task(slot);
    task::harness::finish(slot, 0);
    GATE.notify_all();
    KERNEL.notify_all();
    while task::reap_child().is_some() {}
}

/// Run `body` with only the timer and the cascade unmasked (the claim
/// unmasks the UART line itself), and the UART's interrupt enables restored.
fn isolated<T>(body: impl FnOnce() -> T) -> T {
    let saved: [bool; 16] = core::array::from_fn(|line| irqchip::is_masked(line as u8));
    // SAFETY: reading COM1's IER has no side effect.
    let ier = unsafe { inb(IER) };
    for line in 0..16u8 {
        irqchip::set_masked(line, !matches!(line, 0 | 2));
    }
    let out = body();
    // SAFETY: restoring COM1's interrupt enables as they were.
    unsafe { outb(IER, ier) };
    for (line, masked) in saved.into_iter().enumerate() {
        irqchip::set_masked(line as u8, masked);
    }
    out
}

fn micros(cycles: u64) -> u64 {
    cycles.saturating_mul(10_000) / crate::arch::clock::cycles_per_tick().max(1)
}

/// IRQ to driver on an idle CPU: the kernel task halts, the UART interrupt
/// arrives, and the claimed driver must run inside the same tick, every time.
/// Before P1.2 nothing ran the bottom half here at all (no syscall, no mux),
/// so each wait timed out.
pub fn irq_prompt_idle_claimant() -> Result<(), String> {
    let fx = Fixture::new()?;
    reset_counters();
    isolated(|| -> Result<(), String> {
        let (slot, _) = claimed_driver(&fx, UART_LINE)?;
        let mut result = Ok(());
        for sample in 0..SAMPLES {
            STAMP_TICK.store(task::ticks(), Ordering::Relaxed);
            STAMP_TSC.store(crate::perf::rdtsc(), Ordering::Relaxed);
            // SAFETY: arm the transmit-empty interrupt; the driver clears it.
            unsafe { outb(IER, IER_THRE) };
            let reason = KERNEL.wait(task::KERNEL_TASK, Some(task::ticks() + 50));
            if reason != WakeReason::Woken {
                result = Err(format!("sample {sample}: no driver wake ({reason:?})"));
                break;
            }
        }
        stop_driver(slot);
        result
    })?;
    let taken = TAKEN.load(Ordering::Relaxed).min(SAMPLES);
    check!(
        taken == SAMPLES,
        "only {taken} of {SAMPLES} interrupts reached the driver"
    );
    let mut values: Vec<u64> = LATENCY.iter().map(|v| v.load(Ordering::Relaxed)).collect();
    values.sort_unstable();
    let crossed = CROSSED.load(Ordering::Relaxed);
    serial_println!(
        "TEST:dev_irq_prompt_idle_claimant:INFO:samples={SAMPLES} p50_us={} p99_us={} max_us={} crossed_tick={crossed}",
        micros(values[SAMPLES / 2]),
        micros(values[SAMPLES * 99 / 100]),
        micros(values[SAMPLES - 1]),
    );
    check!(
        crossed <= (SAMPLES / 10) as u64,
        "{crossed} of {SAMPLES} deliveries waited for a timer tick"
    );
    check!(
        EXTRA.load(Ordering::Relaxed) == 0,
        "more than one message was queued"
    );
    check!(ACK_FAILURES.load(Ordering::Relaxed) == 0, "an ack failed");
    Ok(())
}

/// An interrupt storm with a slow claimant: the UART asserts again the
/// moment each ack unmasks its line, and the driver burns ~0.2 ms per
/// interrupt. The kernel task must keep
/// getting the CPU and the clock must keep ticking, the driver's queue must
/// never hold more than one message, and once the clock goes quiet nothing
/// is left owed or queued.
pub fn irq_prompt_storm_slow_claimant() -> Result<(), String> {
    const SLEEPS: u64 = 50;
    let fx = Fixture::new()?;
    reset_counters();
    let (elapsed, acks) = isolated(|| -> Result<(u64, u64), String> {
        let (slot, dev) = claimed_driver(&fx, UART_LINE)?;
        NOISY.store(true, Ordering::Relaxed);
        SPIN.store(
            crate::arch::clock::cycles_per_tick() / 50,
            Ordering::Relaxed,
        );
        // SAFETY: arm the transmit-empty interrupt; the noisy driver re-arms it.
        unsafe { outb(IER, IER_THRE) };
        let start = task::ticks();
        let raised_before = irq::stats().raised;
        for _ in 0..SLEEPS {
            task::wait_sleep(task::ticks() + 1);
        }
        let elapsed = task::ticks() - start;
        RAISED_DELTA.store(
            u64::from(irq::stats().raised - raised_before),
            Ordering::Relaxed,
        );
        // The device goes quiet: an interrupt still in flight is served (and
        // quieted) by the driver.
        NOISY.store(false, Ordering::Relaxed);
        task::wait_sleep(task::ticks() + 5);
        let acks = ACKS.load(Ordering::Relaxed);
        let claim = CLAIMS.lock().get(dev).map(|c| (c.pending, c.missed));
        enter(slot)?;
        let left = queued(ENDPOINT.load(Ordering::Relaxed))?;
        leave(&fx);
        stop_driver(slot);
        check!(
            claim == Some((false, false)),
            "after the storm the claim is {claim:?}"
        );
        check!(left == 0, "{left} interrupt messages were left queued");
        Ok((elapsed, acks))
    })?;
    serial_println!(
        "TEST:dev_irq_prompt_storm_slow_claimant:INFO:sleeps={SLEEPS} elapsed_ticks={elapsed} interrupts_handled={acks} raised={}",
        RAISED_DELTA.load(Ordering::Relaxed),
    );
    check!(
        elapsed <= SLEEPS * 3,
        "{SLEEPS} one-tick sleeps took {elapsed} ticks under the storm"
    );
    // A storm, not a trickle: many interrupts per tick reached the driver.
    check!(
        acks >= SLEEPS * 5,
        "only {acks} interrupts were handled in {elapsed} ticks"
    );
    check!(
        EXTRA.load(Ordering::Relaxed) == 0,
        "more than one message was queued"
    );
    check!(ACK_FAILURES.load(Ordering::Relaxed) == 0, "an ack failed");
    Ok(())
}

/// Inside an interrupt, a raise that nobody can be told of (the only
/// claimant is a laggard whose round expired) keeps its line masked and is
/// handed to the next task-context pass, which lets the line go: unmasking
/// inside the interrupt would let a device nobody quiets re-enter forever.
pub fn irq_prompt_laggard_stays_masked_in_interrupt() -> Result<(), String> {
    let fx = Fixture::new()?;
    let r = super::irq::rig(LINE_A, false, true)?;
    leave(&fx);
    fire(LINE_A, 1_000);
    // The driver never acks: its round expires and the line is let go.
    intx::service_at(1_000 + intx::ACK_DEADLINE_TICKS);
    check!(!masked(LINE_A), "the expired round left the line masked");
    for round in 0..1_000u64 {
        irq::dispatch(LINE_A);
        intx::service_in_interrupt_at(2_000 + round);
        check!(
            masked(LINE_A),
            "round {round}: unmasked inside the interrupt"
        );
    }
    intx::service_at(3_000);
    check!(
        !masked(LINE_A),
        "the task-context pass did not let the line go"
    );
    check!(
        r.flags()? == (true, true, true),
        "claim state {:?}",
        r.flags()?
    );
    check!(
        r.queued()? == 1,
        "the laggard was posted {} messages",
        r.queued()?
    );
    leave(&fx);
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("dev_irq_prompt_idle_claimant", irq_prompt_idle_claimant),
    (
        "dev_irq_prompt_storm_slow_claimant",
        irq_prompt_storm_slow_claimant,
    ),
    (
        "dev_irq_prompt_laggard_stays_masked_in_interrupt",
        irq_prompt_laggard_stays_masked_in_interrupt,
    ),
];
