//! An in-memory Messenger fabric for the host tests: the service's [`Fabric`] over plain vectors,
//! with the client's side as methods. A "channel" is one number that stands for both ends (the
//! handle installed in the service's table and the client's endpoint); a "buffer" is shared bytes
//! the test keeps a second reference to, so it can read what the service painted.
//!
//! It counts what a leak would leave behind: every handle the service was given must be closed
//! exactly once ([`MemFabric::open_handles`], [`MemFabric::double_closes`]).

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;

use crate::fabric::{errno, Fabric, Mapping};

/// Bytes shared between the test (the client) and the service.
pub type Shared = Rc<RefCell<Vec<u8>>>;

struct Chan {
    open: bool,
    /// The client is gone: sends fail with `-EPIPE`.
    dead: bool,
    /// The client's queue is full: sends fail with `-EAGAIN`.
    full: bool,
    events: VecDeque<(u32, Vec<u8>)>,
}

struct Buf {
    open: bool,
    data: Shared,
}

#[derive(Default)]
struct Inner {
    next: u64,
    channels: BTreeMap<u64, Chan>,
    buffers: BTreeMap<u64, Buf>,
    double_closes: usize,
}

/// The fabric. Clones share one state.
#[derive(Clone, Default)]
pub struct MemFabric {
    inner: Rc<RefCell<Inner>>,
}

impl MemFabric {
    pub fn new() -> Self {
        Self::default()
    }

    fn fresh(inner: &mut Inner) -> u64 {
        inner.next += 1;
        inner.next
    }

    /// A new event endpoint a client could move in `CreateView`.
    pub fn new_channel(&self) -> u64 {
        let mut inner = self.inner.borrow_mut();
        let handle = Self::fresh(&mut inner);
        inner.channels.insert(
            handle,
            Chan {
                open: true,
                dead: false,
                full: false,
                events: VecDeque::new(),
            },
        );
        handle
    }

    /// A new buffer of `bytes` zeroed bytes a client could share, and the client's view of it.
    pub fn new_buffer(&self, bytes: usize) -> (u64, Shared) {
        self.new_buffer_with(vec![0; bytes])
    }

    /// A new buffer holding `data`.
    pub fn new_buffer_with(&self, data: Vec<u8>) -> (u64, Shared) {
        let shared: Shared = Rc::new(RefCell::new(data));
        (self.new_buffer_shared(shared.clone()), shared)
    }

    /// A new handle on bytes that already back another buffer (many handles, one region: the
    /// quota test attaches 512 MiB of slots without allocating it).
    pub fn new_buffer_shared(&self, shared: Shared) -> u64 {
        let mut inner = self.inner.borrow_mut();
        let handle = Self::fresh(&mut inner);
        inner.buffers.insert(
            handle,
            Buf {
                open: true,
                data: shared,
            },
        );
        handle
    }

    /// The client behind `channel` exited: sends to it fail with `-EPIPE`.
    pub fn kill(&self, channel: u64) {
        if let Some(chan) = self.inner.borrow_mut().channels.get_mut(&channel) {
            chan.dead = true;
        }
    }

    /// Make sends to `channel` fail with `-EAGAIN` (a client that does not read).
    pub fn set_full(&self, channel: u64, full: bool) {
        if let Some(chan) = self.inner.borrow_mut().channels.get_mut(&channel) {
            chan.full = full;
        }
    }

    /// The events the service sent on `channel` since the last call: `(method id, body)`.
    pub fn take_events(&self, channel: u64) -> Vec<(u32, Vec<u8>)> {
        match self.inner.borrow_mut().channels.get_mut(&channel) {
            Some(chan) => chan.events.drain(..).collect(),
            None => Vec::new(),
        }
    }

    /// Whether the service still holds `channel`.
    pub fn channel_open(&self, channel: u64) -> bool {
        self.inner
            .borrow()
            .channels
            .get(&channel)
            .is_some_and(|chan| chan.open)
    }

    /// Channel and buffer handles the service holds and has not closed.
    pub fn open_handles(&self) -> usize {
        let inner = self.inner.borrow();
        inner.channels.values().filter(|chan| chan.open).count()
            + inner.buffers.values().filter(|buf| buf.open).count()
    }

    /// Handles closed more than once (a bug in the service).
    pub fn double_closes(&self) -> usize {
        self.inner.borrow().double_closes
    }

    fn close_buffer_inner(inner: &mut Inner, handle: u64) {
        match inner.buffers.get_mut(&handle) {
            Some(buf) if buf.open => buf.open = false,
            _ => inner.double_closes += 1,
        }
    }
}

struct MemMapping {
    fabric: MemFabric,
    handle: u64,
    data: Shared,
    offset: usize,
    len: usize,
}

impl Mapping for MemMapping {
    fn capacity(&self) -> usize {
        self.len
    }

    fn write(&mut self, bytes: &[u8]) {
        let mut data = self.data.borrow_mut();
        data[self.offset..self.offset + bytes.len()].copy_from_slice(bytes);
    }
}

impl Drop for MemMapping {
    fn drop(&mut self) {
        let mut inner = self.fabric.inner.borrow_mut();
        MemFabric::close_buffer_inner(&mut inner, self.handle);
    }
}

impl Fabric for MemFabric {
    fn send(&self, channel: u64, method: u32, body: Vec<u8>) -> Result<(), i64> {
        let mut inner = self.inner.borrow_mut();
        let Some(chan) = inner.channels.get_mut(&channel).filter(|chan| chan.open) else {
            return Err(-errno::EPIPE);
        };
        if chan.dead {
            return Err(-errno::EPIPE);
        }
        if chan.full {
            return Err(-errno::EAGAIN);
        }
        chan.events.push_back((method, body));
        Ok(())
    }

    fn close_channel(&self, channel: u64) {
        let mut inner = self.inner.borrow_mut();
        match inner.channels.get_mut(&channel) {
            Some(chan) if chan.open => chan.open = false,
            _ => inner.double_closes += 1,
        }
    }

    fn close_buffer(&self, handle: u64) {
        let mut inner = self.inner.borrow_mut();
        Self::close_buffer_inner(&mut inner, handle);
    }

    fn map(&self, buffer: libmessenger::Buffer) -> Result<Box<dyn Mapping>, i64> {
        let data = {
            let mut inner = self.inner.borrow_mut();
            let found = inner
                .buffers
                .get(&buffer.handle)
                .filter(|buf| buf.open)
                .map(|buf| buf.data.clone());
            let Some(data) = found else {
                inner.double_closes += 1;
                return Err(-9);
            };
            let fits = buffer
                .end()
                .is_some_and(|end| end <= data.borrow().len() as u64);
            if !fits {
                Self::close_buffer_inner(&mut inner, buffer.handle);
                return Err(-errno::EINVAL);
            }
            data
        };
        Ok(Box::new(MemMapping {
            fabric: self.clone(),
            handle: buffer.handle,
            data,
            offset: buffer.offset as usize,
            len: buffer.len as usize,
        }))
    }

    fn read(&self, buffer: libmessenger::Buffer) -> Result<Vec<u8>, i64> {
        let mut inner = self.inner.borrow_mut();
        let found = inner
            .buffers
            .get(&buffer.handle)
            .filter(|buf| buf.open)
            .map(|buf| buf.data.clone());
        let Some(data) = found else {
            inner.double_closes += 1;
            return Err(-9);
        };
        Self::close_buffer_inner(&mut inner, buffer.handle);
        let data = data.borrow();
        match buffer.end() {
            Some(end) if end <= data.len() as u64 => {
                Ok(data[buffer.offset as usize..end as usize].to_vec())
            }
            _ => Err(-errno::EINVAL),
        }
    }
}
