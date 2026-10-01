//! The raw input event bus (`docs/input-plan.md`, layer 1).
//!
//! Device drivers [`publish`] fixed-size, timestamped, HID-coded events; a
//! consumer holding the `input.raw` capability ([`credentials::CAP_INPUT_RAW`],
//! granted only to `inputd`) drains its own bounded ring through syscall 25
//! ([`super::rawsys`]). The bus is deliberately dumb: no layout, no repeat, no
//! focus. Those are `inputd` policy.
//!
//! Loss is never silent. Each consumer ring keeps the newest [`RING_CAP`]
//! events; when a producer outruns the consumer the *oldest* events are
//! discarded and counted, and the next drain yields one
//! [`kind::DROPPED`] record (in the position the loss happened: at the head)
//! whose `seq` is the first lost sequence number and whose `value` is the
//! number lost. Sequence numbers are global and gapless, so
//! `seq(next) == seq(previous) + 1` holds across every record *including* a
//! `Dropped` record's `value` span, and a consumer can resynchronise (release
//! every key it thinks is held) instead of guessing.
//!
//! Pointer records ([`kind::REL_MOTION`], [`kind::ABS_MOTION`], [`kind::SCROLL`])
//! are **tail-merged** so a chatty mouse cannot evict key events: a new one is
//! folded into the newest queued record when that record is the bus's last
//! publication, from the same device, of the same kind and code, and still
//! sits at the tail of every live ring (see [`merge_tail`]). The merged record
//! keeps its `seq`, so the stream stays gapless and identical in every ring;
//! keys and buttons are edges and never merge. Encodings are in [`pointer`].
//! (Merged deltas reach `inputd` summed, so it clamps the sum, not each step;
//! `docs/usb-hid-plan.md`, risk 4.)
//!
//! Locking: one leaf spin lock guards sequence assignment and every ring, so
//! records reach all rings in the same order. The producer runs in IRQ
//! context; every consumer-side entry point disables interrupts around the
//! lock, and nothing under it allocates or touches user memory.

use alloc::vec::Vec;
use spin::Mutex;

use crate::task;

/// Bytes per encoded [`RawEvent`] on the syscall wire.
pub const RAW_EVENT_BYTES: usize = 24;

/// Events one consumer ring retains. A power of two so the index wrap is a mask.
pub const RING_CAP: usize = 256;

/// Independent consumers the bus serves (the capability gate is what keeps
/// this to `inputd`; more than one slot lets tests prove ring independence).
pub const MAX_CONSUMERS: usize = 4;

/// Raw event kinds. `SYNC` is reserved; the pointer kinds are encoded as
/// [`pointer`] describes (`docs/usb-hid-plan.md`, decision 2).
#[allow(dead_code)] // `SYNC` has no producer yet
pub mod kind {
    pub const KEY: u8 = 1;
    pub const REL_MOTION: u8 = 2;
    pub const ABS_MOTION: u8 = 3;
    pub const BUTTON: u8 = 4;
    pub const SCROLL: u8 = 5;
    pub const SYNC: u8 = 6;
    /// Consumer-side marker: `value` events were lost starting at `seq`.
    pub const DROPPED: u8 = 7;
}

/// Device ids. Assigned by the kernel driver, so a client can never forge one.
pub mod device {
    /// The i8042 PS/2 keyboard.
    pub const PS2_KEYBOARD: u8 = 1;
    /// The i8042 PS/2 auxiliary-port mouse.
    pub const PS2_MOUSE: u8 = 2;
}

/// The device-independent pointer encoding (`docs/usb-hid-plan.md`).
///
/// | kind | code | value |
/// |---|---|---|
/// | `REL_MOTION` | 0 | `dx` low `i16`, `dy` high `i16`: counts, screen-oriented (`dy > 0` is down) |
/// | `ABS_MOTION` | 0 | `x` low `u16`, `y` high `u16`, normalised to `0..=0xFFFF` |
/// | `BUTTON` | HID button usage ([`button`](pointer::button)) | `0` release, `1` press |
/// | `SCROLL` | [`VERTICAL`](pointer::VERTICAL) or [`HORIZONTAL`](pointer::HORIZONTAL) | signed notches, `> 0` is up / right |
///
/// Producers split deltas wider than `i16` across records.
pub mod pointer {
    use super::kind;

    /// HID button usages (page 0x09).
    #[allow(dead_code)] // back/forward have no producer until USB mice
    pub mod button {
        pub const LEFT: u16 = 1;
        pub const RIGHT: u16 = 2;
        pub const MIDDLE: u16 = 3;
        pub const BACK: u16 = 4;
        pub const FORWARD: u16 = 5;
    }

    /// `SCROLL` codes.
    pub const VERTICAL: u16 = 0;
    #[allow(dead_code)] // no PS/2 horizontal wheel
    pub const HORIZONTAL: u16 = 1;

    /// Pack a relative motion.
    pub fn pack_rel(dx: i16, dy: i16) -> i32 {
        (u32::from(dx as u16) | (u32::from(dy as u16) << 16)) as i32
    }

    /// Unpack a relative motion: `(dx, dy)`.
    pub fn unpack_rel(value: i32) -> (i16, i16) {
        (
            value as u32 as u16 as i16,
            ((value as u32) >> 16) as u16 as i16,
        )
    }

    /// Pack an absolute position.
    #[allow(dead_code)] // no kernel producer: absolute devices are USB
    pub fn pack_abs(x: u16, y: u16) -> i32 {
        (u32::from(x) | (u32::from(y) << 16)) as i32
    }

    /// Unpack an absolute position: `(x, y)`.
    #[allow(dead_code)]
    pub fn unpack_abs(value: i32) -> (u16, u16) {
        (value as u32 as u16, ((value as u32) >> 16) as u16)
    }

    /// Whether records of `kind` may be folded into their predecessor.
    pub fn mergeable(kind: u8) -> bool {
        matches!(kind, kind::REL_MOTION | kind::ABS_MOTION | kind::SCROLL)
    }

    /// `older` and `newer` combined: deltas add (saturating), positions
    /// replace. Only meaningful for [`mergeable`] kinds.
    pub fn merge(kind: u8, older: i32, newer: i32) -> i32 {
        match kind {
            kind::REL_MOTION => {
                let (ax, ay) = unpack_rel(older);
                let (bx, by) = unpack_rel(newer);
                pack_rel(ax.saturating_add(bx), ay.saturating_add(by))
            }
            kind::SCROLL => older.saturating_add(newer),
            _ => newer,
        }
    }
}

/// Key event values. The kernel never synthesises repeat (`docs/input-plan.md`).
pub mod value {
    pub const RELEASE: i32 = 0;
    pub const PRESS: i32 = 1;
}

/// One event: `#[repr(C)]`, 24 bytes, little-endian on the wire.
///
/// `code` is a USB HID usage (page 0x07) for [`kind::KEY`]. `device` and `kind`
/// are one byte each (the plan sketched `u16`; a 24-byte record with a full
/// `i32` value needs the narrower fields, and 255 devices/kinds is ample).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RawEvent {
    pub seq: u64,
    /// Monotonic nanoseconds since boot, at PIT-tick (10 ms) resolution.
    pub ts_ns: u64,
    pub device: u8,
    pub kind: u8,
    pub code: u16,
    pub value: i32,
}

const _: () = assert!(core::mem::size_of::<RawEvent>() == RAW_EVENT_BYTES);

impl RawEvent {
    const ZERO: RawEvent = RawEvent {
        seq: 0,
        ts_ns: 0,
        device: 0,
        kind: 0,
        code: 0,
        value: 0,
    };

    /// The wire encoding.
    pub fn to_bytes(self) -> [u8; RAW_EVENT_BYTES] {
        let mut out = [0u8; RAW_EVENT_BYTES];
        out[0..8].copy_from_slice(&self.seq.to_le_bytes());
        out[8..16].copy_from_slice(&self.ts_ns.to_le_bytes());
        out[16] = self.device;
        out[17] = self.kind;
        out[18..20].copy_from_slice(&self.code.to_le_bytes());
        out[20..24].copy_from_slice(&self.value.to_le_bytes());
        out
    }
}

/// Why a bus operation failed; `rawsys` maps these to errnos.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Every consumer slot is held by a live task.
    Full,
    /// The id names no consumer, or one owned by someone else.
    BadId,
}

/// One consumer's bounded ring plus its loss accounting.
struct Ring {
    buf: [RawEvent; RING_CAP],
    /// Index of the oldest retained event.
    head: usize,
    len: usize,
    /// Events discarded since the last drain, and the first one's sequence.
    lost: u32,
    lost_first_seq: u64,
}

impl Ring {
    const fn new() -> Self {
        Ring {
            buf: [RawEvent::ZERO; RING_CAP],
            head: 0,
            len: 0,
            lost: 0,
            lost_first_seq: 0,
        }
    }

    fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
        self.lost = 0;
    }

    /// Append, discarding (and counting) the oldest event when full.
    fn push(&mut self, event: RawEvent) {
        if self.len == RING_CAP {
            let oldest = self.buf[self.head];
            if self.lost == 0 {
                self.lost_first_seq = oldest.seq;
            }
            self.lost = self.lost.saturating_add(1);
            self.head = (self.head + 1) & (RING_CAP - 1);
            self.len -= 1;
        }
        self.buf[(self.head + self.len) & (RING_CAP - 1)] = event;
        self.len += 1;
    }

    /// Pop up to `max` records, a `Dropped` marker first when events were lost.
    fn drain(&mut self, max: usize, out: &mut Vec<RawEvent>) {
        if max == 0 {
            return;
        }
        let mut room = max;
        if self.lost > 0 {
            out.push(RawEvent {
                seq: self.lost_first_seq,
                ts_ns: now_ns(),
                device: 0,
                kind: kind::DROPPED,
                code: 0,
                value: self.lost.min(i32::MAX as u32) as i32,
            });
            self.lost = 0;
            room -= 1;
        }
        let take = room.min(self.len);
        for _ in 0..take {
            out.push(self.buf[self.head]);
            self.head = (self.head + 1) & (RING_CAP - 1);
        }
        self.len -= take;
    }

    /// Buffer index of the newest retained record, if any.
    fn newest(&self) -> Option<usize> {
        let back = self.len.checked_sub(1)?;
        Some((self.head + back) & (RING_CAP - 1))
    }

    fn tail(&self) -> Option<&RawEvent> {
        self.newest().map(|index| &self.buf[index])
    }

    fn tail_mut(&mut self) -> Option<&mut RawEvent> {
        self.newest().map(|index| &mut self.buf[index])
    }
}

struct Slot {
    /// Task slot of the owner; `None` when free.
    owner: Option<usize>,
    ring: Ring,
}

struct Bus {
    next_seq: u64,
    slots: [Slot; MAX_CONSUMERS],
}

const FREE_SLOT: Slot = Slot {
    owner: None,
    ring: Ring::new(),
};

static BUS: Mutex<Bus> = Mutex::new(Bus {
    next_seq: 1,
    slots: [FREE_SLOT; MAX_CONSUMERS],
});

fn now_ns() -> u64 {
    task::ticks().saturating_mul(10_000_000)
}

/// Publish one event to every consumer ring. Called from IRQ context (and from
/// tests); never blocks and never allocates.
pub fn publish(device: u8, kind: u8, code: u16, value: i32) {
    let ts_ns = now_ns();
    let mut bus = BUS.lock();
    if pointer::mergeable(kind) && merge_tail(&mut bus, device, kind, code, value, ts_ns) {
        return;
    }
    let seq = bus.next_seq;
    bus.next_seq += 1;
    let event = RawEvent {
        seq,
        ts_ns,
        device,
        kind,
        code,
        value,
    };
    for slot in bus.slots.iter_mut().filter(|slot| slot.owner.is_some()) {
        slot.ring.push(event);
    }
}

/// Fold a pointer record into the newest one, if every live ring still holds
/// the bus's last publication at its tail and it matches `device`, `kind` and
/// `code`. All or nothing: rings must stay identical, and a ring that already
/// drained the record would otherwise see a delta it never gets. Returns
/// whether the record was absorbed (it then consumes no `seq`).
fn merge_tail(bus: &mut Bus, device: u8, kind: u8, code: u16, value: i32, ts_ns: u64) -> bool {
    let last = bus.next_seq - 1;
    let mut live = bus
        .slots
        .iter()
        .filter(|slot| slot.owner.is_some())
        .peekable();
    let at_tail = |slot: &Slot| {
        slot.ring.tail().is_some_and(|tail| {
            tail.seq == last && tail.device == device && tail.kind == kind && tail.code == code
        })
    };
    if live.peek().is_none() || !live.all(at_tail) {
        return false;
    }
    for slot in bus.slots.iter_mut().filter(|slot| slot.owner.is_some()) {
        if let Some(tail) = slot.ring.tail_mut() {
            tail.value = pointer::merge(kind, tail.value, value);
            tail.ts_ns = ts_ns;
        }
    }
    true
}

/// Run `body` with the bus locked and interrupts off (the IRQ producer takes
/// the same lock).
fn locked<R>(body: impl FnOnce(&mut Bus) -> R) -> R {
    x86_64::instructions::interrupts::without_interrupts(|| body(&mut BUS.lock()))
}

/// Claim a consumer slot for task `owner`, reclaiming slots whose owner died.
/// Re-opening returns the caller's existing slot with its ring untouched.
pub fn open(owner: usize) -> Result<usize, Error> {
    locked(|bus| {
        if let Some(id) = bus.slots.iter().position(|slot| slot.owner == Some(owner)) {
            return Ok(id);
        }
        let id = bus
            .slots
            .iter()
            .position(|slot| slot.owner.is_none_or(|task| !task::live(task)))
            .ok_or(Error::Full)?;
        bus.slots[id].owner = Some(owner);
        bus.slots[id].ring.clear();
        Ok(id)
    })
}

/// Release consumer `id`, which must belong to `owner`.
pub fn close(id: usize, owner: usize) -> Result<(), Error> {
    locked(|bus| {
        let slot = bus.slots.get_mut(id).ok_or(Error::BadId)?;
        if slot.owner != Some(owner) {
            return Err(Error::BadId);
        }
        slot.owner = None;
        slot.ring.clear();
        Ok(())
    })
}

/// The consumer slot task `owner` holds, if any.
pub fn consumer_of(owner: usize) -> Option<usize> {
    locked(|bus| bus.slots.iter().position(|slot| slot.owner == Some(owner)))
}

/// Pop up to `max` records for consumer `id` (owned by `owner`) into `out`.
pub fn drain(id: usize, owner: usize, max: usize, out: &mut Vec<RawEvent>) -> Result<(), Error> {
    locked(|bus| {
        let slot = bus.slots.get_mut(id).ok_or(Error::BadId)?;
        if slot.owner != Some(owner) {
            return Err(Error::BadId);
        }
        slot.ring.drain(max, out);
        Ok(())
    })
}

/// Test-harness hook: free every consumer and restart sequence numbering.
#[cfg(lazyos_tests)]
pub fn reset() {
    locked(|bus| {
        bus.next_seq = 1;
        for slot in bus.slots.iter_mut() {
            slot.owner = None;
            slot.ring.clear();
        }
    });
}
