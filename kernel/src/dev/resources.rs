//! Typed device resources: BAR windows, interrupt lines and message-signalled
//! interrupts (issues #239, #616).
//!
//! A discovered function reports its resources as a fixed set of [`Bar`]
//! windows plus an optional [`Irq`] line and the [`Msi`] and [`MsiX`]
//! capabilities it offers. Nothing here is heap-allocated:
//! enumeration runs once at boot and the device table is read on the hot path.

/// Maximum number of BARs a type-0 PCI function can expose.
pub const MAX_BARS: usize = 6;

/// A BAR decodes either a memory or an I/O window.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BarKind {
    Mem,
    Io,
}

/// A base-address register window.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Bar {
    /// Which of the function's BAR registers this is (0..6).
    pub index: u8,
    pub kind: BarKind,
    /// Window base, already merged from both halves for a 64-bit memory BAR.
    pub base: u64,
    /// Window length from the write-ones decode probe; 0 when it could not be
    /// sized (for example a BAR hidden behind a disabled bridge).
    pub len: u64,
    /// Whether the BAR consumes the following register as its high half.
    pub is_64: bool,
    /// Memory BARs only: the device may prefetch this window.
    pub prefetchable: bool,
}

/// A legacy interrupt line the function is wired to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Irq {
    pub line: u8,
}

/// The function's MSI capability (PCI 3.0, 6.8.1): the kernel programs it,
/// never the driver.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Msi {
    /// Config-space offset of the capability header.
    pub cap: u8,
    /// The message address has a high dword (and the data moves to +0xC).
    pub is_64: bool,
    /// The capability has a per-vector mask register.
    pub maskable: bool,
}

/// The function's MSI-X capability (PCI 3.0, 6.8.2). The vector table lives
/// in one of the function's memory BARs; `map_bar` never maps its pages to
/// the driver (the kernel writes it through its own mapping).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MsiX {
    pub cap: u8,
    /// Entries in the vector table (1..=2048).
    pub table_size: u16,
    /// BAR index and byte offset of the vector table.
    pub table_bar: u8,
    pub table_offset: u32,
}

impl MsiX {
    /// Bytes the vector table spans (16 per entry).
    pub fn table_len(&self) -> u64 {
        u64::from(self.table_size) * 16
    }
}

/// One typed resource.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Resource {
    Bar(Bar),
    Irq(Irq),
    Msi(Msi),
    MsiX(MsiX),
}

/// The typed resource set of one device, keyed by BAR index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Resources {
    bars: [Option<Bar>; MAX_BARS],
    irq: Option<Irq>,
    msi: Option<Msi>,
    msix: Option<MsiX>,
}

impl Resources {
    /// No resources (a plain bridge or an unenumerated function).
    pub const fn empty() -> Resources {
        Resources {
            bars: [None; MAX_BARS],
            irq: None,
            msi: None,
            msix: None,
        }
    }

    /// Record `bar` in its own slot, ignoring an out-of-range index.
    pub fn set_bar(&mut self, bar: Bar) {
        if let Some(slot) = self.bars.get_mut(bar.index as usize) {
            *slot = Some(bar);
        }
    }

    /// Record the interrupt line.
    pub fn set_irq(&mut self, irq: Irq) {
        self.irq = Some(irq);
    }

    /// The BAR at `index`, if present.
    pub fn bar(&self, index: u8) -> Option<Bar> {
        self.bars.get(index as usize).copied().flatten()
    }

    /// Every present BAR in index order.
    pub fn bars(&self) -> impl Iterator<Item = Bar> + '_ {
        self.bars.iter().flatten().copied()
    }

    /// The interrupt line, if the function has one.
    pub fn irq(&self) -> Option<Irq> {
        self.irq
    }

    /// Record the MSI capability.
    pub fn set_msi(&mut self, msi: Msi) {
        self.msi = Some(msi);
    }

    /// Record the MSI-X capability.
    pub fn set_msix(&mut self, msix: MsiX) {
        self.msix = Some(msix);
    }

    /// The MSI capability, if the function has one.
    pub fn msi(&self) -> Option<Msi> {
        self.msi
    }

    /// The MSI-X capability, if the function has a usable one.
    pub fn msix(&self) -> Option<MsiX> {
        self.msix
    }

    /// Whether the function can signal interrupts with messages.
    pub fn message_capable(&self) -> bool {
        self.msi.is_some() || self.msix.is_some()
    }

    /// The first memory BAR, if any.
    pub fn mem_bar(&self) -> Option<Bar> {
        self.bars().find(|bar| bar.kind == BarKind::Mem)
    }

    /// The first I/O BAR, if any.
    pub fn io_bar(&self) -> Option<Bar> {
        self.bars().find(|bar| bar.kind == BarKind::Io)
    }
}
