//! The I3 half of an input session (`docs/input-plan.md`): the key-state page
//! a game polls ("is W held?" with one memory read) and keyboard grabs.
//!
//! [`Session::attach_key_state`] creates a one-page shared buffer and hands
//! it to `inputd`, which writes the keys held right now into it while the
//! window has keyboard focus and clears it when focus leaves
//! (`inputmap::keystate`). [`Session::request_grant`] asks for a keyboard
//! grab: the compositor answers later with an [`super::Event::Grant`]; the
//! user can always end it with Ctrl+Alt+Esc.

use libmessenger::BufferDesc;
use messenger_generated::os_lazy_input_v1 as wire;

pub use inputmap::keystate::Snapshot;
use inputmap::keystate::{SharedKeys, SIZE};

use super::{call, Session, NAME};
use crate::sys::{self, errno, msg_resolve};

/// One page: plenty for the 56-byte layout.
const PAGE_BYTES: u64 = 4096;

/// The key-state page this task created and `inputd` writes.
pub struct KeyStatePage {
    handle: u64,
    va: u64,
}

impl KeyStatePage {
    /// A consistent copy of the page, or `None` when every read raced a
    /// write (try again next frame).
    pub fn snapshot(&self) -> Option<Snapshot> {
        // SAFETY: `va` is this task's mapping of the `PAGE_BYTES`-byte shared
        // buffer created in `attach_key_state` (page-aligned, so aligned for
        // `SharedKeys`, which needs `SIZE <= PAGE_BYTES` bytes), mapped until
        // `drop` closes it. Its fields are atomics, so `inputd` writing them
        // concurrently is an atomic race, never undefined behaviour.
        let page = unsafe { &*(self.va as *const SharedKeys) };
        page.snapshot()
    }
}

impl Drop for KeyStatePage {
    fn drop(&mut self) {
        let _ = sys::display_close_buffer(self.handle);
    }
}

impl Session {
    /// Create a key-state page and attach it to this session.
    pub fn attach_key_state(&self) -> Result<KeyStatePage, i64> {
        const _: () = assert!(SIZE as u64 <= PAGE_BYTES);
        let (handle, va, _) = sys::display_create_buffer(PAGE_BYTES)?;
        // Dropped on any failure below, which closes the buffer.
        let page = KeyStatePage { handle, va };
        let body = wire::encode_attach_key_state_args(&wire::AttachKeyStateArgs {
            session: self.session,
        })
        .map_err(|_| -errno::EINVAL)?;
        let (handles, buffers) =
            wire::encode_attach_key_state_transfers(&wire::AttachKeyStateTransfers {
                state: BufferDesc {
                    handle,
                    offset: 0,
                    len: PAGE_BYTES,
                    flags: 0,
                },
            });
        on_service(|service| call(service, wire::METHOD_ATTACHKEYSTATE, body, handles, buffers))?;
        Ok(page)
    }

    /// Ask for a keyboard grab (the window must have focus, else `-EACCES`).
    /// The answer arrives as [`super::Event::Grant`].
    pub fn request_grant(&self) -> Result<(), i64> {
        let body = wire::encode_request_grant_args(&wire::RequestGrantArgs {
            session: self.session,
            kind: wire::GRANT_KIND_KEYBOARD,
        })
        .map_err(|_| -errno::EINVAL)?;
        on_service(|service| {
            call(
                service,
                wire::METHOD_REQUESTGRANT,
                body,
                Vec::new(),
                Vec::new(),
            )
        })
        .map(|_| ())
    }

    /// Give the grab (or a pending request) back.
    pub fn release_grant(&self) -> Result<(), i64> {
        let body = wire::encode_release_grant_args(&wire::ReleaseGrantArgs {
            session: self.session,
        })
        .map_err(|_| -errno::EINVAL)?;
        on_service(|service| {
            call(
                service,
                wire::METHOD_RELEASEGRANT,
                body,
                Vec::new(),
                Vec::new(),
            )
        })
        .map(|_| ())
    }

    /// `Ping`: `inputd`'s newest processed raw sequence number while this
    /// session has focus, else 0 (a page whose `seq` is at least this is
    /// current).
    pub fn ping(&self, token: u64) -> Result<u64, i64> {
        let body = wire::encode_ping_args(&wire::PingArgs {
            session: self.session,
            token,
        })
        .map_err(|_| -errno::EINVAL)?;
        let reply =
            on_service(|service| call(service, wire::METHOD_PING, body, Vec::new(), Vec::new()))?;
        let reply = wire::decode_ping_reply(&reply.body).map_err(|_| -errno::EINVAL)?;
        if reply.token != token {
            return Err(-errno::EINVAL);
        }
        Ok(reply.seq)
    }
}

/// Run `f` against a resolved `inputd` handle, releasing it afterwards.
fn on_service<T>(f: impl FnOnce(u64) -> Result<T, i64>) -> Result<T, i64> {
    let service = msg_resolve(NAME)?;
    let result = f(service);
    // A resolved handle: release it (closing would end inputd's side).
    let _ = crate::display::release(service);
    result
}
