//! Global descriptor table and task state segment.
//!
//! We install our own GDT with ring-3 code/data selectors and a TSS, so the CPU
//! can switch stacks when a user program traps into the kernel (syscalls and
//! interrupts).
//!
//! The GDT and the TSS are **per CPU** (`arch::percpu`): `RSP0` is rewritten
//! at every task switch, so two CPUs cannot share one TSS. Every CPU's GDT is
//! built identically (the TSS descriptor pointing into its own area), so the
//! `sysret` selector arithmetic is a machine-wide invariant and the user
//! selectors (`selectors()`) are one value.

use x86_64::instructions::segmentation::{Segment, CS, DS, ES, FS, GS, SS};
use x86_64::instructions::tables::load_tss;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable};
use x86_64::structures::tss::TaskStateSegment;
use x86_64::VirtAddr;

use super::percpu;

/// IST index used by the double-fault handler.
pub const DOUBLE_FAULT_IST: u16 = 0;
/// IST slot of the NMI's stack (`percpu::Cpu::nmi_stack`).
pub const NMI_IST: u16 = 1;

static mut USER_CODE: u16 = 0;
static mut USER_DATA: u16 = 0;

/// Install the boot CPU's GDT and TSS (its per-CPU area's), then point `%gs`
/// at that area. The `%gs` base must be written after the selector loads:
/// `GS::set_reg` reloads the segment's hidden base from the GDT (0), and the
/// MSR is what gives memory operands the area's address again.
pub fn init() {
    // SAFETY: runs once at boot, single-threaded, before interrupts are
    // enabled and before any other code touches the area; every raw pointer
    // below stays within its own static.
    unsafe {
        let selectors = init_cpu(percpu::boot_area());
        percpu::init();
        USER_CODE = selectors.user_code;
        USER_DATA = selectors.user_data;
    }
}

/// Build CPU `area`'s GDT/TSS/stacks and load them. The BSP runs this once at
/// boot; an AP runs it from its own bring-up (S8.2) against its own area.
///
/// SAFETY: the caller guarantees nothing else touches `area`'s TSS and GDT
/// (its own CPU, before it publishes `online`; or the boot CPU at boot), and
/// that this may load a GDT/TSS (boot, or an AP that has left firmware code
/// behind).
pub(crate) unsafe fn init_cpu(area: &'static percpu::Cpu) -> Selectors {
    // `TaskStateSegment::new()` points `iomap_base` past the TSS's own
    // bytes, so the segment limit denies every port bit to CPL > IOPL —
    // this CPU runs no I/O bitmap.
    // SAFETY: the caller's exclusivity contract makes this the only
    // reference to the area's TSS cell; it ends with this function (the
    // hardware keeps only the address).
    let tss = unsafe { &mut *area.tss_ptr() };
    *tss = TaskStateSegment::new();
    // Kernel stack used by the CPU on interrupts/syscalls from ring 3.
    tss.privilege_stack_table[0] = VirtAddr::new(area.kernel_stack_top());
    tss.interrupt_stack_table[DOUBLE_FAULT_IST as usize] =
        VirtAddr::new(area.double_fault_stack_top());
    tss.interrupt_stack_table[NMI_IST as usize] = VirtAddr::new(area.nmi_stack_top());

    // A fresh table every time: an area can be set up again (an AP that
    // timed out and was retried), and appending to the old one overflows.
    // SAFETY: as for the TSS: the only reference to the GDT cell.
    let gdt = unsafe { (*area.gdt_ptr()).write(GlobalDescriptorTable::new()) };
    let kcode = gdt.append(Descriptor::kernel_code_segment());
    let kdata = gdt.append(Descriptor::kernel_data_segment());
    // Order matters for `sysret`: it loads CS = base+16 and SS = base+8, so
    // user data (0x18) must precede user code (0x20) with base 0x10.
    let udata = gdt.append(Descriptor::user_data_segment());
    let ucode = gdt.append(Descriptor::user_code_segment());
    // SAFETY: the TSS was written above and lives in the static per-CPU
    // area for good; the descriptor embeds its address and size.
    let tss_selector = gdt.append(unsafe { Descriptor::tss_segment_unchecked(area.tss_ptr()) });

    // SAFETY: the GDT lives in the static area until a later `init_cpu` of
    // the same CPU rewrites it (and reloads it before using it).
    unsafe { gdt.load_unsafe() };

    CS::set_reg(kcode);
    DS::set_reg(kdata);
    ES::set_reg(kdata);
    SS::set_reg(kdata);
    FS::set_reg(kdata);
    GS::set_reg(kdata);
    load_tss(tss_selector);

    Selectors {
        user_code: ucode.0,
        user_data: udata.0,
    }
}

/// User-mode `CS`/`SS` selectors (with RPL 3).
pub fn selectors() -> Selectors {
    // Safety: `USER_CODE`/`USER_DATA` are written once by `init` before any
    // task runs and never written again; this is a read-only access after that.
    unsafe {
        Selectors {
            user_code: USER_CODE,
            user_data: USER_DATA,
        }
    }
}

/// The selectors needed when building an `iretq` frame.
#[derive(Clone, Copy)]
pub struct Selectors {
    pub user_code: u16,
    pub user_data: u16,
}

/// Point the ring0 stack used for the next user->kernel trap at `top` — this
/// CPU's TSS (`arch::percpu`; the scheduler calls it at every switch).
pub fn set_kernel_stack(top: u64) {
    percpu::set_rsp0(top);
}
