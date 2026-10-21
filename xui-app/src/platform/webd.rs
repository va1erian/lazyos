//! `xui-webd-client`'s [`Transport`] over this task's Messenger syscalls: how an xui app shows a
//! light view of `webd` (docs/webd-plan.md, P2 slice 2).
//!
//! ```ignore
//! let mut view = LightWebView::new(WebdTransport::new(), LightConfig::default());
//! view.create(480, 320)?;            // Error::NotRunning while webd has not started
//! view.set_html(&page, "about:blank")?;
//! // on a UI timer:
//! for event in view.poll() { /* Frame, State, LinkClicked, Lost ... */ }
//! ```
//!
//! Nothing here parks the UI thread beyond a bounded call: [`WebdTransport::recv`] asks the channel
//! counters first, like [`crate::tray::TrayIcon::poll`], and a call is a `Service::call_moving_within`
//! with a tick deadline. The service is resolved without waiting (`Service::try_connect`): an app that
//! starts before `webd` has registered retries `create` on a timer.

use libmessenger::{Object, Parcel};
use messenger_generated::os_lazy_webd_light_v1 as wire;
use xui_webd_client::transport::{
    close_unqueued, RawEvent, SharedMem, Transport, EVENT_BYTES, SERVICE_NAME,
};

use crate::platform::messenger::Service;
use crate::server::ERROR_FIELD;
use crate::sys::{self, errno};

/// Ticks (100 Hz) one call to `webd` may take: it answers from its service thread, and a UI
/// thread must not wait long for it.
const CALL_TICKS: u64 = 200;
/// What `recv` hands up for a message that is not one of the interface's events (method 0 is none).
const NOT_AN_EVENT: RawEvent = (0, Vec::new());

/// Shared pages created by this task. Dropping unmaps and closes them.
pub struct Pages {
    handle: u64,
    va: u64,
    len: usize,
}

impl SharedMem for Pages {
    fn handle(&self) -> u64 {
        self.handle
    }

    fn len(&self) -> usize {
        self.len
    }

    fn write(&mut self, bytes: &[u8]) {
        let count = bytes.len().min(self.len);
        // SAFETY: `buffer_create` mapped `len` writable bytes at `va`, owned by this task until
        // `Drop` closes the buffer, and `count <= len`.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), self.va as *mut u8, count) };
    }

    fn read(&self, into: &mut [u8]) {
        let count = into.len().min(self.len);
        // SAFETY: as in `write`; the other process may be writing the pages (a frame webd has not
        // yet announced), which can tear a picture but never reads outside the mapping.
        unsafe { std::ptr::copy_nonoverlapping(self.va as *const u8, into.as_mut_ptr(), count) };
    }
}

impl Drop for Pages {
    fn drop(&mut self) {
        let _ = sys::buffer_close(self.handle);
    }
}

/// The transport.
pub struct WebdTransport {
    buf: Vec<u8>,
}

impl Default for WebdTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl WebdTransport {
    pub fn new() -> Self {
        WebdTransport {
            buf: vec![0; EVENT_BYTES],
        }
    }

    fn service() -> Option<Service> {
        Service::try_connect(SERVICE_NAME)
    }
}

/// Close the channels of a request that never left (the service is not there, or the kernel
/// refused it before queuing it): the kernel moves a request's channels only once it is queued, so
/// a handle this task still has is the client's to close, and one it has not was moved. A buffer
/// is the caller's `Pages`, which closes it when it drops.
fn close_unsent(objects: &[Object]) {
    close_unqueued(objects, |handle| sys::msg_queued(handle).is_ok(), |handle| {
        let _ = sys::msg_close(handle);
    });
}

impl Transport for WebdTransport {
    type Mem = Pages;

    fn call(&mut self, method: u32, body: Vec<u8>, objects: Vec<Object>) -> Result<Vec<u8>, i64> {
        let Some(service) = Self::service() else {
            close_unsent(&objects);
            return Err(-errno::ENOENT);
        };
        let carried = objects.clone();
        service
            .call_moving_within(
                wire::INTERFACE_ID,
                method,
                ERROR_FIELD,
                body,
                objects,
                CALL_TICKS,
            )
            .map(|reply| reply.body)
            .inspect_err(|_| close_unsent(&carried))
    }

    fn send(&mut self, method: u32, body: Vec<u8>) -> Result<(), i64> {
        let service = Self::service().ok_or(-errno::ENOENT)?;
        service.send_oneway(wire::INTERFACE_ID, method, body)
    }

    fn create_pair(&mut self) -> Result<(u64, u64), i64> {
        sys::msg_create_pair()
    }

    fn recv(&mut self, channel: u64) -> Result<Option<RawEvent>, i64> {
        match sys::msg_queued(channel) {
            Ok(0) => return Ok(None),
            Ok(_) => {}
            Err(code) => return Err(code),
        }
        let result = match sys::msg_recv(channel, &mut self.buf, sys::EXPIRED_DEADLINE) {
            Ok(result) => result,
            // A message larger than the buffer is dropped by the kernel (`EMSGSIZE`, which a receive
            // reports, where a reply that does not fit is `E2BIG`); it is not the end of the view.
            // `EVENT_BYTES` holds the largest event webd sends, so this is a service gone wrong.
            Err(code) if code == -errno::EMSGSIZE => return Ok(Some(NOT_AN_EVENT)),
            Err(code) => return Err(code),
        };
        let parcel = self
            .buf
            .get(..result.bytes as usize)
            .and_then(|bytes| Parcel::decode(bytes).ok())
            .filter(|parcel| parcel.header.interface_id == wire::INTERFACE_ID);
        // A message that does not decode, or is not this interface's, is read and dropped: the
        // client counts it as ignored and goes on to the next.
        Ok(Some(match parcel {
            Some(parcel) => (parcel.header.method, parcel.body),
            None => NOT_AN_EVENT,
        }))
    }

    fn alloc(&mut self, bytes: usize) -> Result<Pages, i64> {
        let (handle, va, size) = sys::buffer_create(bytes as u64)?;
        // `len` is what the kernel mapped: claiming more would let `write` and `read` run past it.
        match usize::try_from(size) {
            Ok(len) if len >= bytes => Ok(Pages { handle, va, len }),
            _ => {
                let _ = sys::buffer_close(handle);
                Err(-errno::ENOMEM)
            }
        }
    }

    fn close_channel(&mut self, handle: u64) {
        let _ = sys::msg_close(handle);
    }
}
