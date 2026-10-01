//! The raw bus as `inputd` sees it: kernel records in, typed items out.

use alloc::vec::Vec;

use inputmap::pointer::raw;
use inputmap::{RawKey, RawPointer};
use user::sys::{self, raw_kind, RawEvent, RAW_EVENT_BYTES};

/// Records drained per syscall.
const BATCH: usize = 64;

/// One thing the bus told us.
pub(super) enum Item {
    Key(RawKey),
    /// Motion, a button edge or the wheel (validated by `inputmap::Pointer`).
    Pointer(RawPointer),
    /// The consumer ring overflowed: the events from `seq` on are gone.
    Dropped {
        ts_ns: u64,
        seq: u64,
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

/// Keep key edges, pointer records and loss markers; `SYNC` and unknown kinds
/// have no consumer.
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
        raw_kind::DROPPED => Some(Item::Dropped {
            ts_ns: event.ts_ns,
            seq: event.seq,
        }),
        _ => None,
    }
}
