//! The raw bus as `inputd` sees it: kernel records in, typed items out.

use alloc::vec::Vec;

use inputmap::pointer::raw;
use inputmap::{RawKey, RawPad, RawPointer};
use user::sys::{self, raw_kind, RawEvent, RAW_EVENT_BYTES};

/// Records drained per syscall.
const BATCH: usize = 64;

/// One thing the bus told us.
pub(super) enum Item {
    Key(RawKey),
    /// Motion, a button edge or the wheel (validated by `inputmap::Pointer`).
    Pointer(RawPointer),
    /// A gamepad axis (`AXIS`): routed by its device's source class.
    Axis(RawPad),
    /// The kernel's `SOURCE` record: a source registered or closed.
    Source(RawPad),
    /// A device's identity (`DEVICE`): diagnostics only, never a class.
    Device {
        device: u8,
        vendor: u16,
        /// Product in the low half, version in the high half.
        info: u32,
    },
    /// The consumer ring overflowed: `lost` events from `seq` on are gone.
    Dropped {
        ts_ns: u64,
        seq: u64,
        lost: u64,
    },
}

pub(super) struct Source {
    buffer: Vec<u8>,
}

impl Source {
    /// Claim the consumer ring. `Err(-EPERM)` when this task lacks
    /// `CAP_INPUT_RAW`, `Err(-EBUSY)` when another consumer holds every slot.
    pub(super) fn open() -> Result<Source, i64> {
        sys::input_raw_open()?;
        Ok(Source {
            buffer: alloc::vec![0u8; RAW_EVENT_BYTES * BATCH],
        })
    }

    /// Feed every queued record to `handle`, oldest first.
    pub(super) fn drain(&mut self, mut handle: impl FnMut(Item)) {
        while let Ok(count) = sys::input_raw_poll(&mut self.buffer) {
            if count == 0 {
                return;
            }
            for index in 0..count {
                if let Some(item) = RawEvent::decode(&self.buffer, index).and_then(item_of) {
                    handle(item);
                }
            }
        }
    }
}

fn pad_record(event: &RawEvent) -> RawPad {
    RawPad {
        seq: event.seq,
        ts_ns: event.ts_ns,
        device: event.device,
        kind: event.kind,
        code: event.code,
        value: event.value,
    }
}

/// Keep key edges, pointer and gamepad records, source announcements and
/// loss markers; `SYNC` and unknown kinds have no consumer.
fn item_of(event: RawEvent) -> Option<Item> {
    match event.kind {
        raw_kind::KEY if matches!(event.value, 0 | 1) => Some(Item::Key(RawKey {
            seq: event.seq,
            ts_ns: event.ts_ns,
            usage: event.code,
            pressed: event.value == 1,
        })),
        kind if raw::is_pointer(kind) => Some(Item::Pointer(RawPointer {
            seq: event.seq,
            ts_ns: event.ts_ns,
            device: event.device,
            kind,
            code: event.code,
            value: event.value,
        })),
        raw_kind::AXIS => Some(Item::Axis(pad_record(&event))),
        raw_kind::SOURCE => Some(Item::Source(pad_record(&event))),
        raw_kind::DEVICE => Some(Item::Device {
            device: event.device,
            vendor: event.code,
            info: event.value as u32,
        }),
        raw_kind::DROPPED => Some(Item::Dropped {
            ts_ns: event.ts_ns,
            seq: event.seq,
            lost: u64::try_from(event.value).unwrap_or(0),
        }),
        _ => None,
    }
}
