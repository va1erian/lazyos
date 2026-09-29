//! Raw x86 port I/O, compartmentalized to one place.
//!
//! Before this module, `block::ata`, `block::virtio`, and `block::pci` each
//! instantiated their own `x86_64::instructions::port::Port` and wrapped it
//! in a one-off `unsafe { }` block with its own safety comment, repeating the
//! same reasoning (and the same risk of getting it subtly wrong) at every
//! call site. Port I/O carries no Rust-level aliasing or lifetime hazard —
//! the CPU's `in`/`out` instructions can't produce a dangling reference or a
//! data race the borrow checker would have caught — so the only thing to get
//! right is *which port, with which width, at which point in a driver's
//! protocol*. That's a per-driver protocol contract, not a per-call-site
//! memory-safety proof, so it belongs in the driver's own `# Safety` comment
//! at the call site, while the mechanical "issue the instruction" part is
//! centralized here, verified once.
//!
//! Every driver that used to hand-roll `Port::<T>::new(port).read()/write()`
//! should call [`inb`]/[`outb`] (and the 16/32-bit variants) instead.

use x86_64::instructions::port::Port;

/// Read one byte from `port`.
///
/// # Safety
/// The caller must know that reading `port` at this point in its device's
/// protocol has no side effect the caller isn't prepared for (some device
/// registers are read-to-clear, or advance internal device state).
#[inline]
pub unsafe fn inb(port: u16) -> u8 {
    Port::new(port).read()
}

/// Write one byte to `port`.
///
/// # Safety
/// The caller must know that `port` is the intended device register and
/// that writing `value` to it now is valid in the device's protocol
/// (uninitialized hardware, or a write at the wrong protocol step, can wedge
/// or misprogram the device).
#[inline]
pub unsafe fn outb(port: u16, value: u8) {
    Port::new(port).write(value)
}

/// Read one 16-bit word from `port`. See [`inb`] for the safety contract.
///
/// # Safety
/// Same contract as [`inb`], at 16-bit width.
#[inline]
pub unsafe fn inw(port: u16) -> u16 {
    Port::new(port).read()
}

/// Write one 16-bit word to `port`. See [`outb`] for the safety contract.
///
/// # Safety
/// Same contract as [`outb`], at 16-bit width.
#[inline]
pub unsafe fn outw(port: u16, value: u16) {
    Port::new(port).write(value)
}

/// Read one 32-bit dword from `port`. See [`inb`] for the safety contract.
///
/// # Safety
/// Same contract as [`inb`], at 32-bit width.
#[inline]
pub unsafe fn inl(port: u16) -> u32 {
    Port::new(port).read()
}

/// Write one 32-bit dword to `port`. See [`outb`] for the safety contract.
///
/// # Safety
/// Same contract as [`outb`], at 32-bit width.
#[inline]
pub unsafe fn outl(port: u16, value: u32) {
    Port::new(port).write(value)
}

/// Read `buf.len() / 2` 16-bit words from `port` into `buf` with a single
/// `rep insw`. Under a hypervisor every port access is a VM exit; a string
/// instruction lets the emulator service the whole run per exit instead of
/// one exit per word (a per-word `in` loop costs 256 exits per ATA sector).
///
/// # Safety
/// Same contract as [`inb`], for every word read. `buf.len()` must be even.
#[inline]
pub unsafe fn insw_bytes(port: u16, buf: &mut [u8]) {
    debug_assert!(buf.len().is_multiple_of(2));
    // SAFETY: `rdi`/`rcx` describe exactly the caller's exclusive slice, and
    // `rep insw` writes `rcx` words there and nothing else. The direction flag
    // is clear on entry (the SysV ABI guarantees it), so `rdi` counts upward;
    // the instruction touches no flags the compiler relies on.
    core::arch::asm!(
        "rep insw",
        in("dx") port,
        inout("rdi") buf.as_mut_ptr() => _,
        inout("rcx") buf.len() / 2 => _,
        options(nostack, preserves_flags),
    );
}
