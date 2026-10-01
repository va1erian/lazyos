//! Delivery on the way out of a native (`int 0x80`) syscall.
//!
//! Native programs install no handlers, so the only signals that can act on
//! one are the default actions, and before this boundary existed only the
//! timer sweep applied them -- and only to a task it caught *in user mode*. A
//! native service parked in a blocking syscall (a Messenger `recv`, `wait`) is
//! woken by a `SIGTERM`, returns to user mode for a few instructions and
//! re-enters the kernel, so the tick almost never found it there and the
//! signal stayed pending forever: `init`'s orderly shutdown (docs/shutdown.md)
//! had to `SIGKILL` every service. Checking at the syscall return, as the
//! Linux path does, ends it there.

use super::*;

/// The default-fatal signal (term or core) pending and unblocked for native
/// task `slot`: the one its return to user mode must end it with. Pending
/// signals whose action is to do nothing (ignored, or a default ignore or
/// continue) are consumed on the way; a stop default is left for the
/// scheduler. `None` for a Linux task, whose syscall return delivers through
/// [`deliver_linux`].
pub fn native_fatal_pending(slot: usize) -> Option<u8> {
    let (pml4, kind) = slot_info(slot)?;
    if kind == Kind::Linux {
        return None;
    }
    while let Some((sig, disposition)) = next_deliverable(pml4) {
        match (disposition, default_action(sig)) {
            (Disposition::Default, DefaultAction::Term | DefaultAction::Core) => {
                return Some(sig);
            }
            (Disposition::Ignore, _)
            | (Disposition::Default, DefaultAction::Ignore | DefaultAction::Cont) => {
                clear_pending(pml4, sig);
            }
            // A stop, or a handler a native task cannot have installed: the
            // scheduler's sweep owns those.
            _ => return None,
        }
    }
    None
}

/// End the current native task if a default-fatal signal is pending, like
/// the timer sweep would have; returns otherwise. Called by the native
/// syscall gate after every call.
pub fn deliver_native() {
    let slot = current();
    if slot == KERNEL_TASK {
        return;
    }
    let Some(sig) = native_fatal_pending(slot) else {
        return;
    };
    let Some((pml4, _)) = slot_info(slot) else {
        return;
    };
    terminate_process(pml4, 128 + sig as u64);
    halt_forever();
}
