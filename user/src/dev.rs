//! Userspace side of the device syscall, number 23 (issue #240).
//!
//! A userspace driver (unprivileged uid holding `CAP_DEV_CLAIM`) finds its
//! device with [`list`], takes it with [`claim`], and from then on holds only a
//! `Device` handle: [`map_bar`], [`pio_read`]/[`pio_write`], [`cfg_read`]/
//! [`cfg_write`], [`irq_enable`]/[`irq_ack`] and [`release`] take that handle,
//! never an address. The kernel bounds-checks every call against the device's
//! own resources and returns `-errno` (surfaced here as `Err(errno)`).
//!
//! Interrupts arrive as one-way Messenger messages from the kernel on the
//! channel endpoint named at [`claim`]; [`parse_irq`] decodes one. After
//! servicing the device, call [`irq_ack`] exactly once per message: until then
//! the kernel sends no further message for this claim and keeps the line masked.
//!
//! The layout mirrors `kernel/src/dev/syscall.rs`; keep them in lockstep.

use libmessenger::{Decoder, Parcel};

pub mod inspect;

/// Capability required to list or claim devices at all.
pub const CAP_DEV_CLAIM: u32 = 1 << 8;

// The syscall's numbers (op codes, `claim`'s endpoint arguments and flag, the
// row width) are defined once in `lazyos-sys`, checked against the kernel.
pub use lazyos_sys::dev::{op, FLAG_SHARED_IRQ, KERNEL_CHANNEL, NO_ENDPOINT, ROW_WORDS};

/// Rows a driver should offer [`list`]: the kernel device table's capacity
/// (`dev::table::MAX_DEVICES`), so a function late in a large PC's bus order
/// (an xHCI controller after 40 chipset functions) is never cut off.
pub const MAX_ROWS: usize = 128;

/// Bits in [`Row::flags`].
pub mod row_flag {
    /// The device has an owner.
    pub const OWNED: u64 = 1 << 0;
    /// A PCI function (config space is available).
    pub const PCI: u64 = 1 << 1;
    /// Its interrupt line can be delivered; otherwise poll.
    pub const IRQ_ROUTABLE: u64 = 1 << 2;
    /// The function has an MSI capability (the kernel programs it).
    pub const MSI: u64 = 1 << 3;
    /// The function has a usable MSI-X capability.
    pub const MSIX: u64 = 1 << 4;
}

/// How a claim's interrupts arrive, as [`irq_enable`] reports it. The
/// messages and [`irq_ack`] are the same for all three; what differs is the
/// device side: an INTx driver must deassert its line before the ack (the
/// kernel unmasks it), and a virtio driver on MSI-X must point its queues at
/// table entry 0 (`virtio::Transport::use_msix`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IrqMode {
    Intx,
    Msi,
    MsiX,
}

/// Errno values the device syscall returns.
pub mod errno {
    pub const EPERM: i64 = 1;
    pub const ENOENT: i64 = 2;
    pub const EBADF: i64 = 9;
    pub const ENOMEM: i64 = 12;
    pub const EACCES: i64 = 13;
    pub const EFAULT: i64 = 14;
    pub const EBUSY: i64 = 16;
    pub const ENODEV: i64 = 19;
    pub const EINVAL: i64 = 22;
    pub const EMFILE: i64 = 24;
    pub const ENOSYS: i64 = 38;
    pub const EDQUOT: i64 = 122;
}

/// `dma_alloc` flag bits (issue #241).
pub mod dma_flag {
    /// Map the buffer only into the creator, never a client.
    pub const SHARE_ONLY: u64 = 1 << 0;
    /// The caller can address the whole 64-bit bus.
    pub const ADDR64: u64 = 1 << 1;
}

/// One decoded `list` row. Physical BAR addresses are never reported: a driver
/// learns sizes and kinds and maps a BAR by index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Row {
    pub id: u64,
    pub vendor: u16,
    pub device: u16,
    pub subsystem_vendor: u16,
    pub subsystem_device: u16,
    pub class: u8,
    pub subclass: u8,
    pub prog_if: u8,
    pub revision: u8,
    /// Hash of the class interface (`os.kernel.dev.<class>`).
    pub class_interface_id: u64,
    /// [`row_flag`] bits.
    pub flags: u64,
    /// The PCI Interrupt Line register, `0xFF` if none.
    pub irq_line: u8,
    /// Bumps every time the device's claim ends.
    pub generation: u32,
    /// BAR `i` length in bytes, 0 if absent.
    pub bar_len: [u64; 6],
    /// BAR `i` metadata nibble: bit 0 present, 1 I/O, 2 64-bit, 3 prefetchable.
    pub bar_meta: [u8; 6],
}

impl Row {
    /// Decode one row of [`ROW_WORDS`] words.
    pub fn from_words(words: &[u64; ROW_WORDS]) -> Row {
        let mut bar_len = [0u64; 6];
        let mut bar_meta = [0u8; 6];
        for index in 0..6 {
            bar_len[index] = words[7 + index];
            bar_meta[index] = ((words[6] >> (index * 4)) & 0xF) as u8;
        }
        Row {
            id: words[0],
            vendor: words[1] as u16,
            device: (words[1] >> 16) as u16,
            subsystem_vendor: (words[1] >> 32) as u16,
            subsystem_device: (words[1] >> 48) as u16,
            class: words[2] as u8,
            subclass: (words[2] >> 8) as u8,
            prog_if: (words[2] >> 16) as u8,
            revision: (words[2] >> 24) as u8,
            class_interface_id: words[3],
            flags: words[4] & 0xFF,
            irq_line: (words[4] >> 8) as u8,
            generation: words[5] as u32,
            bar_len,
            bar_meta,
        }
    }
}

fn dev_syscall(op: u64, a1: u64, a2: u64, a3: u64, a4: u64) -> i64 {
    // SAFETY: every caller in this module passes, for its `op`, pointers to
    // buffers it owns for the call (rows, BAR records, IRQ blocks) with their
    // real lengths; the kernel validates them against this task again.
    unsafe { lazyos_sys::dev::dev_syscall(op, a1, a2, a3, a4) }
}

fn value(code: i64) -> Result<u64, i64> {
    if code < 0 {
        Err(-code)
    } else {
        Ok(code as u64)
    }
}

/// Fill `rows` with the devices the caller may see; returns the total device
/// count (retry with a larger buffer if it exceeds `rows.len()`).
pub fn list(rows: &mut [[u64; ROW_WORDS]]) -> Result<usize, i64> {
    value(dev_syscall(
        op::LIST,
        rows.as_mut_ptr() as u64,
        rows.len() as u64,
        0,
        0,
    ))
    .map(|total| total as usize)
}

/// Claim device `id` without interrupts (the driver polls). Returns the
/// `Device` handle.
pub fn claim(id: u64) -> Result<u64, i64> {
    value(dev_syscall(op::CLAIM, id, NO_ENDPOINT, 0, 0))
}

/// Claim device `id` with interrupts: the kernel makes the channel its
/// `os.kernel.dev` `irq` messages arrive on and hands back its receive
/// handle, which can be received and waited on but never duplicated,
/// transferred or published. `shared` opts in to sharing an interrupt line.
/// Returns `(Device handle, interrupt channel handle)`.
pub fn claim_with_irq(id: u64, shared: bool) -> Result<(u64, u64), i64> {
    let flags = if shared { FLAG_SHARED_IRQ } else { 0 };
    let mut channel = 0u64;
    let handle = value(dev_syscall(
        op::CLAIM,
        id,
        KERNEL_CHANNEL,
        flags,
        &mut channel as *mut u64 as u64,
    ))?;
    Ok((handle, channel))
}

/// Map memory BAR `bar` uncached into this address space; returns its address.
pub fn map_bar(handle: u64, bar: u64) -> Result<*mut u8, i64> {
    value(dev_syscall(op::MAP_BAR, handle, bar, 0, 0)).map(|va| va as *mut u8)
}

fn pio(handle: u64, bar: u64, offset: u64, width: u64, write: Option<u32>) -> Result<u32, i64> {
    let request = width | u64::from(write.is_some()) << 8 | u64::from(write.unwrap_or(0)) << 32;
    value(dev_syscall(op::PIO, handle, bar, offset, request)).map(|read| read as u32)
}

/// Read `width` (1, 2 or 4) bytes at `offset` in I/O BAR `bar`.
pub fn pio_read(handle: u64, bar: u64, offset: u64, width: u64) -> Result<u32, i64> {
    pio(handle, bar, offset, width, None)
}

/// Write `width` (1, 2 or 4) bytes at `offset` in I/O BAR `bar`.
pub fn pio_write(handle: u64, bar: u64, offset: u64, width: u64, data: u32) -> Result<(), i64> {
    pio(handle, bar, offset, width, Some(data)).map(|_| ())
}

/// Read the device's PCI configuration space.
pub fn cfg_read(handle: u64, offset: u64, width: u64) -> Result<u32, i64> {
    value(dev_syscall(op::CFG_READ, handle, offset, width, 0)).map(|read| read as u32)
}

/// Write the 16-bit command register at offset 4 (decode, bus-master, INTx).
/// Nothing else in config space is writable.
pub fn cfg_write(handle: u64, offset: u64, width: u64, data: u64) -> Result<(), i64> {
    value(dev_syscall(op::CFG_WRITE, handle, offset, width, data)).map(|_| ())
}

/// Start receiving interrupt messages; returns how they arrive. `Err(ENOSYS)`
/// means the function has neither a deliverable line nor MSI: poll instead.
pub fn irq_enable(handle: u64) -> Result<IrqMode, i64> {
    value(dev_syscall(op::IRQ_ENABLE, handle, 0, 0, 0)).map(|mode| match mode {
        1 => IrqMode::Msi,
        2 => IrqMode::MsiX,
        _ => IrqMode::Intx,
    })
}

/// Acknowledge the interrupt message just serviced.
pub fn irq_ack(handle: u64) -> Result<(), i64> {
    value(dev_syscall(op::IRQ_ACK, handle, 0, 0, 0)).map(|_| ())
}

/// Quiesce the device, unmap its BARs and end the claim.
pub fn release(handle: u64) -> Result<(), i64> {
    value(dev_syscall(op::RELEASE, handle, 0, 0, 0)).map(|_| ())
}

/// Allocate `len` bytes of physically contiguous DMA memory (issue #241).
///
/// Returns the shared-buffer handle and the bus address to program into the
/// device. The buffer can be transferred to a client zero-copy; `flags` is a
/// bitwise OR of [`dma_flag`].
pub fn dma_alloc(handle: u64, len: u64, flags: u64) -> Result<(u64, u64), i64> {
    let mut bus = 0u64;
    let buffer = value(dev_syscall(
        op::DMA_ALLOC,
        handle,
        len,
        flags,
        &mut bus as *mut u64 as u64,
    ))?;
    Ok((buffer, bus))
}

/// The fields of an interrupt notification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IrqMessage {
    pub device: u32,
    pub index: u32,
    pub generation: u32,
}

/// Decode the parcel bytes of a message received on the claim's endpoint. Only
/// trust it if the message's kernel-stamped `sender` is slot 0 (the kernel).
pub fn parse_irq(parcel_bytes: &[u8]) -> Option<IrqMessage> {
    let parcel = Parcel::decode(parcel_bytes).ok()?;
    parse_irq_body(&parcel.body)
}

/// Decode the TLV body of an interrupt message that a Messenger receive already
/// unwrapped into a parcel (`Message::parcel.body`). The same trust rule as
/// [`parse_irq`] applies: check the sender is slot 0 first.
pub fn parse_irq_body(body: &[u8]) -> Option<IrqMessage> {
    let mut fields = [None::<u32>; 3];
    let mut decoder = Decoder::new(body);
    while let Ok(Some(field)) = decoder.next() {
        let slot = usize::from(field.id).checked_sub(1)?;
        if let Some(entry) = fields.get_mut(slot) {
            *entry = field.as_u32().ok();
        }
    }
    Some(IrqMessage {
        device: fields[0]?,
        index: fields[1]?,
        generation: fields[2]?,
    })
}
