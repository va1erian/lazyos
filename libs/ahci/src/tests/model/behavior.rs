//! How the model HBA misbehaves.

#[derive(Clone, Copy, Default)]
pub struct Behavior {
    /// `CR` never clears after `ST` is cleared.
    pub never_stop: bool,
    /// Commands never complete.
    pub hang: bool,
    /// A read or write touching this LBA fails (task file error).
    pub fail_lba: Option<u64>,
    /// After an error the device stays `BSY` until COMRESET.
    pub busy_after_error: bool,
    /// `PRDBC` is two bytes short.
    pub short_prdbc: bool,
    /// `CI` reads between two completions (0: one per read).
    pub latency: u32,
    /// Reads of `PxTFD` that show `BSY` after the link comes up.
    pub busy_polls: u32,
    /// Reads of `PxSSTS` that show `DET = 1` before the link is up.
    pub settle_polls: u32,
    /// `BOHC.BOS` stays set for this many reads after `OOS` is written.
    pub bios_polls: u32,
    /// `BOHC.BB` also shows for this many reads once `BOS` cleared.
    pub bios_busy_polls: u32,
    /// The HBA implements `CAP2.BOH`.
    pub handoff: bool,
    /// The HBA can address 64-bit memory.
    pub s64a: bool,
    /// COMRESET never brings the link back.
    pub dead_link: bool,
}
