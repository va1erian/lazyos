//! Small public types of [`crate::Host`].

/// Counters for what the host refused or dropped, for tests and `dbgd`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Packets that did not parse.
    pub malformed: u32,
    /// Connections and channels refused by policy.
    pub refused: u32,
    /// Reports dropped because the link was not a registered device.
    pub dropped_reports: u32,
    /// Commands or ACL fragments dropped because a queue was full.
    pub overflow: u32,
}

/// What kind of failure ended a host; the shell's restart policy follows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailKind {
    /// Bring-up or firmware did not finish (including the re-enumeration
    /// cap). A new host would fail the same way: the shell leaves the
    /// controller alone until it is unplugged.
    BringUp,
    /// A working controller stopped answering or overflowed its queue: a
    /// bounded start-over may recover it.
    Runtime,
}
