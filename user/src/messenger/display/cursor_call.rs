//! `SetCursor` for native clients (`os.lazy.display.cursor.v1`,
//! docs/cursor-plan.md): the pointer shape over an app's own surface.

use alloc::vec::Vec;

use libmessenger::{flags, Header, Parcel, VERSION};
use messenger_generated::os_lazy_display_cursor_v1 as cursor;

use super::super::{Error, Result};
use super::client::Client;
use super::error_field;

impl Client {
    /// Show `shape` (a `CursorShape` wire value) while the pointer is over
    /// `surface`'s content. The call travels on the cursor scope's interface
    /// id, so the kernel refuses an app whose package did not ask for the
    /// scope (`EACCES`); the compositor also checks that the task owns the
    /// surface.
    pub fn set_cursor(&self, surface: u64, shape: u32) -> Result<()> {
        let body = cursor::encode_set_cursor_args(&cursor::SetCursorArgs { surface, shape })
            .map_err(Error::Parcel)?;
        let parcel = Parcel {
            header: Header {
                version: VERSION,
                flags: flags::ALLOW_NESTED,
                interface_id: cursor::INTERFACE_ID,
                method: cursor::METHOD_SETCURSOR,
                txn_id: 0,
                reply_to: 0,
                deadline_ns: 0,
            },
            body,
            handles: Vec::new(),
            buffers: Vec::new(),
        };
        let mut buf = [0u8; 128];
        let reply = self.endpoint().call_with(&parcel, &mut buf, None)?;
        match error_field(&reply) {
            Some(code) => Err(Error::Errno(-code)),
            None => Ok(()),
        }
    }
}
