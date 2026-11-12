//! The key-state pages sessions attach (`AttachKeyState`, I3): a shared
//! buffer per session that holds the keys down right now while that session
//! has keyboard focus, and nothing otherwise (`inputmap::keystate`).
//!
//! The buffer is the client's (created by it, charged to it); `inputd` maps
//! it and only ever writes it, through `keystate::Writer`, which keeps its own
//! seqlock counter and never reads the page back. A page of a session that
//! loses focus is cleared before its `KeyboardLeave` is sent.

use alloc::collections::BTreeMap;

use inputmap::keystate::{self, SharedKeys, Snapshot, Writer};
use inputmap::Engine;
use libmessenger::client::Errno;
use libmessenger::server::Fault;
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
    /// `AttachKeyState`: adopt the page the caller's own session carried.
    pub(super) fn adopt_key_state(
        &mut self,
        sender: u64,
        session: u64,
        state: &libmessenger::Buffer,
    ) -> Result<(), Fault> {
        let va = self.map_page(sender, session, state, keystate::SIZE)?;
        self.key_pages.forget(session);
        self.key_pages.pages.insert(
            session,
            Page {
                handle: state.handle,
                va,
                writer: Writer::new(),
            },
        );
        // Seed it at once, so a focused session sees its keys right away.
        let focused = self.router.focused_session();
        let keys = self.page_keys();
        self.key_pages.publish(&self.engine, focused, keys);
        Ok(())
    }

    /// Map the page `state` carries for `session` (the caller's own), after
    /// the checks every state page shares: the page is the whole buffer from
    /// its start, at least `size` bytes, word aligned, and the mapped size is
    /// the truth about how much there is. The caller closes the buffer when
    /// this refuses.
    pub(super) fn map_page(
        &self,
        sender: u64,
        session: u64,
        state: &libmessenger::Buffer,
        size: usize,
    ) -> Result<u64, Fault> {
        if state.offset != 0 || state.len < size as u64 {
            return Err(Fault::errno(Errno::INVAL));
        }
        self.own_session(session, sender)?;
        let (va, mapped) = sys::buffer_map(state.handle).map_err(Fault::from_syscall)?;
        if va == 0 || va % 8 != 0 || !state.fits(mapped) {
            return Err(Fault::errno(Errno::INVAL));
        }
        Ok(va)
    }

    /// `session` exists and belongs to `sender` (else `ENOENT`, as `Close`).
    pub(super) fn own_session(&self, session: u64, sender: u64) -> Result<(), Fault> {
        match self.router.session(session) {
            Some(found) if found.owner == sender => Ok(()),
            _ => Err(route_error(inputmap::router::Error::NoSession)),
        }
    }
}
