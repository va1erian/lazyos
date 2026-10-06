//! Host tests of the split virtqueue against a fake device, including the
//! hostile used-ring entries it must refuse.

use super::*;
use std::vec::Vec;

/// A queue over host memory plus a fake device that completes requests.
struct Rig {
    block: Vec<u32>,
    queue: Virtqueue,
    device_seen: u16,
    device_used: u16,
}

impl Rig {
    fn new(size: u16) -> Rig {
        let words = Virtqueue::bytes_needed(size).div_ceil(4);
        let mut block = std::vec![0u32; words];
        let mem = block.as_mut_ptr() as *mut u8;
        // SAFETY: `block` is 4-aligned, big enough, and lives as long as
        // the rig; the fake device below is the only other accessor.
        let queue = unsafe { Virtqueue::new(mem, 0x10_0000, size) }.expect("queue");
        Rig {
            block,
            queue,
            device_seen: 0,
            device_used: 0,
        }
    }

    fn mem(&mut self) -> *mut u8 {
        self.block.as_mut_ptr() as *mut u8
    }

    /// Complete the oldest available request with `written` bytes.
    fn device_complete(&mut self, written: u32) -> Option<u16> {
        let layout = Layout::new(self.queue.size());
        let size = self.queue.size();
        let mem = self.mem();
        // SAFETY: offsets come from `Layout`, inside the block.
        unsafe {
            let avail_idx = ptr::read_volatile(mem.add(layout.avail + 2) as *const u16);
            if avail_idx == self.device_seen {
                return None;
            }
            let slot = usize::from(self.device_seen % size);
            let head = ptr::read_volatile(mem.add(layout.avail + 4 + 2 * slot) as *const u16);
            self.device_seen = self.device_seen.wrapping_add(1);
            let out = usize::from(self.device_used % size);
            let element = mem.add(layout.used + 4 + 8 * out);
            ptr::write_volatile(element as *mut u32, u32::from(head));
            ptr::write_volatile(element.add(4) as *mut u32, written);
            self.device_used = self.device_used.wrapping_add(1);
            ptr::write_volatile(mem.add(layout.used + 2) as *mut u16, self.device_used);
            Some(head)
        }
    }

    fn desc(&mut self, index: u16) -> (u64, u32, u16, u16) {
        let mem = self.mem();
        // SAFETY: `index` is in range at every call in these tests.
        unsafe {
            let desc = mem.add(usize::from(index) * DESC_SIZE);
            (
                ptr::read_volatile(desc as *const u64),
                ptr::read_volatile(desc.add(8) as *const u32),
                ptr::read_volatile(desc.add(12) as *const u16),
                ptr::read_volatile(desc.add(14) as *const u16),
            )
        }
    }
}

fn out(bus: u64, len: u32) -> Buf {
    Buf {
        bus,
        len,
        device_writes: false,
    }
}

fn inn(bus: u64, len: u32) -> Buf {
    Buf {
        bus,
        len,
        device_writes: true,
    }
}

#[test]
fn layout_matches_the_spec_sizes() {
    // 8 entries: desc 128, avail 2+2+16+2 = 22 -> used at 152 (aligned),
    // used 2+2+64+2 = 70.
    let layout = Layout::new(8);
    assert_eq!(layout.avail, 128);
    assert_eq!(layout.used, 152);
    assert_eq!(layout.total, 152 + 70);
}

#[test]
fn rejects_bad_sizes_and_alignment() {
    let mut block = std::vec![0u32; 1024];
    let mem = block.as_mut_ptr() as *mut u8;
    for size in [0u16, 3, 257, 512, 1024] {
        // SAFETY: never dereferenced: the size check fails first.
        assert!(unsafe { Virtqueue::new(mem, 0x1000, size) }.is_err());
    }
    // SAFETY: misaligned bus address is rejected before any access.
    assert!(unsafe { Virtqueue::new(mem, 0x1002, 8) }.is_err());
}

#[test]
fn a_three_descriptor_chain_round_trips() {
    let mut rig = Rig::new(8);
    let head = rig
        .queue
        .add(&[out(0x1000, 8), out(0x2000, 512), inn(0x3000, 8)])
        .expect("add");
    assert_eq!(rig.queue.num_free(), 5);
    let (addr, len, flags, next) = rig.desc(head);
    assert_eq!((addr, len, flags), (0x1000, 8, F_NEXT));
    let (_, _, flags2, next2) = rig.desc(next);
    assert_eq!(flags2, F_NEXT);
    let (addr3, _, flags3, _) = rig.desc(next2);
    assert_eq!((addr3, flags3), (0x3000, F_WRITE));

    assert_eq!(rig.queue.pop_used(), Ok(None));
    assert_eq!(rig.device_complete(8), Some(head));
    assert_eq!(rig.queue.pop_used(), Ok(Some(Used { head, len: 8 })));
    assert_eq!(rig.queue.num_free(), 8);
    assert_eq!(rig.queue.pop_used(), Ok(None));
}

#[test]
fn exhaustion_is_reported_and_recovers() {
    let mut rig = Rig::new(4);
    let a = rig.queue.add(&[out(0x1000, 4), inn(0x2000, 4)]).expect("a");
    let b = rig.queue.add(&[out(0x3000, 4), inn(0x4000, 4)]).expect("b");
    assert_eq!(rig.queue.add(&[out(0x5000, 4)]), Err(Error::QueueFull));
    rig.device_complete(0);
    assert_eq!(rig.queue.pop_used().unwrap().unwrap().head, a);
    assert!(rig.queue.add(&[out(0x5000, 4)]).is_ok());
    rig.device_complete(0);
    assert_eq!(rig.queue.pop_used().unwrap().unwrap().head, b);
}

#[test]
fn the_default_ring_of_256_entries_fills_drains_and_wraps() {
    let mut rig = Rig::new(256);
    assert_eq!(rig.queue.size() as usize, MAX_QUEUE);
    assert_eq!(Layout::new(256).total, 6670);
    for round in 0..5u32 {
        let mut heads = Vec::new();
        for i in 0..256u32 {
            heads.push(
                rig.queue
                    .add(&[inn(0x10_0000 + u64::from(i) * 0x800, 2048)])
                    .expect("add"),
            );
        }
        assert_eq!(rig.queue.num_free(), 0);
        assert_eq!(rig.queue.add(&[inn(0, 1)]), Err(Error::QueueFull));
        for (i, head) in heads.iter().enumerate() {
            assert_eq!(rig.device_complete(60 + round), Some(*head));
            assert_eq!(
                rig.queue.pop_used(),
                Ok(Some(Used {
                    head: *head,
                    len: 60 + round
                })),
                "{i}"
            );
        }
        assert_eq!(rig.queue.num_free(), 256);
    }
}

#[test]
fn a_255_descriptor_chain_fits_the_bookkeeping() {
    let mut rig = Rig::new(256);
    let chain = [out(0x1000, 4); 255];
    let head = rig.queue.add(&chain).expect("chain");
    assert_eq!(rig.queue.num_free(), 1);
    assert_eq!(
        rig.queue.add(&[out(0, 1); 256]),
        Err(Error::BadRequest),
        "a chain of 256 is refused"
    );
    rig.device_complete(0);
    assert_eq!(rig.queue.pop_used().unwrap().unwrap().head, head);
    assert_eq!(rig.queue.num_free(), 256);
}

#[test]
fn empty_and_oversized_requests_are_refused() {
    let mut rig = Rig::new(4);
    assert_eq!(rig.queue.add(&[]), Err(Error::BadRequest));
    let five = [out(0, 1); 5];
    assert_eq!(rig.queue.add(&five), Err(Error::BadRequest));
}

#[test]
fn indices_wrap_past_u16() {
    let mut rig = Rig::new(4);
    // 70k round trips crosses the 16-bit index wrap several times.
    for round in 0..70_000u32 {
        let head = rig
            .queue
            .add(&[out(0x1000, 4), inn(0x2000, 4)])
            .expect("add");
        assert_eq!(rig.device_complete(round), Some(head));
        let used = rig.queue.pop_used().expect("pop").expect("some");
        assert_eq!(used, Used { head, len: round });
        assert_eq!(rig.queue.num_free(), 4);
    }
}

#[test]
fn interleaved_completion_keeps_the_free_list_intact() {
    let mut rig = Rig::new(8);
    let mut heads = Vec::new();
    for index in 0..4u64 {
        heads.push(rig.queue.add(&[out(index, 1), inn(index, 1)]).expect("add"));
    }
    for expected in heads {
        rig.device_complete(1);
        assert_eq!(rig.queue.pop_used().unwrap().unwrap().head, expected);
    }
    // Everything is free again and reusable in full.
    assert_eq!(rig.queue.num_free(), 8);
    let all: Vec<Buf> = (0..8).map(|i| out(i, 1)).collect();
    assert!(rig.queue.add(&all).is_ok());
}

#[test]
fn hostile_used_entries_are_rejected() {
    // An id outside the queue.
    let mut rig = Rig::new(4);
    rig.queue.add(&[out(0x1000, 4)]).expect("add");
    rig.device_complete(0);
    let layout = Layout::new(4);
    let mem = rig.mem();
    // SAFETY: overwrite element 0's id inside the block.
    unsafe { ptr::write_volatile(mem.add(layout.used + 4) as *mut u32, 99) };
    assert_eq!(rig.queue.pop_used(), Err(Error::DeviceError));

    // A completion for a head that is not in flight.
    let mut rig = Rig::new(4);
    rig.queue.add(&[out(0x1000, 4)]).expect("add");
    rig.device_complete(0);
    let mem = rig.mem();
    // SAFETY: element 0's id now names descriptor 2, which is idle.
    unsafe { ptr::write_volatile(mem.add(layout.used + 4) as *mut u32, 2) };
    assert_eq!(rig.queue.pop_used(), Err(Error::DeviceError));

    // A used index that jumps further than anything submitted.
    let mut rig = Rig::new(4);
    rig.queue.add(&[out(0x1000, 4)]).expect("add");
    let mem = rig.mem();
    // SAFETY: the used index sits at offset 2 of the used ring.
    unsafe { ptr::write_volatile(mem.add(layout.used + 2) as *mut u16, 3) };
    assert_eq!(rig.queue.pop_used(), Err(Error::DeviceError));
}
