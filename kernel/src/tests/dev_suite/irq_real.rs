//! End-to-end interrupt delivery from a real PCI function (issue #240).
//!
//! When the test VM has a legacy virtio-net function (the CI image does not; the
//! development runs in `docs/architecture/devices.md` add one), a userspace
//! "driver" task claims it through the syscall, arms its interrupt, kicks a TX
//! descriptor with nothing but `pio`/`cfg_write`, and the test proves the
//! interrupt travels PCI INTx -> PIC line named by the Interrupt Line register
//! -> IDT stub -> `dispatch` -> bottom half -> one-way message -> `irq_ack`.
//! With message interrupts on (issue #616) the same function takes MSI-X
//! instead: the kernel programs its table, the test points the TX queue at
//! entry 0, and the vector shows in the local APIC's request register.
//! It is also the record of what each QEMU machine type programmed: the INFO
//! lines name the function, its line and whether the PIC saw it asserted.
//! Without such a function the test only logs the routing table and passes.

use super::fixture::*;
use super::*;
use crate::dev::errno::*;
use crate::dev::syscall::*;
use crate::dev::{intx, irq};
use core::cell::UnsafeCell;
use core::sync::atomic::{fence, Ordering};

/// Legacy virtio register offsets in the I/O BAR.
const GUEST_FEATURES: u64 = 4;
const QUEUE_ADDRESS: u64 = 8;
const QUEUE_SIZE: u64 = 12;
const QUEUE_SELECT: u64 = 14;
const QUEUE_NOTIFY: u64 = 16;
const STATUS: u64 = 18;
const ISR: u64 = 19;
/// Legacy queue MSI-X vector register (present while MSI-X is enabled).
const QUEUE_VECTOR: u64 = 22;
const TX_QUEUE: u64 = 1;
/// Offset of the TX packet inside [`Region`].
const PACKET_OFFSET: usize = 12288;

/// One contiguous, page-aligned region of the kernel image: the legacy queue
/// address register needs a physical page frame number, so the descriptor
/// table, rings and packet must be physically contiguous.
#[repr(C, align(4096))]
struct Region(UnsafeCell<[u8; 16384]>);

// SAFETY: the device is quiesced (reset) before the test returns, and the
// region is only touched by the single test that owns it.
unsafe impl Sync for Region {}

static REGION: Region = Region(UnsafeCell::new([0; 16384]));

fn info(msg: &str) {
    serial_println!("TEST:dev_irq_real_device_end_to_end:INFO:{msg}");
}

fn find_nic() -> Option<DeviceInfo> {
    let table = crate::dev::table().lock();
    let found = table.iter().find(|info| {
        matches!(info.bus, BusId::Pci(_))
            && info.vendor == pci::VIRTIO_VENDOR
            && info.device == 0x1000
            && info.resources.io_bar().is_some()
            && table.owner(info.id).is_none()
    });
    found
}

/// Where the function's interrupt arrives.
#[derive(Clone, Copy)]
enum Route {
    Line(u8),
    Vector(u8),
}

impl Route {
    /// The interrupt is pending at the controller (interrupts off).
    fn requested(self) -> bool {
        match self {
            Route::Line(line) => irqchip::requested(line),
            Route::Vector(index) => crate::arch::lapic::requested(crate::dev::msi::vector(index)),
        }
    }

    fn masked(self) -> bool {
        match self {
            Route::Line(line) => masked(line),
            Route::Vector(index) => crate::dev::msi::is_masked(index),
        }
    }

    fn raised(self) -> u32 {
        match self {
            Route::Line(_) => irq::stats().raised,
            Route::Vector(_) => crate::dev::msi::stats().raised,
        }
    }
}

/// Drive the claimed function until its interrupt reaches the driver's inbox.
fn drive(
    fx: &Fixture,
    handle: u64,
    endpoint: u64,
    bar: u64,
    line: u8,
    dev: DeviceId,
) -> Result<(), String> {
    let io = |offset, width, write, value| {
        sys(OP_PIO, handle, bar, offset, pio_word(width, write, value))
    };
    expect_ok(
        sys(OP_CFG_WRITE, handle, 0x04, 2, 0x0005),
        "enable I/O and bus master",
    )?;
    let mode = expect_ok(sys(OP_IRQ_ENABLE, handle, 0, 0, 0), "irq_enable")?;
    let route = match mode {
        0 => Route::Line(line),
        2 => {
            let index = crate::dev::claims::CLAIMS
                .lock()
                .get(dev)
                .and_then(|claim| claim.msi)
                .ok_or("MSI-X without a vector")?;
            Route::Vector(index)
        }
        other => return Err(format!("virtio-net has no MSI, yet mode {other}")),
    };
    info(&format!(
        "irq_enable mode {mode} ({})",
        if mode == 0 { "INTx" } else { "MSI-X" }
    ));

    expect_ok(io(STATUS, 1, true, 0), "reset")?;
    expect_ok(io(STATUS, 1, true, 3), "ack+driver")?;
    expect_ok(io(GUEST_FEATURES, 4, true, 0), "features")?;
    expect_ok(io(QUEUE_SELECT, 2, true, TX_QUEUE as u32), "select tx")?;
    if let Route::Vector(_) = route {
        // The reset dropped every vector: point the TX queue at entry 0.
        expect_ok(io(QUEUE_VECTOR, 2, true, 0), "queue vector")?;
        let vector = expect_ok(io(QUEUE_VECTOR, 2, false, 0), "read queue vector")?;
        check!(
            vector == 0,
            "the device refused MSI-X entry 0 ({vector:#x})"
        );
    }
    let size = expect_ok(io(QUEUE_SIZE, 2, false, 0), "queue size")? as usize;
    check!((1..=256).contains(&size), "unexpected queue size {size}");

    let base = REGION.0.get() as *mut u8;
    // SAFETY: the region is 16 KiB of our own static memory, no device is
    // using it yet (it was just reset), and every offset below is inside it.
    unsafe { core::ptr::write_bytes(base, 0, 16384) };
    let phys = crate::block::virt_to_phys(x86_64::VirtAddr::from_ptr(base as *const u8))
        .ok_or("no physical address for the queue")?
        .as_u64();
    check!(
        phys < 1 << 32,
        "queue at {phys:#x} is above the legacy 32-bit PFN limit"
    );
    expect_ok(
        io(QUEUE_ADDRESS, 4, true, (phys >> 12) as u32),
        "queue address",
    )?;
    expect_ok(io(STATUS, 1, true, 7), "driver ok")?;

    let avail = size * 16;
    // SAFETY: as above; descriptor 0 points at the packet inside the region.
    unsafe {
        let desc = base as *mut u64;
        desc.write_volatile(phys + PACKET_OFFSET as u64);
        (base.add(8) as *mut u32).write_volatile(70);
        (base.add(12) as *mut u16).write_volatile(0); // flags: device reads
                                                      // A broadcast frame after a zeroed 10-byte virtio-net header.
        core::ptr::write_bytes(base.add(PACKET_OFFSET + 10), 0xFF, 6);
        (base.add(avail + 2) as *mut u16).write_volatile(1); // avail.idx
    }
    fence(Ordering::Release);
    expect_ok(io(QUEUE_NOTIFY, 2, true, TX_QUEUE as u32), "kick")?;

    // Interrupts are off, but the controller latches the request: the route
    // is right if and only if it lights up.
    let mut seen = false;
    for _ in 0..400_000 {
        if route.requested() {
            seen = true;
            break;
        }
    }
    info(&format!(
        "line {line}: the controller {} the function's interrupt",
        if seen { "SAW" } else { "did NOT see" }
    ));
    check!(
        seen,
        "the device kicked but its interrupt was never requested"
    );

    // Let the real interrupt through, with everything else masked.
    let quiet = [0u8, 1, 12];
    let saved = quiet.map(irqchip::is_masked);
    for line in quiet {
        irqchip::set_masked(line, true);
    }
    let before = route.raised();
    x86_64::instructions::interrupts::enable();
    for _ in 0..1_000_000 {
        if route.raised() != before {
            break;
        }
        core::hint::spin_loop();
    }
    x86_64::instructions::interrupts::disable();
    for (line, was) in quiet.into_iter().zip(saved) {
        irqchip::set_masked(line, was);
    }
    check!(route.raised() == before + 1, "the ISR never ran");
    check!(route.masked(), "the ISR did not mask the source");

    intx::service_at(50_000);
    check!(
        queued(endpoint)? == 1,
        "the bottom half posted {} messages",
        queued(endpoint)?
    );
    let (sender, device, ..) = take_irq(endpoint)?;
    check!(
        sender == task::KERNEL_TASK && device == u32::from(dev.0),
        "message from {sender} for device {device}"
    );
    let status = expect_ok(io(ISR, 1, false, 0), "read the virtio ISR")?;
    check!(
        status & 1 != 0,
        "the device's queue interrupt bit is clear ({status:#x})"
    );
    if let Route::Line(line) = route {
        check!(
            !irqchip::requested(line),
            "reading the ISR did not deassert the line"
        );
    }
    expect_ok(sys(OP_IRQ_ACK, handle, 0, 0, 0), "irq_ack")?;
    check!(!route.masked(), "the ack did not unmask the source");
    let _ = fx;
    Ok(())
}

pub fn irq_real_device_end_to_end() -> Result<(), String> {
    let fx = Fixture::new()?;
    let Some(nic) = find_nic() else {
        info("no unclaimed legacy virtio-net function in this VM; routing log only");
        return Ok(());
    };
    let line = nic.resources.irq().map_or(0xFF, |irq| irq.line);
    let BusId::Pci(address) = nic.bus else {
        return Ok(());
    };
    let bar = nic.resources.io_bar().ok_or("no I/O BAR")?.index;
    info(&format!(
        "virtio-net {:02x}:{:02x}.{} interrupt line {line} ({})",
        address.bus,
        address.device,
        address.function,
        if irq::routable(line) {
            "routable"
        } else {
            "polling fallback"
        }
    ));

    let saved_command = pci::command(address);
    let slot = spawn_driver(driver_cred())?;
    enter(slot)?;
    let mut endpoint = 0u64;
    let handle = expect_ok(
        claim_irq(nic.id, &mut endpoint, false),
        "claim the real function",
    )?;
    let outcome = if irq::routable(line) || crate::dev::msi::enabled() {
        drive(&fx, handle, endpoint, u64::from(bar), line, nic.id)
    } else {
        expect_errno(
            sys(OP_IRQ_ENABLE, handle, 0, 0, 0),
            ENOSYS,
            "irq_enable on an unroutable line",
        )
    };
    // Whatever happened, silence the function before its memory goes away.
    let _ = sys(OP_PIO, handle, u64::from(bar), STATUS, pio_word(1, true, 0));
    let _ = sys(OP_RELEASE, handle, 0, 0, 0);
    leave(&fx);
    pci::write_command(address, saved_command);
    outcome
}

pub(super) const CASES: &[(&str, Test)] =
    &[("dev_irq_real_device_end_to_end", irq_real_device_end_to_end)];
