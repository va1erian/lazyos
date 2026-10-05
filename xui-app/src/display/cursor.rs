//! `SetCursor` (`os.lazy.display.cursor.v1`, docs/cursor-plan.md): the pointer
//! shape over an app's own surface. The call travels on the cursor scope's
//! interface id so the kernel's label rules can refuse an app whose package
//! did not ask for the scope.

use libmessenger::{flags, Header, Parcel, VERSION};
use messenger_generated::os_lazy_display_cursor_v1 as wire;

use super::Client;
use crate::sys::errno;

/// What a host test sees of the calls a detached client makes.
#[cfg(test)]
pub(crate) mod probe {
    use std::cell::{Cell, RefCell};

    thread_local! {
        /// `(surface, shape)` of every call, in order.
        pub static SENT: RefCell<Vec<(u64, u32)>> = const { RefCell::new(Vec::new()) };
        /// The error the next calls fail with, if any.
        pub static FAIL: Cell<Option<i64>> = const { Cell::new(None) };
    }

    pub fn sent() -> Vec<(u64, u32)> {
        SENT.with(|sent| sent.borrow().clone())
    }

    pub fn reset() {
        SENT.with(|sent| sent.borrow_mut().clear());
        FAIL.with(|fail| fail.set(None));
    }
}

impl Client {
    /// Show `shape` (a `CursorShape` wire value) while the pointer is over
    /// `surface`'s content. Refused with `-EACCES` when the app's package did
    /// not ask for the scope, `-ENOENT` for a surface the compositor does not
    /// know, `-EINVAL` for a shape it cannot draw.
    pub fn set_cursor(&self, surface: u64, shape: u32) -> Result<(), i64> {
        #[cfg(test)]
        if self.endpoint == 0 {
            probe::SENT.with(|sent| sent.borrow_mut().push((surface, shape)));
            return probe::FAIL.with(|fail| fail.get()).map_or(Ok(()), Err);
        }
        let body = wire::encode_set_cursor_args(&wire::SetCursorArgs { surface, shape })
            .map_err(|_| -errno::EINVAL)?;
        let parcel = Parcel {
            header: Header {
                version: VERSION,
                flags: flags::ALLOW_NESTED,
                interface_id: wire::INTERFACE_ID,
                method: wire::METHOD_SETCURSOR,
                txn_id: 0,
                reply_to: 0,
                deadline_ns: 0,
            },
            body,
            handles: Vec::new(),
            buffers: Vec::new(),
        };
        self.call(&parcel).map(|_| ())
    }
}
