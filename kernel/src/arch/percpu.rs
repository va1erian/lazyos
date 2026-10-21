//! Per-CPU areas: the state that is "the CPU's", not the kernel's
//! (docs/smp-plan.md S8.1).
//!
//! Everything the entry paths and the scheduler touch before they can name a
//! task — the ring-0 stack a user trap switches to, the captured syscall
//! registers, the current task slot, the preemption bookkeeping, the
//! interrupts-off span and the interrupt-window state — lives in one `Cpu`
//! struct per CPU, addressed through `%gs`. The base of `%gs` is the area's
//! own address, and `self_ptr` sits at offset 0, so `current()` is a single
//! `gs:[0]` load.
//!
//! **No `swapgs`.** The kernel deliberately runs every privilege level with
//! the same `%gs` base: user code never touches `%gs` (`arch_prctl`'s
//! `ARCH_SET_GS` is a stub, no vDSO, nothing writes `IA32_KERNEL_GS_BASE`),
//! and a task can block inside a syscall, which made per-task GS state
//! unsound (`arch/linux.rs`'s old note). The consequence is that every
//! entry path — syscall, gate, fault, IRQ, NMI — can read per-CPU state
//! directly, with no CS test and no paranoid-entry rule, and a task that
//! resumes on another CPU (S8.3) finds that CPU's area there. If user `%gs`
//! support is ever added, this decision must be revisited with `swapgs`
//! discipline at every entry; the audit row records it.
//!
//! The areas are one static array of [`MAX_CPUS`]; a CPU is brought online by
//! writing its `self_ptr`, building its GDT/TSS (`gdt::init_cpu`) and
//! pointing its own `IA32_GS_BASE` at the area (BSP in [`init`], the APs in
//! S8.2's `ap_entry`). Before `init`, nothing reads per-CPU state: the boot
//! runs with interrupts off and the scheduler has not started.

use core::arch::asm;
use core::mem::offset_of;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, AtomicU8, Ordering};
use x86_64::structures::gdt::GlobalDescriptorTable;
use x86_64::structures::tss::TaskStateSegment;

use super::linux::UserContext;
use super::msr;

/// Per-CPU areas that exist (the AP bring-up caps the count here;
/// `limit.cpu_max` clamps below it).
pub const MAX_CPUS: usize = 64;

/// The ring-0 stack sizes (moved here from `arch/gdt.rs`: they are the
/// per-CPU stacks' sizes).
pub const KERNEL_STACK_SIZE: usize = 32 * 1024;
pub const DOUBLE_FAULT_STACK_SIZE: usize = 4 * 1024 * 5;

/// The Linux `syscall` entry's transient state (was `arch/linux.rs` globals).
#[repr(C)]
pub struct SyscallEntry {
    /// Kernel stack top the next Linux `syscall` switches to (the scheduler
    /// writes it alongside the TSS `RSP0` at every switch).
    pub stack: u64,
    /// Scratch for the user RSP during entry (only touched with interrupts
    /// disabled, before dispatch can be preempted).
    pub saved_user_rsp: u64,
    /// User register state captured at `syscall` entry (Linux preserves
    /// these across the syscall; `clone` builds a child thread's first frame
    /// from them).
    pub user_context: UserContext,
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

/// The state an IPI lands on and the bring-up stamps (S8.2).
#[repr(C)]
pub struct IpiState {
    /// The TSC this CPU read as its first kernel instruction (the bring-up's
    /// coherence check compares it against the BSP's).
    pub tsc_entry: AtomicU64,
    /// Shootdown request: the first virtual address to invalidate (0 = none)
    /// and the page count, then the generation the sender bumped.
    pub sd_addr: AtomicU64,
    pub sd_pages: AtomicU64,
    /// The last shootdown generation this CPU acknowledged.
    pub sd_ack: AtomicU64,
}

/// One CPU's state. `self_ptr` must stay first: the asm and `current()`
/// address the area itself through `gs:[0]`.
#[repr(C)]
pub struct Cpu {
    pub self_ptr: u64,
    /// Index in [`AREAS`] (0 on the boot CPU).
    pub index: u32,
    /// The APIC ID this area belongs to (filled from the MADT at S8.2).
    pub apic_id: u32,
    /// Set when the CPU finished its init (the BSP in [`init`]; the APs in
    /// S8.2's `ap_entry`): published `Release` by its own CPU, read `Acquire`
    /// by the bring-up's poll.
    pub online: AtomicBool,
    /// The task slot currently running here (`u64`-shaped so the
    /// `usize::MAX` sentinels of the old `preempt` statics survive the move;
    /// read with `task::current()`).
    pub current: AtomicU64,
    pub preempt: PreemptState,
    pub irqoff: IrqOffSpan,
    pub window: WindowState,
    pub syscall: SyscallEntry,
    /// The state an IPI lands on and the bring-up stamps (S8.2): the entry
    /// TSC (the coherence check), the shootdown request and its ack.
    pub ipi: IpiState,
    /// The ring-0 stack a user trap switches to (`RSP0`), and the
    /// double-fault stack (IST 0). Per CPU since the TSS is: `RSP0` is
    /// rewritten at every task switch. Written by `gdt::init_cpu` before the
    /// GDT that names it is loaded; read through [`Cpu::tss`] after that.
    pub tss: MaybeUninit<TaskStateSegment>,
    pub gdt: GlobalDescriptorTable,
    pub kernel_stack: [u8; KERNEL_STACK_SIZE],
    pub double_fault_stack: [u8; DOUBLE_FAULT_STACK_SIZE],
}

impl Cpu {
    /// This CPU's TSS, after `gdt::init_cpu` wrote it. Panics (uninitialized
    /// read) if per-CPU state is touched before `percpu::init`/`init_cpu` —
    /// which is a boot-ordering bug the suite's first case catches.
    pub fn tss(&mut self) -> &mut TaskStateSegment {
        // SAFETY: the TSS is plain data zeroed in the static and written by
        // `init_cpu` before the GDT naming it is loaded; every accessor
        // runs after that (the boot order pins it, `percpu_suite` checks it).
        unsafe { self.tss.assume_init_mut() }
    }
    /// The all-zero area: **every byte of this value is zero**, so the
    /// static array of [`MAX_CPUS`] lands in `.bss` (64 × ~52 KiB would be
    /// 3.3 MiB of image otherwise). The `u64::MAX` sentinels and the TSS's
    /// `iomap_base` are written by [`init`] for every area at boot.
    // The atomics are interior-mutable on purpose: this const is a byte
    // pattern the static initializer copies, never a shared constant read.
    #[allow(clippy::declare_interior_mutable_const)]
    const ZERO: Cpu = Cpu {
        self_ptr: 0,
        index: 0,
        apic_id: 0,
        online: AtomicBool::new(false),
        current: AtomicU64::new(0),
        preempt: PreemptState {
            need_resched: AtomicBool::new(false),
            preempting: AtomicBool::new(false),
            selected: AtomicU64::new(0),
            selected_at: AtomicU64::new(0),
            slice_end: AtomicU64::new(0),
            deferred: AtomicU64::new(0),
            handoff_from: AtomicU64::new(0),
            handoff_to: AtomicU64::new(0),
        },
        irqoff: IrqOffSpan {
            start: AtomicU64::new(0),
            nr: AtomicU64::new(0),
            from: AtomicPtr::new(core::ptr::null_mut()),
            task: AtomicU64::new(0),
        },
        window: WindowState {
            open: AtomicU8::new(0),
            last: AtomicU64::new(0),
            opened: AtomicU64::new(0),
            window_ticks: AtomicU64::new(0),
            uncharged: AtomicU64::new(0),
        },
        syscall: SyscallEntry {
            stack: 0,
            saved_user_rsp: 0,
            user_context: UserContext::ZEROED,
        },
        ipi: IpiState {
            tsc_entry: AtomicU64::new(0),
            sd_addr: AtomicU64::new(0),
            sd_pages: AtomicU64::new(0),
            sd_ack: AtomicU64::new(0),
        },
        // The TSS's reserved fields are private, so a literal is out; the
        // zeroed `MaybeUninit` is plain-data zero bytes and `init_cpu`
        // writes the real `TaskStateSegment::new()` (whose `iomap_base`
        // points past the struct, denying every port bit) before loading it.
        tss: MaybeUninit::zeroed(),
        gdt: GlobalDescriptorTable::new(),
        kernel_stack: [0; KERNEL_STACK_SIZE],
        double_fault_stack: [0; DOUBLE_FAULT_STACK_SIZE],
    };

    /// Install the "nothing charged, nothing pending" sentinels the code
    /// expects (`u64::MAX` = none) into every area. Only [`init`] calls
    /// this; the zero values are never read before it (no span is open, no
    /// selection made).
    fn install_sentinels(areas: &mut [Cpu; MAX_CPUS]) {
        for area in areas.iter_mut() {
            area.preempt.selected.store(u64::MAX, Ordering::Relaxed);
            area.preempt.handoff_from.store(u64::MAX, Ordering::Relaxed);
            area.preempt.handoff_to.store(u64::MAX, Ordering::Relaxed);
            area.irqoff.nr.store(u64::MAX, Ordering::Relaxed);
        }
    }
}

// The `%gs`-relative offsets the entry asm uses. They are `offset_of!` so the
// struct and the asm cannot drift apart (a mismatch is a boot triple-fault,
// not a wrong value); `kernel/src/tests/percpu_suite.rs` asserts the ones the
// asm assumes against the layout, and the entry probe
// (`arch::linux::probe_entry`) runs the real instructions.
pub const OFF_SYSCALL_STACK: usize = offset_of!(Cpu, syscall) + offset_of!(SyscallEntry, stack);
pub const OFF_SAVED_USER_RSP: usize =
    offset_of!(Cpu, syscall) + offset_of!(SyscallEntry, saved_user_rsp);
pub const OFF_USER_CONTEXT: usize =
    offset_of!(Cpu, syscall) + offset_of!(SyscallEntry, user_context);
pub const OFF_WINDOW_OPEN: usize = offset_of!(Cpu, window) + offset_of!(WindowState, open);

static mut AREAS: [Cpu; MAX_CPUS] = [const { Cpu::ZERO }; MAX_CPUS];

/// `IA32_GS_BASE` (the `%gs` base memory operands use; `arch/msr.rs` holds
/// the kernel's other MSRs).
const IA32_GS_BASE: u32 = 0xC000_0101;

/// The address of area `index` (raw: the callers run before the area is
/// self-referential, or are the tests probing a not-yet-online area).
unsafe fn addr_of_area(index: usize) -> *mut Cpu {
    // SAFETY: a raw address computation on a static; the CPU bringing an
    // area online is the only one touching it, and it publishes `self_ptr`
    // before anything reads through `%gs`.
    unsafe { &raw mut AREAS[index] }
}

/// The boot CPU's area, addressable before `%gs` points at it: `gdt::init`
/// builds the boot CPU's GDT/TSS through this, then calls [`init`].
///
/// SAFETY (callers): single-threaded boot or an AP's own bring-up.
pub unsafe fn boot_area() -> &'static mut Cpu {
    // SAFETY: see the function contract above; the deref names the static's
    // own storage, no other reference to it exists yet.
    unsafe { &mut *addr_of_area(0) }
}

/// Publish the boot CPU's area: write its self-pointer and point `%gs`'s
/// base at it. Called twice on the boot CPU — at `kernel_main`'s first
/// statement (everything below, including `fs::init`, reads per-CPU state)
/// and again by `gdt::init` after its selector loads reset the base. An AP
/// never calls it (it would claim area 0): the BSP runs [`prepare_ap`] and
/// the AP [`set_gs_base`]. Also installs the state sentinels into every
/// area (the static is all-zero `.bss`).
pub fn init() {
    // SAFETY: single-threaded boot (the BSP only); the
    // areas are only reachable through `gs`-relative loads and these raw
    // pointers.
    unsafe {
        Cpu::install_sentinels(&mut *core::ptr::addr_of_mut!(AREAS));
        let area = addr_of_area(0);
        (*area).self_ptr = area as u64;
        (*area).index = 0;
        (*area).online.store(true, Ordering::Release);
        msr::write(IA32_GS_BASE, area as u64);
    }
}

/// Make area `index` addressable for the AP about to be started on it: its
/// self-pointer and index, written by the BSP before the SIPI (the AP's
/// first `current()` reads `gs:[0]`). Returns the area's address, which the
/// trampoline loads into the AP's `IA32_GS_BASE`. `None` for index 0 (the
/// BSP's) or out of range.
pub fn prepare_ap(index: usize) -> Option<u64> {
    if index == 0 || index >= MAX_CPUS {
        return None;
    }
    // SAFETY: the area is offline (no CPU reads it through `%gs` yet) and
    // the BSP is the only writer until the AP publishes `online`.
    unsafe {
        let area = addr_of_area(index);
        (*area).self_ptr = area as u64;
        (*area).index = index as u32;
        Some(area as u64)
    }
}

/// Point this CPU's `IA32_GS_BASE` at `area` again — an AP after
/// `gdt::init_cpu`, whose `GS` selector load reset the hidden base to 0.
pub fn set_gs_base(area: u64) {
    msr::write(IA32_GS_BASE, area);
}

/// The current CPU's area: `gs:[0]` holds the area's own address.
///
/// The reference is only valid for the CPU that fetched it, and must **not**
/// be held across anything that can re-enter the area — an interrupt window
/// (`irq_window::open`), a park, a scheduler switch — because the re-entered
/// code takes its own `&mut` to the same object (aliasing UB). Re-fetch
/// after the re-entrant point instead.
#[inline]
pub fn current() -> &'static mut Cpu {
    // SAFETY: `%gs`'s base is this CPU's `Cpu` (`init`, and the AP init in
    // S8.2) at every privilege level (module docs: no `swapgs`), and it is
    // set before the first per-CPU read. `self_ptr` at offset 0 names the
    // area, so the reference is within the static's bounds.
    unsafe {
        let area: u64;
        asm!(
            "mov {}, gs:[0]",
            out(reg) area,
            options(nostack, preserves_flags, readonly)
        );
        &mut *(area as *mut Cpu)
    }
}

/// The area of CPU `index`, for the tests (which simulate a second CPU by
/// asserting it is untouched) and the S8.2 bring-up.
#[allow(dead_code)]
pub fn area(index: usize) -> Option<&'static mut Cpu> {
    if index >= MAX_CPUS {
        return None;
    }
    // SAFETY: the same reachability contract as `current`: areas belong to
    // their CPU; a test's second-CPU probe touches only its own static.
    unsafe { Some(&mut *addr_of_area(index)) }
}
