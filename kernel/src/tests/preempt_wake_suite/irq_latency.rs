//! A real interrupt wakes a thread while the CPU is halted.
//!
//! The kernel task arms the COM1 UART's transmit-holding-register-empty
//! interrupt (line 4; the register is already empty, so it fires as soon as
//! interrupts come on) and parks. Its wait loop halts with interrupts on, the
//! UART interrupt arrives, a kernel handler wakes a parked thread, and the
//! thread records how long after the interrupt it got the CPU, then wakes
//! the kernel task again. Before P1.1 the halted kernel task halted again
//! and the thread waited for the next timer tick (every sample crossed a
//! tick); now the interrupt return hands it the CPU at once.

use super::*;
use crate::arch::io::{inb, outb};
use crate::arch::irqchip;
use crate::dev::irq;

const COM1: u16 = 0x3F8;
const IER: u16 = COM1 + 1;
const IIR: u16 = COM1 + 2;
/// Interrupt enable: transmit holding register empty.
const IER_THRE: u8 = 0x02;
const LINE: u8 = 4;
const SAMPLES: usize = 200;

static IRQ_TSC: AtomicU64 = AtomicU64::new(0);
static IRQ_TICK: AtomicU64 = AtomicU64::new(0);
static LATENCY: [AtomicU64; SAMPLES] = [const { AtomicU64::new(0) }; SAMPLES];
static TAKEN: AtomicUsize = AtomicUsize::new(0);
static CROSSED: AtomicU64 = AtomicU64::new(0);

/// The kernel handler for line 4: quiet the UART, stamp, wake the thread.
fn uart_irq(_line: u8) {
    // SAFETY: COM1's IER and IIR; clearing IER stops the transmit-empty
    // source and reading IIR acknowledges it. Nothing else drives the UART
    // with interrupts while this test runs.
    unsafe {
        outb(IER, 0);
        let _ = inb(IIR);
    }
    IRQ_TSC.store(crate::perf::rdtsc(), Ordering::Relaxed);
    IRQ_TICK.store(task::ticks(), Ordering::Relaxed);
    THREAD_QUEUE.notify_one();
}

/// The "driver": record the latency of each interrupt-driven wake.
extern "C" fn waiter() -> ! {
    let me = task::current();
    loop {
        RUNS.fetch_add(1, Ordering::Relaxed);
        THREAD_QUEUE.wait(me, None);
        let stamp = IRQ_TSC.swap(0, Ordering::Relaxed);
        if stamp != 0 {
            let index = TAKEN.fetch_add(1, Ordering::Relaxed);
            if let Some(slot) = LATENCY.get(index) {
                slot.store(crate::perf::rdtsc().wrapping_sub(stamp), Ordering::Relaxed);
            }
            if task::ticks() != IRQ_TICK.load(Ordering::Relaxed) {
                CROSSED.fetch_add(1, Ordering::Relaxed);
            }
        }
        KERNEL_QUEUE.notify_one();
    }
}

/// Run `body` with only the timer, the cascade and `LINE` unmasked.
fn with_lines<T>(body: impl FnOnce() -> T) -> T {
    let saved: [bool; 16] = core::array::from_fn(|line| irqchip::is_masked(line as u8));
    for line in 0..16u8 {
        irqchip::set_masked(line, !matches!(line, 0 | 2));
    }
    let out = body();
    for (line, masked) in saved.into_iter().enumerate() {
        irqchip::set_masked(line as u8, masked);
    }
    out
}

fn micros(cycles: u64) -> u64 {
    let per_tick = crate::arch::clock::cycles_per_tick().max(1);
    cycles.saturating_mul(10_000) / per_tick
}

/// `SAMPLES` UART interrupts while the CPU is halted: each must wake the
/// thread within its own tick, and the report gives the latency.
pub fn irq_wake_idle_latency() -> Result<(), String> {
    fresh();
    TAKEN.store(0, Ordering::Relaxed);
    CROSSED.store(0, Ordering::Relaxed);
    IRQ_TSC.store(0, Ordering::Relaxed);
    let slot = start_thread(waiter, PriorityClass::Interactive)?;
    // SAFETY: reading COM1's IER has no side effect.
    let saved_ier = unsafe { inb(IER) };
    let outcome = with_lines(|| -> Result<(), String> {
        irq::register_kernel(LINE, uart_irq).map_err(|e| format!("register line {LINE}: {e:?}"))?;
        let mut result = Ok(());
        for sample in 0..SAMPLES {
            // SAFETY: arming the transmit-empty interrupt; the handler clears
            // it again on the first delivery.
            unsafe { outb(IER, IER_THRE) };
            let reason = KERNEL_QUEUE.wait(task::KERNEL_TASK, Some(task::ticks() + 50));
            if reason != crate::task::WakeReason::Woken {
                result = Err(format!(
                    "sample {sample}: the kernel task woke with {reason:?}"
                ));
                break;
            }
        }
        irq::unregister_kernel(LINE);
        result
    });
    // SAFETY: restoring COM1's interrupt enables as the boot left them.
    unsafe { outb(IER, saved_ier) };
    outcome?;
    let taken = TAKEN.load(Ordering::Relaxed).min(SAMPLES);
    check!(
        taken == SAMPLES,
        "only {taken} of {SAMPLES} interrupts reached the thread"
    );
    let mut values: Vec<u64> = LATENCY.iter().map(|v| v.load(Ordering::Relaxed)).collect();
    values.sort_unstable();
    let crossed = CROSSED.load(Ordering::Relaxed);
    serial_println!(
        "TEST:task_preempt_irq_wake_idle_latency:INFO:samples={SAMPLES} p50_us={} p99_us={} max_us={} crossed_tick={crossed}",
        micros(values[SAMPLES / 2]),
        micros(values[SAMPLES * 99 / 100]),
        micros(values[SAMPLES - 1]),
    );
    // A tick can fall between an interrupt and the switch by coincidence;
    // waiting for one (the old behaviour) makes every sample cross.
    check!(
        crossed <= (SAMPLES / 10) as u64,
        "{crossed} of {SAMPLES} interrupt wakes waited for a timer tick"
    );
    stop_thread(slot)
}
