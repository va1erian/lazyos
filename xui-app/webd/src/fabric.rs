//! The real Messenger binding of [`xui_webd::fabric::Fabric`]: the service's events go out on the
//! channels the clients moved in, and the clients' shared buffers are mapped, written and read
//! through the kernel's buffer calls.
//!
//! Every handle here belongs to the thread that made or received it, so [`RealFabric`] is used on
//! the service thread only (it is zero-sized and `!Send` in spirit: the `Service` that owns it
//! never leaves that thread). Requests come in through `xui_app::server::Server`; this file is only
//! what the service does *with* the objects they carry.

use libmessenger::{flags, Buffer, Header, Parcel, VERSION};
use messenger_generated::os_lazy_webd_light_v1 as wire;
use xui_app::sys::{self, errno};
use xui_webd::fabric::{Fabric, Mapping};

/// The fabric over this task's Messenger syscalls.
pub struct RealFabric;

impl Fabric for RealFabric {
    fn send(&self, channel: u64, method: u32, body: Vec<u8>) -> Result<(), i64> {
        let parcel = Parcel {
            header: Header {
                version: VERSION,
                flags: flags::ONE_WAY,
                interface_id: wire::INTERFACE_ID,
                method,
                txn_id: 0,
                reply_to: 0,
                deadline_ns: 0,
            },
            body,
            objects: Vec::new(),
        };
        // A full queue is `-EAGAIN` (a slow client) and a closed peer `-EPIPE` (a gone one): the
        // service tells the two apart by the code.
        sys::msg_send(channel, &parcel)
    }

    fn close_channel(&self, channel: u64) {
        // The handle a request carried is this task's alone; `release` closes the endpoint when
        // no other handle names it, which is how `server::Request` drops one too.
        let _ = sys::msg_release(channel);
    }

    fn close_buffer(&self, handle: u64) {
        let _ = sys::buffer_close(handle);
    }

    fn map(&self, buffer: Buffer) -> Result<Box<dyn Mapping>, i64> {
        let (base, len, size) = map_range(buffer)?;
        Ok(Box::new(SlotMapping {
            handle: buffer.handle,
            base,
            len,
            size,
        }))
    }

    fn read(&self, buffer: Buffer) -> Result<Vec<u8>, i64> {
        let mapped = map_range(buffer);
        let result = mapped.map(|(base, len, _)| {
            // SAFETY: `map_range` checked that `base..base + len` lies inside the mapping the
            // kernel reported, which stays mapped until the handle is closed below. The client
            // may write the bytes while they are read; a torn page of HTML is its own doing.
            unsafe { std::slice::from_raw_parts(base as *const u8, len) }.to_vec()
        });
        let _ = sys::buffer_close(buffer.handle);
        result
    }
}

/// Map `buffer`'s handle and check its declared range against the size the kernel reports (the
/// kernel never reads the range): the address of the range's first byte, its length and the size
/// of the whole mapping the kernel reports. The handle is closed again when the range does not fit.
fn map_range(buffer: Buffer) -> Result<(usize, usize, u64), i64> {
    let (address, size) = match sys::buffer_map(buffer.handle) {
        Ok(mapped) => mapped,
        Err(code) => {
            let _ = sys::buffer_close(buffer.handle);
            return Err(code);
        }
    };
    let end = buffer.offset.checked_add(buffer.len);
    let fits = matches!(end, Some(end) if end <= size);
    let (start, len) = (
        address.checked_add(buffer.offset),
        usize::try_from(buffer.len),
    );
    match (fits, start, len) {
        (true, Some(start), Ok(len)) => match usize::try_from(start) {
            Ok(start) => Ok((start, len, size)),
            Err(_) => close_and_refuse(buffer.handle),
        },
        _ => close_and_refuse(buffer.handle),
    }
}

fn close_and_refuse<T>(handle: u64) -> Result<T, i64> {
    let _ = sys::buffer_close(handle);
    Err(-errno::EINVAL)
}

/// A client's slot, mapped for writing. Dropping it unmaps and closes the handle.
struct SlotMapping {
    handle: u64,
    base: usize,
    len: usize,
    /// What the kernel mapped: the whole buffer, which the client's quota is charged.
    size: u64,
}

impl Mapping for SlotMapping {
    fn capacity(&self) -> usize {
        self.len
    }

    fn mapped_bytes(&self) -> u64 {
        self.size
    }

    fn write(&mut self, bytes: &[u8]) {
        let count = bytes.len().min(self.len);
        // SAFETY: `map_range` checked that `base..base + len` lies inside the mapping, which stays
        // mapped until `Drop` closes the handle; `count <= len`. The client reads the slot only
        // after the `Frame` event and writes it never, so nothing else touches these bytes while a
        // frame is copied in (and a client that does is only hurting its own picture).
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), self.base as *mut u8, count) }
    }
}

impl Drop for SlotMapping {
    fn drop(&mut self) {
        let _ = sys::buffer_close(self.handle);
    }
}
