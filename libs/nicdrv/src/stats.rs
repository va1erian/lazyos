//! Driver counters: the numbers `Stats` reports and the soak checks for leaks.

/// Counters since the driver started. The fields mirror
/// `os.lazy.net.nic.v1`'s `NicStats`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub rx_frames: u64,
    pub tx_frames: u64,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    /// Frames dropped on receive for any reason.
    pub rx_dropped: u64,
    /// Frames dropped on transmit for any reason.
    pub tx_dropped: u64,
    /// Frames shorter than an Ethernet header, either direction.
    pub runts: u64,
    /// Frames longer than `max_frame`, either direction.
    pub oversize: u64,
    /// Rings poisoned by a peer, plus device completions the driver rejected.
    pub ring_errors: u64,
    /// Interrupt messages handled.
    pub interrupts: u64,
    pub link_changes: u32,
}
