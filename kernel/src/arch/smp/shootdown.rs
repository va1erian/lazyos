//! The cross-CPU half of a TLB shootdown (docs/architecture/smp.md, "TLB
//! shootdown"): the per-target mailbox, the doorbell IPI and its handler,
//! and the rules that keep several senders from deadlocking each other.
//!
//! **One sender at a time.** A sender takes [`SENDER`], a spin lock, with
//! interrupts off, then posts to every target and waits for every ack. A
//! mailbox therefore has one writer, and the previous holder waited for
//! every ack before releasing it, so a request is never overwritten before
//! it is served (the generation pair `sd_req`/`sd_ack` checks it).
//!
//! **No deadlock while waiting.** A CPU waiting for the lock may itself be a
//! target of the holder, and it waits with interrupts off (the doorbell
//! cannot land): its wait loop serves its own mailbox ([`serve_pending`]),
//! so the holder's ack arrives anyway. The holder waits only for acks, which
//! every online CPU produces from its handler (interrupts on) or from such a
//! loop. **Rule for S8.3 and later:** any loop that spins with interrupts
//! off on an online CPU, waiting for another CPU, calls [`serve_pending`]
//! (the entry lock's spin is the first one), or a shootdown sent to it never
//! completes.
//!
//! **Serving is idempotent.** A request served from a wait loop leaves its
//! doorbell pending; when it lands, the handler finds `sd_req == sd_ack` and
//! does nothing. The handler and a wait loop on one CPU cannot interleave
//! harmfully either: both read the same request and store the same ack.

use core::sync::atomic::{fence, AtomicU32, Ordering};
use x86_64::structures::idt::InterruptStackFrameValue;
use x86_64::VirtAddr;

use super::{rdtsc, spin_loop, SHOOTDOWN_VECTOR};
use crate::arch::gsgate::irq_gate;
use crate::arch::{lapic, percpu};
use crate::mem::tlb::FLUSH_ALL_ABOVE;

irq_gate!(shootdown_gate => shootdown_handler);

/// The sender lock: 0 when free, else the holder's area index + 1.
static SENDER: AtomicU32 = AtomicU32::new(0);

/// The doorbell: serve the mailbox (if a wait loop has not already).
extern "C" fn shootdown_handler(_frame: &InterruptStackFrameValue) {
    serve();
    lapic::eoi();
}

/// Invalidate `[addr, addr + pages * 4096)` on this CPU: the "everything"
/// case (`mem::tlb::shootdown_all`, 0/0), and any range too long to walk
/// page by page, is a full flush (never truncate: an entry left stale here
/// is a use-after-free).
fn flush(addr: u64, pages: u64) {
    if (addr == 0 && pages == 0) || pages > FLUSH_ALL_ABOVE {
        x86_64::instructions::tlb::flush_all();
    } else {
        for page in 0..pages {
            x86_64::instructions::tlb::flush(VirtAddr::new(addr + page * 4096));
        }
    }
}

/// Serve this CPU's pending request, if any; whether one was served.
fn serve() -> bool {
    let ipi = &percpu::current().ipi;
    // Acquire: the sender wrote the range before publishing the generation.
    let request = ipi.sd_req.load(Ordering::Acquire);
    if request == ipi.sd_ack.load(Ordering::Relaxed) {
        return false;
    }
    flush(
        ipi.sd_addr.load(Ordering::Relaxed),
        ipi.sd_pages.load(Ordering::Relaxed),
    );
    // Release, after the flush (`invlpg` and the `CR3` write serialize): the
    // sender may free the frame once it sees this.
    ipi.sd_ack.store(request, Ordering::Release);
    true
}

/// What a CPU spinning with interrupts off owes the others: serve a
/// shootdown posted to it, and halt if another CPU is panicking (the `STOP`
/// it was sent cannot land either). See the module rule.
pub fn serve_pending() {
    serve();
    super::ipi::halt_if_stopping();
}

/// The machine's shootdowns are sent one at a time; this is the turn.
struct Turn;

impl Turn {
    /// Wait for the sender lock, serving this CPU's own mailbox meanwhile.
    /// The holder's ack wait is bounded at a second, so two seconds here is
    /// a lock that will never be released.
    fn take(me: u32) -> Turn {
        let started = rdtsc();
        let bound = crate::arch::clock::cycles_per_tick() * 200;
        loop {
            match SENDER.compare_exchange_weak(0, me + 1, Ordering::Acquire, Ordering::Relaxed) {
                Ok(_) => return Turn,
                Err(holder) if holder == me + 1 => {
                    panic!("shootdown: CPU{me} re-entered its own send")
                }
                Err(holder) => {
                    serve_pending();
                    if rdtsc().wrapping_sub(started) > bound {
                        panic!(
                            "shootdown: CPU{me} waited 2 s for the sender lock (held by CPU{})",
                            holder.wrapping_sub(1)
                        );
                    }
                    spin_loop();
                }
            }
        }
    }
}

impl Drop for Turn {
    fn drop(&mut self) {
        SENDER.store(0, Ordering::Release);
    }
}

/// `mem::tlb::shootdown`'s cross-CPU half: post `[addr, addr + pages *
/// 4096)` to every other online CPU, ring each doorbell, and wait for every
/// ack (a calibrated second each, then a panic naming the CPU). Safe from
/// any CPU and any context but an NMI: interrupts are off for the whole
/// exchange, so an interrupt handler on this CPU cannot start a second send
/// in the middle of this one (or of its two-write xAPIC ICR sends).
pub fn shootdown_others(addr: u64, pages: u64) {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let me = percpu::current().index();
        let _turn = Turn::take(me);
        // Pairs with `ap_entry`'s fence between its `online` store and its
        // full flush: either this scan sees the new CPU online (and posts to
        // it), or that CPU's flush comes after the caller's page-table
        // change (and dropped any stale entry). The store-buffer pattern
        // needs a full fence on both sides.
        fence(Ordering::SeqCst);
        let posted = post_all(me, addr, pages);
        wait_all(posted, addr, pages);
    });
}

/// Post the request to every other online CPU; the set posted to.
fn post_all(me: u32, addr: u64, pages: u64) -> u64 {
    let mut posted = 0u64;
    for index in 0..percpu::MAX_CPUS {
        let Some(cpu) = percpu::area(index) else {
            break;
        };
        if index as u32 == me || !cpu.online.load(Ordering::Acquire) {
            continue;
        }
        let ipi = &cpu.ipi;
        let generation = ipi.sd_req.load(Ordering::Relaxed);
        // Every earlier holder waited for its acks, so the last request is
        // served; a mismatch is a broken protocol, and overwriting would
        // lose a flush.
        assert_eq!(
            generation,
            ipi.sd_ack.load(Ordering::Acquire),
            "shootdown: CPU{index} still has an unserved request"
        );
        ipi.sd_addr.store(addr, Ordering::Relaxed);
        ipi.sd_pages.store(pages, Ordering::Relaxed);
        ipi.sd_req
            .store(generation.wrapping_add(1), Ordering::Release);
        lapic::send_fixed(cpu.apic_id(), SHOOTDOWN_VECTOR);
        posted |= 1 << index;
    }
    posted
}

/// Wait until every CPU in `posted` acknowledged its request.
fn wait_all(posted: u64, addr: u64, pages: u64) {
    // A second of the calibrated clock (the bring-up refuses to start APs
    // without one, so it is never 0 here).
    let bound = crate::arch::clock::cycles_per_tick() * 100;
    for index in (0..percpu::MAX_CPUS).filter(|i| posted & (1 << i) != 0) {
        let Some(cpu) = percpu::area(index) else {
            continue;
        };
        let started = rdtsc();
        let want = cpu.ipi.sd_req.load(Ordering::Relaxed);
        // A CPU taken offline meanwhile (the suite's `STOP`) is halted with
        // interrupts off and starts again through `ap_entry`'s full flush.
        while cpu.ipi.sd_ack.load(Ordering::Acquire) != want && cpu.online.load(Ordering::Acquire) {
            // Nobody can post to the holder, but a panic elsewhere must
            // still stop it.
            super::ipi::halt_if_stopping();
            if rdtsc().wrapping_sub(started) > bound {
                panic!(
                    "shootdown: CPU{index} apic={:#x} never acknowledged ({pages} pages at {addr:#x})",
                    cpu.apic_id()
                );
            }
            spin_loop();
        }
    }
}
