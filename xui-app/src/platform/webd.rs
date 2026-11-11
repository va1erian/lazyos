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
//! counters first, like [`crate::tray::TrayIcon::poll`], and a call is a [`Conn::call`] with a tick
//! deadline. The service is resolved without waiting (`Conn::try_connect`): an app that starts before
//! `webd` has registered retries `create` on a timer. The calls stay raw (method id, body, objects)
//! because `xui_webd_client::transport::Transport` is one seam over the light and the full interface,
//! which a generated client per interface cannot be.
//!
//! A full view (`FullWebView`, `os.lazy.webd.view.v1`) takes [`WebdTransport::full`]: it talks to
//! `app.os.lazy.webd.view`, sends every call as `os.lazy.webd.view.v1` except the `CreateView` of a
//! scope (`ScopedTransport::call_through`), and reads events of that interface.

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

use libmessenger::client::{Connection, Deadline};
use libmessenger::{Object, Parcel};
use messenger_generated::os_lazy_webd_light_v1 as wire;
use messenger_generated::os_lazy_webd_view_v1 as view_wire;
use xui_webd_client::transport::{
    close_unqueued, RawEvent, ScopedTransport, SharedMem, Transport, EVENT_BYTES,
    FULL_SERVICE_NAME, SERVICE_NAME,
};

use crate::platform::messenger::{Conn, Observed};
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

    fn read_at(&self, offset: usize, into: &mut [u8]) {
        let count = into.len().min(self.len.saturating_sub(offset));
        // SAFETY: as in `read`: `offset + count <= len`, so the copy stays inside the mapping.
        unsafe {
            std::ptr::copy_nonoverlapping(
                (self.va as *const u8).add(offset.min(self.len)),
                into.as_mut_ptr(),
                count,
            )
        };
    }
}

impl Drop for Pages {
    fn drop(&mut self) {
        let _ = sys::buffer_close(self.handle);
    }
}

/// The descriptor watching a channel's peer; only a LazyOS (unix) build has descriptors, so a host
/// build of this crate (Windows tooling) never watches and treats an empty queue as nothing yet.
#[cfg(unix)]
type HangupFd = OwnedFd;
#[cfg(not(unix))]
type HangupFd = ();

/// The transport: the registered name it calls and the interface of its calls and events.
pub struct WebdTransport {
    buf: Vec<u8>,
    conn: Conn,
    interface: u64,
    /// The event channel and a descriptor watching it, made at the first empty poll
    /// ([`WebdTransport::peer_gone`]); dropped when the channel is closed.
    hangup: Option<(u64, HangupFd)>,
}

impl Default for WebdTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl WebdTransport {
    /// For a light view: `app.os.lazy.webd.light`, `os.lazy.webd.light.v1`.
    pub fn new() -> Self {
        WebdTransport {
            buf: vec![0; EVENT_BYTES],
            conn: Connection::named(Observed::default(), SERVICE_NAME.to_owned()),
            interface: wire::INTERFACE_ID,
            hangup: None,
        }
    }

    /// For a full view: `app.os.lazy.webd.view`, `os.lazy.webd.view.v1` (and a scope's
    /// `CreateView` through [`ScopedTransport::call_through`]).
    pub fn full() -> Self {
        WebdTransport {
            buf: vec![0; EVENT_BYTES],
            conn: Connection::named(Observed::default(), FULL_SERVICE_NAME.to_owned()),
            interface: view_wire::INTERFACE_ID,
            hangup: None,
        }
    }

    /// An empty queue is not yet "nothing to read": the kernel counts no queued message for a
    /// channel whose peer closed, and only readiness says so (`POLLIN | POLLHUP` on the descriptor
    /// the window parks on). Taking that for "nothing yet" left a view of a webd that died waiting
    /// for events for good: the descriptor stayed readable, the window's loop spun on it, and the
    /// view was never made again. Asked with a zero-timeout `poll` on a descriptor of our own (a
    /// blocking `msg_wait_any` would park the UI thread until the next tick on every empty poll).
    #[cfg(not(unix))]
    fn peer_gone(&mut self, _channel: u64) -> Result<Option<RawEvent>, i64> {
        Ok(None)
    }

    #[cfg(unix)]
    fn peer_gone(&mut self, channel: u64) -> Result<Option<RawEvent>, i64> {
        let fd = match &self.hangup {
            Some((handle, fd)) if *handle == channel => fd.as_raw_fd(),
            _ => {
                let Ok(raw) = lazyos_sys::msg::endpoint_fd(channel, true) else {
                    return Ok(None);
                };
                // SAFETY: `endpoint_fd` just made this descriptor and nothing else owns it.
                let owned = unsafe { OwnedFd::from_raw_fd(raw) };
                self.hangup = Some((channel, owned));
                raw
            }
        };
        let mut watch = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: `watch` is one valid `pollfd` and the count says so; a zero timeout never parks.
        let ready = unsafe { libc::poll(&mut watch, 1, 0) };
        let closed = ready > 0 && watch.revents & (libc::POLLHUP | libc::POLLERR) != 0;
        // A message that arrived since the count is read by the next poll.
        if closed && matches!(sys::msg_queued(channel), Ok(0)) {
            return Err(-errno::EPIPE);
        }
        Ok(None)
    }

    /// Whether `webd` is registered now (one attempt, never a wait).
    fn connected(&self) -> bool {
        self.conn.try_connect().is_ok()
    }

    fn call_as(
        &mut self,
        interface: u64,
        method: u32,
        body: Vec<u8>,
        objects: Vec<Object>,
    ) -> Result<Vec<u8>, i64> {
        if !self.connected() {
            close_unsent(&objects);
            return Err(-errno::ENOENT);
        }
        let carried = objects.clone();
        // webd's inbox is one endpoint every client shares (64 messages deep), and oneway input is not
        // counted per sender, so another client's flood can fill it: the kernel then refuses the call
        // with `EAGAIN` before webd sees it (found by `tools/webd/attack_run.py`). The service itself
        // never answers `EAGAIN`, so this always means "not queued, try again": the objects are still
        // this task's. Sleeping lets webd run and drain (one CPU), and the retries are bounded so a UI
        // thread waits at most `QUEUE_FULL_RETRIES` x `QUEUE_FULL_PAUSE`.
        let mut tries = 0;
        let result = loop {
            let attempt = self.conn.call(
                interface,
                method,
                body.clone(),
                objects.clone(),
                Deadline::ticks(CALL_TICKS),
            );
            match attempt {
                Err(error) if error.code() == -errno::EAGAIN && tries < QUEUE_FULL_RETRIES => {
                    tries += 1;
                    std::thread::sleep(QUEUE_FULL_PAUSE);
                }
                other => break other,
            }
        };
        result
            .map(|reply| reply.body)
            .map_err(i64::from)
            .inspect_err(|_| close_unsent(&carried))
    }
}

/// Tries of a call webd's full inbox refused (`EAGAIN`) before the caller is told, and the pause
/// between them: 50 x 4 ms is at most 0.2 s on a UI thread, enough for webd to drain 64 messages.
const QUEUE_FULL_RETRIES: u32 = 50;
const QUEUE_FULL_PAUSE: std::time::Duration = std::time::Duration::from_millis(4);

/// Close the channels of a request that never left (the service is not there, or the kernel
/// refused it before queuing it): the kernel moves a request's channels only once it is queued, so
/// a handle this task still has is the client's to close, and one it has not was moved. A buffer
/// is the caller's `Pages`, which closes it when it drops.
fn close_unsent(objects: &[Object]) {
    close_unqueued(
        objects,
        |handle| sys::msg_queued(handle).is_ok(),
        |handle| {
            let _ = sys::msg_close(handle);
        },
    );
}

impl Transport for WebdTransport {
    type Mem = Pages;

    fn call(&mut self, method: u32, body: Vec<u8>, objects: Vec<Object>) -> Result<Vec<u8>, i64> {
        self.call_as(self.interface, method, body, objects)
    }

    fn send(&mut self, method: u32, body: Vec<u8>) -> Result<(), i64> {
        if !self.connected() {
            return Err(-errno::ENOENT);
        }
        self.conn
            .send(self.interface, method, body, Vec::new())
            .map_err(i64::from)
    }

    fn create_pair(&mut self) -> Result<(u64, u64), i64> {
        sys::msg_create_pair()
    }

    fn recv(&mut self, channel: u64) -> Result<Option<RawEvent>, i64> {
        match sys::msg_queued(channel) {
            Ok(0) => return self.peer_gone(channel),
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
            .filter(|parcel| parcel.header.interface_id == self.interface);
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
        if self
            .hangup
            .as_ref()
            .is_some_and(|(held, _)| *held == handle)
        {
            self.hangup = None;
        }
        let _ = sys::msg_close(handle);
    }
}

impl ScopedTransport for WebdTransport {
    fn call_through(
        &mut self,
        interface: u64,
        method: u32,
        body: Vec<u8>,
        objects: Vec<Object>,
    ) -> Result<Vec<u8>, i64> {
        self.call_as(interface, method, body, objects)
    }
}
