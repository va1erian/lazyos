//! Boot-time evidence for the interrupt and syscall surfaces (issue #240): the
//! `DEV:IRQ` and `DEV:SYSCALL` lines, and the per-device interrupt routing log
//! that documents what the platform actually programmed.

use x86_64::instructions::tables::sidt;

use super::errno::{EBADF, EINVAL, EPERM};
use super::irq::{self, LINES};
use super::syscall::{self, NO_ENDPOINT};
use super::{pci, table, BusId};

/// First interrupt vector of the remapped PIC.
const PIC_VECTOR_BASE: u64 = 32;

/// Log every PCI function's Interrupt Line and whether the PIC can deliver it.
/// The firmware programs this register; the log is how we learn what a given
/// machine type (i440fx, q35) really did.
pub(super) fn log_irq_routes() {
    let devices = table().lock();
    for info in devices.iter() {
        let BusId::Pci(address) = info.bus else {
            continue;
        };
        let (line, verdict) = match info.resources.irq() {
            None => (pci::interrupt_line(address), "no INTx pin"),
            Some(irq) if irq::routable(irq.line) => (irq.line, "routable"),
            Some(irq) => (irq.line, "polling"),
        };
        serial_println!(
            "dev: irq route {:02x}:{:02x}.{} {:04x}:{:04x} class {:02x}{:02x} pin {} line {} ({})",
            address.bus,
            address.device,
            address.function,
            info.vendor,
            info.device,
            info.class,
            info.subclass,
            pci::interrupt_pin(address),
            line,
            verdict
        );
    }
}

/// Whether the IDT entry for `vector` is present.
fn vector_present(vector: u64) -> bool {
    let idtr = sidt();
    if vector * 16 + 15 > u64::from(idtr.limit) {
        return false;
    }
    let entry = idtr.base.as_u64() + vector * 16;
    // SAFETY: `entry` lies inside the loaded IDT (limit checked above), which is
    // leaked and never freed; an entry is two naturally aligned u64 words.
    let word = unsafe { core::ptr::read_volatile(entry as *const u64) };
    // Bit 47 of the low word is the gate's present bit.
    word & (1 << 47) != 0
}

/// Print `DEV:IRQ`, `DEV:SYSCALL` and `DEV:DMA`. Needs the IDT loaded, so it
/// runs after `arch::init`.
pub fn selfcheck() {
    check_irq();
    check_syscall();
    check_dma();
}

/// `DEV:DMA`: the boot-time DMA pool reservation (issue #241).
fn check_dma() {
    let stats = crate::mem::dma_stats();
    if stats.total_pages == 0 {
        serial_println!("DEV:DMA:INFO:no DMA pool reserved");
    } else {
        serial_println!(
            "DEV:DMA:PASS:{} pages, largest run {}",
            stats.total_pages,
            stats.largest_run
        );
    }
}

fn check_irq() {
    let missing = (0..u64::from(LINES))
        .filter(|line| !vector_present(PIC_VECTOR_BASE + line))
        .count();
    let (mut wired, mut routed) = (0usize, 0usize);
    for info in table().lock().iter() {
        // Only functions that wire an INTx pin carry an `Irq` resource.
        if let (BusId::Pci(_), Some(line)) = (info.bus, info.resources.irq()) {
            wired += 1;
            routed += usize::from(irq::routable(line.line));
        }
    }
    if missing == 0 {
        serial_println!(
            "DEV:IRQ:PASS:{LINES} vectors installed, {routed}/{wired} INTx-wired PCI functions on routable lines"
        );
    } else {
        serial_println!("DEV:IRQ:FAIL:{missing} PIC vectors have no handler");
    }
}

/// Exercise the syscall's argument gates from the kernel task, which must be
/// refused everywhere and touch no device.
fn check_syscall() {
    let neg = |errno: i64| (-errno) as u64;
    let no_device = 0xFFFF;
    let checks = [
        (
            "unknown op",
            syscall::dispatch(0xFF, 0, 0, 0, 0),
            neg(EINVAL),
        ),
        (
            "kernel task claim",
            syscall::dispatch(syscall::OP_CLAIM, no_device, NO_ENDPOINT, 0, 0),
            neg(EPERM),
        ),
        (
            "bad handle",
            syscall::dispatch(syscall::OP_RELEASE, 0xDEAD, 0, 0, 0),
            neg(EBADF),
        ),
        (
            "dma_alloc bad handle",
            syscall::dispatch(syscall::OP_DMA_ALLOC, 0, 0, 0, 0),
            neg(EBADF),
        ),
    ];
    let failed = checks.iter().find(|(_, got, want)| got != want);
    match failed {
        None if syscall::claim_count() == 0 => {
            serial_println!("DEV:SYSCALL:PASS:syscall 23 gates refuse, 0 claims");
        }
        None => serial_println!("DEV:SYSCALL:FAIL:a claim exists at boot"),
        Some((name, got, want)) => {
            serial_println!("DEV:SYSCALL:FAIL:{name}: got {got:#x}, want {want:#x}");
        }
    }
}
