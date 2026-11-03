//! What the host asks its I/O shell to do. The shell (`user/src/bin/btd/`)
//! owns every syscall; the host only decides.

use alloc::string::String;
use alloc::vec::Vec;

use bthid::decode::Event;

/// The input source class a device registers (`lazyos_sys::source_class`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceKind {
    Keyboard,
    Pointer,
    /// A pointer that reports positions.
    Tablet,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Register an input source for the link (one per half of a device).
    OpenSource { handle: u16, kind: SourceKind },
    /// Publish a decoded event on the link's source of the event's class.
    Input { handle: u16, event: Event },
    /// Close every source of the link. The kernel releases what is still
    /// held; the host has already emitted the releases as `Input`.
    CloseSources { handle: u16 },
    /// Write the bond store (the encoded file) atomically.
    SaveBonds(Vec<u8>),
    /// One evidence line, without the `BT:` prefix.
    Trace(String),
    /// The controller is unusable (bring-up failed, or it stopped answering).
    Failed(String),
}
