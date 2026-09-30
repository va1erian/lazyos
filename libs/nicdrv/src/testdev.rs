//! A fake virtio-net device for host tests and fuzzing.
//!
//! It works on the same memory as the driver's [`Queues`](crate::Queues),
//! which a test builds with `bus == va` so that the fake can follow a
//! descriptor's address with a plain pointer. It plays the device's side of the
//! split virtqueues: it takes buffers from the available rings and writes
//! completions to the used rings, either honestly ([`FakeDevice::deliver`],
//! [`FakeDevice::transmitted`]) or as a hostile device would (arbitrary used
//! ids and lengths, used indices that run ahead).

use core::ptr;
use std::vec::Vec;

use virtio::queue::Layout as QueueLayout;
use virtio_net::hdr::{NetHdr, HDR_LEN};
use virtio_net::SLOT_BYTES;

use crate::queues::{DmaBlock, Layout};
use crate::Doorbell;

/// Which queue a raw used entry goes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Which {
    Rx,
    Tx,
}

#[derive(Default)]
struct Side {
    /// Available entries the device has consumed.
    seen: u16,
    /// Used entries the device has published.
    used: u16,
}

pub struct FakeDevice {
    base: *mut u8,
    layout: Layout,
    rx: Side,
    tx: Side,
    /// Kicks the driver sent, per queue.
    pub rx_kicks: u32,
    pub tx_kicks: u32,
}

impl FakeDevice {
    pub fn new(block: &DmaBlock, layout: &Layout) -> FakeDevice {
        FakeDevice {
            base: block.va(),
            layout: *layout,
            rx: Side::default(),
            tx: Side::default(),
            rx_kicks: 0,
            tx_kicks: 0,
        }
    }

    fn queue(&self, which: Which) -> (*mut u8, u16, QueueLayout) {
        let (offset, size) = match which {
            Which::Rx => (self.layout.rx_queue, self.layout.rx_entries),
            Which::Tx => (self.layout.tx_queue, self.layout.tx_entries),
        };
        // SAFETY: the queue window is inside the block (`Layout::total`).
        (
            unsafe { self.base.add(offset) },
            size,
            QueueLayout::new(size),
        )
    }

    fn side(&mut self, which: Which) -> &mut Side {
        match which {
            Which::Rx => &mut self.rx,
            Which::Tx => &mut self.tx,
        }
    }

    /// Available buffers the device has not taken yet.
    pub fn available(&mut self, which: Which) -> u16 {
        let (mem, _, layout) = self.queue(which);
        // SAFETY: the avail index is at offset 2 of the avail ring.
        let idx = unsafe { ptr::read_volatile(mem.add(layout.avail + 2) as *const u16) };
        idx.wrapping_sub(self.side(which).seen)
    }

    /// Take the next available chain head and the first descriptor's
    /// `(address, length)`.
    fn take(&mut self, which: Which) -> Option<(u16, u64, u32)> {
        if self.available(which) == 0 {
            return None;
        }
        let (mem, size, layout) = self.queue(which);
        let seen = self.side(which).seen;
        // SAFETY: ring entry `seen % size` and descriptor `head` (< size, the
        // driver wrote it) are inside the queue window.
        let (head, addr, len) = unsafe {
            let head = ptr::read_volatile(
                mem.add(layout.avail + 4 + 2 * usize::from(seen % size)) as *const u16
            );
            let desc = mem.add(usize::from(head % size) * 16);
            (
                head,
                ptr::read_volatile(desc as *const u64),
                ptr::read_volatile(desc.add(8) as *const u32),
            )
        };
        self.side(which).seen = seen.wrapping_add(1);
        Some((head, addr, len))
    }

    /// Publish a used entry `{id, len}` and advance the used index.
    pub fn write_used_raw(&mut self, which: Which, id: u32, len: u32) {
        let (mem, size, layout) = self.queue(which);
        let used = self.side(which).used;
        // SAFETY: used element `used % size` and the used index are inside the window.
        unsafe {
            let element = mem.add(layout.used + 4 + 8 * usize::from(used % size));
            ptr::write_volatile(element as *mut u32, id);
            ptr::write_volatile(element.add(4) as *mut u32, len);
            self.side(which).used = used.wrapping_add(1);
            ptr::write_volatile(mem.add(layout.used + 2) as *mut u16, self.side(which).used);
        }
    }

    /// Overwrite the used index outright (a device claiming completions it
    /// never made).
    pub fn set_used_index(&mut self, which: Which, index: u16) {
        let (mem, _, layout) = self.queue(which);
        // SAFETY: the used index is at offset 2 of the used ring.
        unsafe { ptr::write_volatile(mem.add(layout.used + 2) as *mut u16, index) };
    }

    /// Receive `frame` the honest way: the packet header, then the frame, into
    /// the next posted buffer. `false` when the driver has none posted.
    pub fn deliver(&mut self, frame: &[u8]) -> bool {
        let mut bytes = Vec::with_capacity(HDR_LEN + frame.len());
        bytes.extend_from_slice(&NetHdr::PLAIN.encode());
        bytes.extend_from_slice(frame);
        self.deliver_raw(&bytes, bytes.len() as u32)
    }

    /// Write `bytes` (truncated to the slot) into the next posted buffer and
    /// report `claimed` as the used length, which may be a lie.
    pub fn deliver_raw(&mut self, bytes: &[u8], claimed: u32) -> bool {
        let Some((head, addr, len)) = self.take(Which::Rx) else {
            return false;
        };
        let n = bytes.len().min(len as usize).min(SLOT_BYTES);
        // SAFETY: `addr` is the bus (= virtual) address of a driver slot of
        // `len` bytes; `n <= len`.
        unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), addr as *mut u8, n) };
        self.write_used_raw(Which::Rx, u32::from(head), claimed);
        true
    }

    /// Transmit side: take the next queued frame (without the packet header)
    /// and complete it. `None` when nothing is queued.
    pub fn transmitted(&mut self) -> Option<Vec<u8>> {
        let (head, addr, len) = self.take(Which::Tx)?;
        let len = (len as usize).min(SLOT_BYTES);
        let mut bytes = std::vec![0u8; len];
        // SAFETY: `addr` is a driver slot of at least `len` bytes.
        unsafe { ptr::copy_nonoverlapping(addr as *const u8, bytes.as_mut_ptr(), len) };
        self.write_used_raw(Which::Tx, u32::from(head), 0);
        assert!(
            len >= HDR_LEN,
            "a transmitted buffer always carries the packet header"
        );
        assert_eq!(
            &bytes[..HDR_LEN],
            &NetHdr::PLAIN.encode(),
            "transmit header is the plain one"
        );
        Some(bytes[HDR_LEN..].to_vec())
    }
}

impl Doorbell for FakeDevice {
    fn ring(&mut self, queue: u16) {
        match queue {
            virtio_net::queue::RX => self.rx_kicks += 1,
            _ => self.tx_kicks += 1,
        }
    }
}

/// A driver engine wired to a fake device and (optionally) a client, all in
/// host memory with guard pages. The "client" is the stack's side of the two
/// rings: it consumes the receive ring and produces into the transmit ring.
pub mod bed {
    use framering::fuzz::Mem;
    use framering::{ring_bytes, Consumer, Producer, Ring};

    use super::FakeDevice;
    use crate::engine::AttachError;
    use crate::queues::{DmaBlock, Layout, Queues};
    use crate::Engine;

    pub const MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
    pub const OWNER: u64 = 7;

    pub struct Client {
        pub mem: Mem,
        pub slots: u32,
        /// Receive ring: the client consumes.
        pub rx: Consumer,
        /// Transmit ring: the client produces.
        pub tx: Producer,
        pub ring: u32,
    }

    pub struct Bed {
        pub dma: Mem,
        pub engine: Engine,
        pub dev: FakeDevice,
        pub client: Option<Client>,
    }

    impl Bed {
        pub fn new(rx_entries: u16, tx_entries: u16) -> Bed {
            let layout = Layout::new(rx_entries, tx_entries).expect("layout");
            let mut dma = Mem::with_len(layout.total);
            let base = dma.base();
            // SAFETY: `dma` lives in the bed next to the engine and is never
            // moved (its allocation is stable); bus == va for the fake device.
            let block = unsafe { DmaBlock::new(base, base as u64, layout.total) };
            // SAFETY: the block is exclusively the queues' from here on.
            let mut queues = unsafe { Queues::new(block, rx_entries, tx_entries) }.expect("queues");
            queues.post_all_rx().expect("post");
            let dev = FakeDevice::new(&block, queues.layout());
            let engine = Engine::new(queues, MAC, 1514, true);
            Bed {
                dma,
                engine,
                dev,
                client: None,
            }
        }

        /// Create the client's rings and attach them as `owner`.
        pub fn attach(&mut self, slots: u32, owner: u64) -> Result<u32, AttachError> {
            let one = ring_bytes(slots);
            let mut mem = Mem::with_len(if one == 0 { 4096 } else { one * 2 });
            let base = mem.base();
            if one != 0 {
                // SAFETY: both halves are inside `mem`, which the client keeps.
                let (rx, tx) = unsafe {
                    (
                        Ring::create(base, one, slots).expect("rx ring"),
                        Ring::create(base.add(one), one, slots).expect("tx ring"),
                    )
                };
                let len = mem.len();
                // SAFETY: `mem` stays alive in `Client` until the test ends.
                let ring = unsafe { self.engine.attach(owner, slots, base, len) }?;
                self.client = Some(Client {
                    mem,
                    slots,
                    rx: rx.consumer(),
                    tx: tx.producer(),
                    ring,
                });
                Ok(ring)
            } else {
                // SAFETY: a refused attach never touches the memory.
                unsafe { self.engine.attach(owner, slots, base, mem.len()) }
            }
        }

        pub fn client(&mut self) -> &mut Client {
            self.client.as_mut().expect("a client is attached")
        }

        pub fn pump(&mut self) -> crate::PumpOutcome {
            self.engine.pump(&mut self.dev).expect("no device fault")
        }

        pub fn assert_guards(&self) {
            self.dma.assert_guards();
            if let Some(client) = &self.client {
                client.mem.assert_guards();
            }
        }
    }
}
