//! A received [`Message`], and the caller credentials the kernel stamps on it.
//!
//! Every message carries a snapshot of its sender's credentials (`uid`,
//! `gid`, capability bits, label and session) taken by the kernel when the
//! message was *queued* (issue #446). [`Message::caller`] is how a service
//! authorizes a request: it needs no `CAP_SETUID` (which reading another
//! task's credentials through `cred_get` does) and cannot be fooled by a
//! sender that exited and whose task slot was reused before the message was
//! read. Never authorize from [`Message::sender`]: it is a slot, not an
//! identity.
//!
//! A message also owns the objects the kernel installed for it (a channel
//! end moved to this task, a buffer shared with it; `docs/messenger-core-plan.md`
//! 3.3) until a generated decoder claims them through [`Message::decode`].
//! Whatever is still unclaimed when the message drops is closed, so a server
//! that ignores a request, or refuses it, cannot leak what it carried.

use alloc::vec::Vec;
use core::cell::RefCell;

use libmessenger::{Object, Parcel};

use lazyos_sys::msg::SenderId;

use super::types::{Error, Result};
use crate::sys::Cred;

/// `recv` flag: also write the sender's stamped credentials to `parcel_ptr`.
pub(super) use lazyos_sys::msg::op::RECV_SENDER_ID;

/// Bytes of the stamped block (the kernel's `channels::SenderId::SIZE`).
pub(super) const CALLER_BLOCK: usize = SenderId::SIZE;

/// Decode the kernel's stamped block. `None` if a word is out of range for
/// its field, which the kernel never writes: the caller refuses the message
/// rather than guess an identity.
pub(super) fn decode_caller(block: &[u8; CALLER_BLOCK]) -> Option<Cred> {
    SenderId::from_bytes(block).map(SenderId::cred)
}

/// A received message: the decoded parcel plus the kernel-stamped metadata
/// userspace cannot otherwise see, and the objects it carried.
#[derive(PartialEq, Eq, Debug)]
pub struct Message {
    /// Task slot that sent the message. Only for addressing (e.g. a reply
    /// path or a per-client table); authorize with [`Message::caller`].
    pub sender: u64,
    /// The sender's credentials when the message was queued.
    pub caller: Cred,
    /// Kernel transaction id for a call; `None` for one-way messages.
    pub txn: Option<u64>,
    /// The decoded parcel. Its own object list still holds the sender's
    /// numbers; [`Message::objects`] holds this task's.
    pub parcel: Parcel,
    /// The message's objects as the kernel installed them in this task's
    /// table, in object-list order, each with its kind. Owned by the message
    /// until a decoder claims them ([`Message::decode`]); closed on drop. A
    /// cell, so a server's `&Message` handler can claim them.
    objects: RefCell<Vec<Object>>,
}

impl Message {
    /// A message from `recv`: the parcel and the installed object numbers,
    /// which must be one per entry of the parcel's object list.
    pub(super) fn new(
        sender: u64,
        caller: Cred,
        txn: Option<u64>,
        parcel: Parcel,
        installed: &[u64],
    ) -> Result<Message> {
        if installed.len() != parcel.objects.len() {
            // The kernel reports exactly the list it validated; anything else
            // is a bug, and the handles must not be adopted blindly.
            for &handle in installed {
                let _ = lazyos_sys::msg::release(handle);
            }
            return Err(Error::Errno(-super::errno::EINVAL));
        }
        let objects = parcel
            .objects
            .iter()
            .zip(installed)
            .map(|(object, &handle)| object.with_handle(handle))
            .collect();
        Ok(Message {
            sender,
            caller,
            txn,
            parcel,
            objects: RefCell::new(objects),
        })
    }

    /// The sender's credentials, as the kernel stamped them at queue time.
    pub fn caller(&self) -> Cred {
        self.caller
    }

    /// Method id from the parcel header.
    pub fn method(&self) -> u32 {
        self.parcel.header.method
    }

    /// Interface id from the parcel header.
    pub fn interface_id(&self) -> u64 {
        self.parcel.header.interface_id
    }

    /// The installed objects, in object-list order, while the message still
    /// owns them (empty once claimed).
    pub fn objects(&self) -> Vec<Object> {
        self.objects.borrow().clone()
    }

    /// Decode the request with a generated `decode_<method>_args` that takes
    /// the installed objects (`wire::decode_attach_buffer_args`). On success
    /// the decoded value owns every object (the decoder claimed each one by
    /// its declared slot, so none is left over); on failure the message keeps
    /// them and closes them when it drops.
    pub fn decode<T>(
        &self,
        decoder: impl FnOnce(&[u8], &[Object]) -> core::result::Result<T, libmessenger::Error>,
    ) -> Result<T> {
        let mut objects = self.objects.borrow_mut();
        let value = decoder(&self.parcel.body, &objects).map_err(Error::Parcel)?;
        objects.clear();
        Ok(value)
    }

    /// Take the installed objects out of the message, to hand them on by
    /// hand; the caller owns them from here.
    pub fn take_objects(&self) -> Vec<Object> {
        core::mem::take(&mut self.objects.borrow_mut())
    }
}

impl Drop for Message {
    fn drop(&mut self) {
        for object in self.objects.get_mut().drain(..) {
            let _ = match object {
                // A received end is released, not closed: other holders of
                // the side (a service end every client resolved) keep it.
                Object::Channel(handle) => lazyos_sys::msg::release(handle),
                Object::Buffer(handle) => crate::sys::buffer_close(handle),
            };
        }
    }
}
