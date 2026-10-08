//! The key-state pages sessions attach (`AttachKeyState`, I3): a shared
//! buffer per session that holds the keys down right now while that session
//! has keyboard focus, and nothing otherwise (`inputmap::keystate`).
//!
//! The buffer is the client's (created by it, charged to it); `inputd` maps
//! it and only ever writes it, through `keystate::Writer`, which keeps its own
//! seqlock counter and never reads the page back. A page of a session that
//! loses focus is cleared before its `KeyboardLeave` is sent.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use inputmap::keystate::{self, SharedKeys, Snapshot, Writer};
use inputmap::Engine;
use user::messenger::input::wire;
use user::messenger::{errno, Error, Message, Result};
use user::sys;

use super::hub::{route_error, Hub};

/// One attached page.
struct Page {
    /// The buffer handle in this task, closed when the page goes.
    handle: u64,
    /// Where it is mapped here.
    va: u64,
    writer: Writer,
}

impl Page {
    fn close(self) {
        let _ = sys::buffer_close(self.handle);
    }
}

/// The page mapped at `va`.
fn shared_at<'a>(va: u64) -> &'a SharedKeys {
    // SAFETY: `va` is this task's mapping of a live shared buffer at least
    // `keystate::SIZE` bytes long (checked on attach), 8-byte aligned
    // (checked too; mappings are page-aligned), and it stays mapped until
    // the page is closed, which only happens after its last use here. The
    // fields are atomics, so a client writing the same memory concurrently
    // races on atomics only, never undefined behaviour here.
    unsafe { &*(va as *const SharedKeys) }
}

#[derive(Default)]
pub(super) struct KeyPages {
    pages: BTreeMap<u64, Page>,
}

impl KeyPages {
    /// Drop `session`'s page, if it had one.
    pub(super) fn forget(&mut self, session: u64) {
        if let Some(page) = self.pages.remove(&session) {
            page.close();
        }
    }

    /// Bring every page up to date: the focused session's shows the keys
    /// held now, every other one is cleared. Cheap when nothing changed (the
    /// writers skip identical states).
    /// `down` is the keys the focused page may show (`held.rs` hides the
    /// ones a panel menu holds).
    pub(super) fn publish(&mut self, engine: &Engine, focused: Option<u64>, down: [u64; 4]) {
        for (session, page) in self.pages.iter_mut() {
            let state = Snapshot {
                seq: engine.last_seq(),
                focused: focused == Some(*session),
                down,
            };
            page.writer.publish(shared_at(page.va), state);
        }
    }

    /// Clear `session`'s page now (it is about to get `KeyboardLeave`).
    pub(super) fn clear(&mut self, engine: &Engine, session: u64) {
        if let Some(page) = self.pages.get_mut(&session) {
            let state = Snapshot {
                seq: engine.last_seq(),
                focused: false,
                down: [0; 4],
            };
            page.writer.publish(shared_at(page.va), state);
        }
    }
}

impl Hub {
    /// `AttachKeyState`: adopt the page the caller's own session transferred.
    pub(super) fn attach_key_state(&mut self, message: &Message) -> Result<Vec<u8>> {
        let result = self.attach_key_state_inner(message);
        if result.is_err() && message.buffers != 0 {
            let _ = sys::buffer_close(message.first_buffer);
        }
        result
    }

    fn attach_key_state_inner(&mut self, message: &Message) -> Result<Vec<u8>> {
        let args =
            wire::decode_attach_key_state_args(&message.parcel.body).map_err(Error::Parcel)?;
        // The kernel bounds the descriptor by the buffer, so its length is
        // a floor on what is mapped.
        let claimed = message.parcel.buffers.first().map_or(0, |b| b.len);
        if !message.carries(wire::ATTACH_KEY_STATE_TRANSFERS) || claimed < keystate::SIZE as u64 {
            return Err(Error::Errno(-errno::EINVAL));
        }
        self.own_session(args.session, message.sender)?;
        let va = sys::buffer_map(message.first_buffer)
            .map(|(va, _)| va)
            .map_err(Error::Errno)?;
        if va == 0 || va % 8 != 0 {
            return Err(Error::Errno(-errno::EINVAL));
        }
        self.key_pages.forget(args.session);
        self.key_pages.pages.insert(
            args.session,
            Page {
                handle: message.first_buffer,
                va,
                writer: Writer::new(),
            },
        );
        // Seed it at once, so a focused session sees its keys right away.
        let focused = self.router.focused_session();
        let keys = self.page_keys();
        self.key_pages.publish(&self.engine, focused, keys);
        Ok(Vec::new())
    }

    /// `session` exists and belongs to `sender` (else `ENOENT`, as `Close`).
    pub(super) fn own_session(&self, session: u64, sender: u64) -> Result<()> {
        match self.router.session(session) {
            Some(found) if found.owner == sender => Ok(()),
            _ => Err(route_error(inputmap::router::Error::NoSession)),
        }
    }
}
