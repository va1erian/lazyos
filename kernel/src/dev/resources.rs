//! Typed device resources: BAR windows and interrupt lines (issue #239).
//!
//! A discovered function reports its resources as a fixed set of [`Bar`]
//! windows plus an optional [`Irq`] line. Nothing here is heap-allocated:
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

/// One typed resource. `Msi` joins this enum when MSI lands (D2).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Resource {
    Bar(Bar),
    Irq(Irq),
}

/// The typed resource set of one device, keyed by BAR index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Resources {
    bars: [Option<Bar>; MAX_BARS],
    irq: Option<Irq>,
}

impl Resources {
    /// No resources (a plain bridge or an unenumerated function).
    pub const fn empty() -> Resources {
        Resources {
            bars: [None; MAX_BARS],
            irq: None,
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

    /// The first memory BAR, if any.
    pub fn mem_bar(&self) -> Option<Bar> {
        self.bars().find(|bar| bar.kind == BarKind::Mem)
    }

    /// The first I/O BAR, if any.
    pub fn io_bar(&self) -> Option<Bar> {
        self.bars().find(|bar| bar.kind == BarKind::Io)
    }
}
