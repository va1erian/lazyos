//! Global descriptor table and task state segment.
//!
//! We install our own GDT with ring-3 code/data selectors and a TSS, so the CPU
//! can switch stacks when a user program traps into the kernel (syscalls and
//! interrupts).

use core::ptr::addr_of;
use x86_64::instructions::segmentation::{Segment, CS, DS, ES, FS, GS, SS};
use x86_64::instructions::tables::load_tss;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable};
use x86_64::structures::tss::TaskStateSegment;
use x86_64::VirtAddr;

/// IST index used by the double-fault handler.
pub const DOUBLE_FAULT_IST: u16 = 0;

const KERNEL_STACK_SIZE: usize = 32 * 1024;
const DOUBLE_FAULT_STACK_SIZE: usize = 4 * 1024 * 5;

static mut GDT: GlobalDescriptorTable = GlobalDescriptorTable::new();
static mut TSS: TaskStateSegment = TaskStateSegment::new();
static mut KERNEL_STACK: [u8; KERNEL_STACK_SIZE] = [0; KERNEL_STACK_SIZE];
static mut DOUBLE_FAULT_STACK: [u8; DOUBLE_FAULT_STACK_SIZE] = [0; DOUBLE_FAULT_STACK_SIZE];

static mut USER_CODE: u16 = 0;
static mut USER_DATA: u16 = 0;
static mut KERNEL_CODE: u16 = 0;
static mut KERNEL_DATA: u16 = 0;

/// Install the GDT and TSS.
pub fn init() {
    unsafe {
        // Kernel stack used by the CPU on interrupts/syscalls from ring 3.
        let stack = addr_of!(KERNEL_STACK) as u64;
        let tss = &mut *addr_of!(TSS).cast_mut();
        tss.privilege_stack_table[0] = VirtAddr::new(stack + KERNEL_STACK_SIZE as u64);
        let df = addr_of!(DOUBLE_FAULT_STACK) as u64;
        tss.interrupt_stack_table[DOUBLE_FAULT_IST as usize] =
            VirtAddr::new(df + DOUBLE_FAULT_STACK_SIZE as u64);

        let gdt = &mut *addr_of!(GDT).cast_mut();
        let kcode = gdt.append(Descriptor::kernel_code_segment());
        let kdata = gdt.append(Descriptor::kernel_data_segment());
        let ucode = gdt.append(Descriptor::user_code_segment());
        let udata = gdt.append(Descriptor::user_data_segment());
        let tss_selector = gdt.append(Descriptor::tss_segment(&*addr_of!(TSS)));

        let gdt: &'static GlobalDescriptorTable = &*addr_of!(GDT);
        gdt.load();

        CS::set_reg(kcode);
        DS::set_reg(kdata);
        ES::set_reg(kdata);
        SS::set_reg(kdata);
        FS::set_reg(kdata);
        GS::set_reg(kdata);
        load_tss(tss_selector);

        KERNEL_CODE = kcode.0;
        KERNEL_DATA = kdata.0;
        USER_CODE = ucode.0;
        USER_DATA = udata.0;
    }
}

/// User-mode `CS`/`SS` selectors (with RPL 3), plus kernel ones for `iretq`.
pub fn selectors() -> Selectors {
    unsafe {
        Selectors {
            user_code: USER_CODE,
            user_data: USER_DATA,
            kernel_code: KERNEL_CODE,
            kernel_data: KERNEL_DATA,
        }
    }
}

/// The selectors we need when building an `iretq` frame.
#[derive(Clone, Copy)]
pub struct Selectors {
    pub user_code: u16,
    pub user_data: u16,
    pub kernel_code: u16,
    pub kernel_data: u16,
}

/// Top of the kernel stack used for user->kernel transitions.
pub fn kernel_stack_top() -> u64 {
    addr_of!(KERNEL_STACK) as u64 + KERNEL_STACK_SIZE as u64
}

/// Top of the double-fault IST stack.
pub fn double_fault_stack_top() -> u64 {
    addr_of!(DOUBLE_FAULT_STACK) as u64 + DOUBLE_FAULT_STACK_SIZE as u64
}
