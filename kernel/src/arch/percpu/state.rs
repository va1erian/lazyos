//! The members of a per-CPU area (`arch::percpu::Cpu`), each with its
//! all-zero value. Everything here is an atomic or an `UnsafeCell`: a
//! `&Cpu` is shared — the owning CPU, its interrupt handlers and (for the
//! atomics) other CPUs all hold one — so nothing in it may be mutated
//! through a plain field (docs/architecture/smp.md, "Per-CPU areas").

// The `ZERO` consts are interior-mutable on purpose: each is a byte pattern
// the static initializer copies, never a shared constant read.
#![allow(clippy::declare_interior_mutable_const)]

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, AtomicU8};

use crate::arch::linux::UserContext;

/// The Linux `syscall` entry's transient state (was `arch/linux.rs` globals).
/// The entry asm reads and writes all three fields `gs:`-relative; the
/// layout is pinned by `percpu_entry_asm_offsets_match_the_layout`.
#[repr(C)]
pub struct SyscallEntry {
    /// Kernel stack top the next Linux `syscall` switches to (the scheduler
    /// writes it alongside the TSS `RSP0` at every switch).
    pub stack: AtomicU64,
    /// Scratch for the user RSP during entry (only touched with interrupts
    /// disabled, before dispatch can be preempted).
    pub saved_user_rsp: AtomicU64,
    /// User register state captured at `syscall` entry (Linux preserves
    /// these across the syscall; `clone` builds a child thread's first frame
    /// from them). Only its own CPU touches it: the entry asm, and
    /// `percpu::user_context`/`set_user_context` by value.
    pub user_context: UnsafeCell<UserContext>,
}

/// The interrupts-off span being charged (was `arch/irqoff.rs` globals).
#[repr(C)]
pub struct IrqOffSpan {
    /// Its start in TSC cycles (0 = none).
    pub start: AtomicU64,
    /// The syscall it is charged to (`u64::MAX` = none).
    pub nr: AtomicU64,
    /// Where it began.
    pub from: AtomicPtr<core::panic::Location<'static>>,
    /// The task whose syscall the span belongs to.
    pub task: AtomicU64,
}

/// The interrupt-window state (was `arch/irq_window.rs` globals; `open` is
/// read by `timer_isr`'s asm through `%gs`).
#[repr(C)]
pub struct WindowState {
    /// Nonzero while a window is open.
    pub open: AtomicU8,
    /// TSC when the last window closed (or the span began).
    pub last: AtomicU64,
    /// Windows opened, and ticks taken inside them, since boot.
    pub opened: AtomicU64,
    pub window_ticks: AtomicU64,
    /// Ticks taken in windows that the scheduler has not charged yet.
    pub uncharged: AtomicU64,
}

/// The wake/preemption bookkeeping (was `task/preempt.rs` globals). All of
/// it is read and written by the CPU that owns it; the atomic types are for
/// S8.3's remote wakes (a waker storing into the target CPU's
/// `need_resched`).
#[repr(C)]
pub struct PreemptState {
    /// A wake made a task more deserving of the CPU than the current one.
    pub need_resched: AtomicBool,
    /// Set by `preempt_point` for the yield it makes: the scheduler entry
    /// that follows is a preemption, not a park or a voluntary yield.
    pub preempting: AtomicBool,
    /// The task whose last selection charge is still open, and when it was
    /// charged (monotonic ns).
    pub selected: AtomicU64,
    pub selected_at: AtomicU64,
    /// The end of the selected task's minimum slice (monotonic ns).
    pub slice_end: AtomicU64,
    /// When a deserving same-class wake deferred by the minimum slice falls
    /// due (0: none pending).
    pub deferred: AtomicU64,
    /// The pending handoff: the task that will park, and the one to run then.
    pub handoff_from: AtomicU64,
    pub handoff_to: AtomicU64,
}

/// The state an IPI or NMI lands on and the bring-up stamps (S8.2).
#[repr(C)]
pub struct IpiState {
    /// The TSC this CPU read just before publishing `online` (the bring-up
    /// logs the latency the BSP sees; it is not a skew measure).
    pub tsc_entry: AtomicU64,
    /// Shootdown request (`arch::smp::shootdown`): the first virtual address
    /// and the page count to invalidate (0 and 0 = everything; a long range
    /// flushes everything), valid while `sd_req != sd_ack`.
    pub sd_addr: AtomicU64,
    pub sd_pages: AtomicU64,
    /// The generation of the last request posted to this CPU, and of the
    /// last one it served. A sender posts only when the two are equal, so a
    /// request is never overwritten before it is served.
    pub sd_req: AtomicU64,
    pub sd_ack: AtomicU64,
    /// Set by the STOP handler just before this CPU halts for good (the
    /// panicking CPU waits on it; the suite restarts the CPU afterwards).
    pub stopped: AtomicBool,
    /// NMIs this CPU's handler ran (`arch::nmi`).
    pub nmis: AtomicU64,
    /// The TSC coherence exchange (`arch::smp::tscsync`): the step counter
    /// both sides wait on, the AP's two reads and the BSP's, and whether the
    /// AP has reached its half.
    pub sy_state: AtomicU64,
    pub sy_t1: AtomicU64,
    pub sy_tb: AtomicU64,
    pub sy_t2: AtomicU64,
    pub sy_ready: AtomicBool,
    /// Test rig: cycles this AP adds to its TSC reads in the exchange.
    #[cfg(lazyos_tests)]
    pub tsc_bias: AtomicU64,
    /// Test rig: what this CPU's NMI handler does instead of the hang report
    /// (`arch::nmi::TestMode`; 0 = the real report).
    #[cfg(lazyos_tests)]
    pub nmi_test: AtomicU8,
    /// Test rig: the points of `ap_entry` before its IDT load where this AP
    /// stops for the BSP to send it an NMI (a bit each), and the point it
    /// is stopped at (0: running).
    #[cfg(lazyos_tests)]
    pub early_pause: AtomicU8,
    #[cfg(lazyos_tests)]
    pub early_paused: AtomicU8,
    /// Test rig: a function for this CPU to run in its next reschedule IPI
    /// (0: none), its argument, its result and a completion generation. The
    /// suite uses it to make an AP touch memory on demand, which is the only
    /// way to prove its TLB holds (or no longer holds) a translation.
    #[cfg(lazyos_tests)]
    pub job: AtomicU64,
    #[cfg(lazyos_tests)]
    pub job_arg: AtomicU64,
    #[cfg(lazyos_tests)]
    pub job_result: AtomicU64,
    #[cfg(lazyos_tests)]
    pub job_done: AtomicU64,
}

impl SyscallEntry {
    pub(super) const ZERO: SyscallEntry = SyscallEntry {
        stack: AtomicU64::new(0),
        saved_user_rsp: AtomicU64::new(0),
        user_context: UnsafeCell::new(UserContext::ZEROED),
    };
}

impl IrqOffSpan {
    pub(super) const ZERO: IrqOffSpan = IrqOffSpan {
        start: AtomicU64::new(0),
        nr: AtomicU64::new(0),
        from: AtomicPtr::new(core::ptr::null_mut()),
        task: AtomicU64::new(0),
    };
}

impl WindowState {
    pub(super) const ZERO: WindowState = WindowState {
        open: AtomicU8::new(0),
        last: AtomicU64::new(0),
        opened: AtomicU64::new(0),
        window_ticks: AtomicU64::new(0),
        uncharged: AtomicU64::new(0),
    };
}

impl PreemptState {
    pub(super) const ZERO: PreemptState = PreemptState {
        need_resched: AtomicBool::new(false),
        preempting: AtomicBool::new(false),
        selected: AtomicU64::new(0),
        selected_at: AtomicU64::new(0),
        slice_end: AtomicU64::new(0),
        deferred: AtomicU64::new(0),
        handoff_from: AtomicU64::new(0),
        handoff_to: AtomicU64::new(0),
    };
}

impl IpiState {
    pub(super) const ZERO: IpiState = IpiState {
        tsc_entry: AtomicU64::new(0),
        sd_addr: AtomicU64::new(0),
        sd_pages: AtomicU64::new(0),
        sd_req: AtomicU64::new(0),
        sd_ack: AtomicU64::new(0),
        stopped: AtomicBool::new(false),
        nmis: AtomicU64::new(0),
        sy_state: AtomicU64::new(0),
        sy_t1: AtomicU64::new(0),
        sy_tb: AtomicU64::new(0),
        sy_t2: AtomicU64::new(0),
        sy_ready: AtomicBool::new(false),
        #[cfg(lazyos_tests)]
        tsc_bias: AtomicU64::new(0),
        #[cfg(lazyos_tests)]
        nmi_test: AtomicU8::new(0),
        #[cfg(lazyos_tests)]
        early_pause: AtomicU8::new(0),
        #[cfg(lazyos_tests)]
        early_paused: AtomicU8::new(0),
        #[cfg(lazyos_tests)]
        job: AtomicU64::new(0),
        #[cfg(lazyos_tests)]
        job_arg: AtomicU64::new(0),
        #[cfg(lazyos_tests)]
        job_result: AtomicU64::new(0),
        #[cfg(lazyos_tests)]
        job_done: AtomicU64::new(0),
    };
}
