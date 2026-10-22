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
//! **`swapgs` at every ring 3 boundary** (`arch::gsgate`). In ring 0 the
//! `%gs` base is this CPU's area; in ring 3 `IA32_KERNEL_GS_BASE` holds it,
//! because ring 3 can rewrite its own GS base (loading any DPL-3 selector
//! zeroes it) and nothing may read per-CPU state through a base the user
//! chose. The state is the CPU's, never the task's: a task that blocks in a
//! syscall and resumes on another CPU (S8.3) finds that CPU's area there.
//!
//! **Shared references only.** [`current`] and [`area`] hand out `&'static
//! Cpu`: the owning CPU, an interrupt handler that re-enters it and (S8.3) a
//! remote waker all hold one at once, so a `&mut` would alias. Every member
//! is an atomic or an `UnsafeCell`; the few that are not atomic (the TSS, the
//! GDT, the captured user context, the stacks) are reached through the raw
//! accessors below, each documenting who may touch it.
//!
//! The areas are one static array of [`MAX_CPUS`]; a CPU is brought online by
//! writing its `self_ptr`, building its GDT/TSS (`gdt::init_cpu`) and
//! pointing its own `IA32_GS_BASE` at the area (BSP in [`init`], the APs in
//! S8.2's `ap_entry`). Before `init`, nothing reads per-CPU state: the boot
//! runs with interrupts off and the scheduler has not started.

mod state;

use core::arch::asm;
use core::cell::UnsafeCell;
use core::mem::offset_of;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use x86_64::structures::gdt::GlobalDescriptorTable;
use x86_64::structures::tss::TaskStateSegment;

use super::linux::UserContext;
pub use state::{IpiState, IrqOffSpan, PreemptState, SyscallEntry, WindowState};

/// Per-CPU areas that exist (the AP bring-up caps the count here;
/// `limit.cpu_max` clamps below it).
pub const MAX_CPUS: usize = 64;

/// The ring-0 stack sizes (moved here from `arch/gdt.rs`: they are the
/// per-CPU stacks' sizes).
pub const KERNEL_STACK_SIZE: usize = 32 * 1024;
pub const DOUBLE_FAULT_STACK_SIZE: usize = 4 * 1024 * 5;
/// The NMI's own stack (IST 1): an NMI can land where `rsp` is still the
/// user's (between `syscall` and the stack switch, or `pop rsp` and
/// `sysretq`), so it must never run on the interrupted stack. Its top is the
/// landing zone the gate copies the frame out of (`arch::nmi`).
pub const NMI_STACK_SIZE: usize = 4 * 1024 * 4;

/// One CPU's state. `self_ptr` must stay first: the asm and `current()`
/// address the area itself through `gs:[0]`.
#[repr(C)]
pub struct Cpu {
    /// The area's own address (`gs:[0]`). Written before the CPU runs on it.
    pub self_ptr: AtomicU64,
    /// Index in [`AREAS`] (0 on the boot CPU).
    index: AtomicU32,
    /// The APIC ID this area belongs to (filled from the MADT at S8.2).
    apic_id: AtomicU32,
    /// Set when the CPU finished its init (the BSP in [`init`]; the APs in
    /// S8.2's `ap_entry`): published `Release` by its own CPU, read `Acquire`
    /// by the bring-up's poll.
    pub online: AtomicBool,
    /// The task slot currently running here (`u64`-shaped so the
    /// `usize::MAX` sentinels of the old `preempt` statics survive the move;
    /// read with `task::current()`).
    pub current: AtomicU64,
    /// The page table this CPU last loaded into `CR3` (`mem::switch_to`
    /// publishes it after the write), so `free_user_table` can prove no
    /// other CPU still runs on a table it frees (0: not known yet).
    pub cr3: AtomicU64,
    pub preempt: PreemptState,
    pub irqoff: IrqOffSpan,
    pub window: WindowState,
    pub syscall: SyscallEntry,
    /// The state an IPI lands on and the bring-up stamps (S8.2): the entry
    /// TSC (the coherence check), the shootdown request and its ack.
    pub ipi: IpiState,
    /// This CPU's TSS: `RSP0` (rewritten at every task switch) and the IST
    /// stacks. Written by `gdt::init_cpu` before the GDT that names it is
    /// loaded, then only by its own CPU ([`set_rsp0`]); the CPU reads it.
    tss: UnsafeCell<MaybeUninit<TaskStateSegment>>,
    /// Written fresh by every `gdt::init_cpu` (zero bytes until then: a
    /// `GlobalDescriptorTable::new()` initializer is not all-zero and would
    /// move the whole array out of `.bss`).
    gdt: UnsafeCell<MaybeUninit<GlobalDescriptorTable>>,
    /// The stacks: memory the CPU uses through `rsp`, never through Rust.
    kernel_stack: UnsafeCell<[u8; KERNEL_STACK_SIZE]>,
    double_fault_stack: UnsafeCell<[u8; DOUBLE_FAULT_STACK_SIZE]>,
    nmi_stack: UnsafeCell<[u8; NMI_STACK_SIZE]>,
}

// SAFETY: every field is an atomic or an `UnsafeCell` whose accessor below
// states who may touch it (its own CPU only, or nobody while it runs), so
// sharing `&Cpu` between CPUs is sound.
unsafe impl Sync for Cpu {}

impl Cpu {
    /// The all-zero area: **every byte of this value is zero**, so the
    /// static array of [`MAX_CPUS`] lands in `.bss` (64 × ~52 KiB would be
    /// 3.3 MiB of image otherwise). The `u64::MAX` sentinels are written by
    /// [`init`] for every area at boot; the TSS by `gdt::init_cpu`.
    // A byte pattern the static initializer copies, never a shared constant.
    #[allow(clippy::declare_interior_mutable_const)]
    const ZERO: Cpu = Cpu {
        self_ptr: AtomicU64::new(0),
        index: AtomicU32::new(0),
        apic_id: AtomicU32::new(0),
        online: AtomicBool::new(false),
        current: AtomicU64::new(0),
        cr3: AtomicU64::new(0),
        preempt: PreemptState::ZERO,
        irqoff: IrqOffSpan::ZERO,
        window: WindowState::ZERO,
        syscall: SyscallEntry::ZERO,
        ipi: IpiState::ZERO,
        // The TSS's reserved fields are private, so a literal is out; the
        // zeroed `MaybeUninit` is plain-data zero bytes and `init_cpu`
        // writes the real `TaskStateSegment::new()` (whose `iomap_base`
        // points past the struct, denying every port bit) before loading it.
        tss: UnsafeCell::new(MaybeUninit::zeroed()),
        gdt: UnsafeCell::new(MaybeUninit::zeroed()),
        kernel_stack: UnsafeCell::new([0; KERNEL_STACK_SIZE]),
        double_fault_stack: UnsafeCell::new([0; DOUBLE_FAULT_STACK_SIZE]),
        nmi_stack: UnsafeCell::new([0; NMI_STACK_SIZE]),
    };

    /// Index in [`AREAS`] (0 on the boot CPU).
    pub fn index(&self) -> u32 {
        self.index.load(Ordering::Relaxed)
    }

    /// The APIC ID this area belongs to.
    pub fn apic_id(&self) -> u32 {
        self.apic_id.load(Ordering::Relaxed)
    }

    /// Record the APIC ID of the AP about to start on this (offline) area.
    pub(super) fn set_apic_id(&self, id: u32) {
        self.apic_id.store(id, Ordering::Relaxed);
    }

    /// The top of the ring-0 stack a user trap switches to at boot (the
    /// AP's first stack too).
    pub fn kernel_stack_top(&self) -> u64 {
        self.kernel_stack.get() as u64 + KERNEL_STACK_SIZE as u64
    }

    /// The tops of the double-fault and NMI IST stacks.
    pub(super) fn double_fault_stack_top(&self) -> u64 {
        self.double_fault_stack.get() as u64 + DOUBLE_FAULT_STACK_SIZE as u64
    }
    pub(super) fn nmi_stack_top(&self) -> u64 {
        self.nmi_stack.get() as u64 + NMI_STACK_SIZE as u64
    }

    /// This area's TSS, raw. Written only by `gdt::init_cpu` (before the CPU
    /// runs on it) and by its own CPU's [`set_rsp0`]; read by the hardware.
    pub(super) fn tss_ptr(&self) -> *mut TaskStateSegment {
        self.tss.get().cast()
    }

    /// This area's GDT slot, raw: only `gdt::init_cpu` writes it.
    pub(super) fn gdt_ptr(&self) -> *mut MaybeUninit<GlobalDescriptorTable> {
        self.gdt.get()
    }

    /// The installed `RSP0` (the suite checks [`set_rsp0`] reached it).
    #[cfg(lazyos_tests)]
    pub fn rsp0(&self) -> u64 {
        // SAFETY: a read of plain data `init_cpu` wrote; the suite calls it
        // on its own CPU's area, whose only writer is that CPU.
        unsafe { (*self.tss_ptr()).privilege_stack_table[0].as_u64() }
    }

    /// Install the "nothing charged, nothing pending" sentinels the code
    /// expects (`u64::MAX` = none). Only [`init`] calls this; the zero
    /// values are never read before it (no span is open, no selection made).
    fn install_sentinels(&self) {
        self.preempt.selected.store(u64::MAX, Ordering::Relaxed);
        self.preempt.handoff_from.store(u64::MAX, Ordering::Relaxed);
        self.preempt.handoff_to.store(u64::MAX, Ordering::Relaxed);
        self.irqoff.nr.store(u64::MAX, Ordering::Relaxed);
    }
}

// The `%gs`-relative offsets the entry asm uses. They are `offset_of!` so the
// struct and the asm cannot drift apart (a mismatch is a boot triple-fault,
// not a wrong value); `kernel/src/tests/percpu_suite.rs` asserts the ones the
// asm assumes against the layout, and the entry probe
// (`arch::linux::probe_entry`) runs the real instructions. `UnsafeCell` and
// the atomics are `repr(transparent)`/same-layout, so the offsets are the
// plain fields' ones.
pub const OFF_SYSCALL_STACK: usize = offset_of!(Cpu, syscall) + offset_of!(SyscallEntry, stack);
pub const OFF_SAVED_USER_RSP: usize =
    offset_of!(Cpu, syscall) + offset_of!(SyscallEntry, saved_user_rsp);
pub const OFF_USER_CONTEXT: usize =
    offset_of!(Cpu, syscall) + offset_of!(SyscallEntry, user_context);
pub const OFF_WINDOW_OPEN: usize = offset_of!(Cpu, window) + offset_of!(WindowState, open);

static AREAS: [Cpu; MAX_CPUS] = [const { Cpu::ZERO }; MAX_CPUS];

/// The boot CPU's area, addressable before `%gs` points at it: `gdt::init`
/// builds the boot CPU's GDT/TSS through this, then calls [`init`].
pub fn boot_area() -> &'static Cpu {
    &AREAS[0]
}

/// Publish the boot CPU's area: write its self-pointer and point `%gs`'s
/// base at it. Called twice on the boot CPU — at `kernel_main`'s first
/// statement (everything below, including `fs::init`, reads per-CPU state)
/// and again by `gdt::init` after its selector loads reset the base. An AP
/// never calls it (it would claim area 0): the BSP runs [`prepare_ap`] and
/// the AP [`set_gs_base`]. Also installs the state sentinels into every
/// area (the static is all-zero `.bss`).
pub fn init() {
    for area in AREAS.iter() {
        area.install_sentinels();
    }
    let area = &AREAS[0];
    area.self_ptr
        .store(area as *const Cpu as u64, Ordering::Release);
    area.index.store(0, Ordering::Relaxed);
    area.online.store(true, Ordering::Release);
    super::gsgate::install(area as *const Cpu as u64);
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
    let area = &AREAS[index];
    area.self_ptr
        .store(area as *const Cpu as u64, Ordering::Release);
    area.index.store(index as u32, Ordering::Relaxed);
    Some(area as *const Cpu as u64)
}

/// Point this CPU's `IA32_GS_BASE` at `area` again — an AP after
/// `gdt::init_cpu`, whose `GS` selector load reset the hidden base to 0.
pub fn set_gs_base(area: u64) {
    super::gsgate::install(area);
}

/// The current CPU's area: `gs:[0]` holds the area's own address. A shared
/// reference: hold it as long as convenient (every member is an atomic or a
/// cell reached through an accessor), but only on the CPU that fetched it —
/// a task that can migrate (S8.3) re-fetches after a park.
#[inline]
pub fn current() -> &'static Cpu {
    // SAFETY: in ring 0 `%gs`'s base is this CPU's `Cpu` (`init`, the AP
    // bring-up, and every ring-3 entry's `swapgs`, `arch::gsgate`), set
    // before the first per-CPU read. `self_ptr` at offset 0 names the area,
    // an element of the `AREAS` static, so the reference is in bounds and
    // `'static`; `Cpu: Sync` makes sharing it sound.
    unsafe {
        let area: u64;
        asm!(
            "mov {}, gs:[0]",
            out(reg) area,
            options(nostack, preserves_flags, readonly)
        );
        &*(area as *const Cpu)
    }
}

/// The area of CPU `index`: the bring-up, the IPI senders and the tests.
pub fn area(index: usize) -> Option<&'static Cpu> {
    AREAS.get(index)
}

/// Point this CPU's `RSP0` (the ring-0 stack of the next user trap) at
/// `top` (the scheduler, at every switch).
pub fn set_rsp0(top: u64) {
    // SAFETY: the TSS of the CPU running this, written only by that CPU
    // (here) after `init_cpu`; no reference to it is live (the descriptor
    // holds its address, the hardware reads it). The scheduler, the only
    // caller outside the suite, runs with interrupts off, so no handler on
    // this CPU sees the store half done.
    unsafe {
        (*current().tss_ptr()).privilege_stack_table[0] = x86_64::VirtAddr::new(top);
    }
}

/// Snapshot of the registers at this CPU's last `syscall` entry.
pub fn user_context() -> UserContext {
    // SAFETY: only this CPU touches its own context (the entry asm, and
    // this pair of by-value accessors); no reference into it outlives a
    // call, so the copy aliases nothing.
    unsafe { *current().syscall.user_context.get() }
}

/// Replace this CPU's captured context.
pub fn set_user_context(context: UserContext) {
    // SAFETY: as in `user_context`.
    unsafe { *current().syscall.user_context.get() = context }
}
