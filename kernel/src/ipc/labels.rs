//! The interned label table (application package system, phase 1).
//!
//! A [`super::credentials::Cred`] carries a small `label_id`; this table maps
//! the id back to the bounded string that names it. Labels have exactly two
//! forms, and nothing else is accepted:
//!
//! * `app:<reverse.dns.name>` -- a sandboxed application (`app:com.example.notes`),
//! * `system:<name>` -- a platform service (`system:netdrv`).
//!
//! Id `0` is "unlabelled" and never names a string. The table is bounded
//! ([`CAPACITY`]) and append-only: interning an equal string returns the
//! existing id, so a label id is stable for the life of the boot and a
//! credential that recorded it never dangles. Only the credential gate
//! (`process::creds`, which requires `CAP_SETUID`) and the policy loader
//! (`CAP_IPC_CONTROL`) reach [`intern`]; a full table refuses new labels
//! instead of evicting one, because eviction would let a stale id alias a
//! different application's identity.

use alloc::string::String;
use spin::Mutex;

/// Longest label, in bytes.
pub const MAX_LABEL_BYTES: usize = 160;
/// Most distinct labels the table holds (ids `1..=CAPACITY`).
pub const CAPACITY: usize = 256;
/// The id every unlabelled task carries.
pub const UNLABELLED: u32 = 0;

/// Prefix of an application label.
pub const APP_PREFIX: &str = "app:";
/// Prefix of a platform-service label.
pub const SYSTEM_PREFIX: &str = "system:";

/// Why a label string or an interning request was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    /// Empty, too long, wrong form or a byte outside `[a-z0-9.:-]`.
    Malformed,
    /// The table already holds [`CAPACITY`] distinct labels.
    TableFull,
}

/// What a valid label names.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// `app:<id>`.
    App,
    /// `system:<name>`.
    System,
}

/// Split a label into its kind and the part after the prefix, validating
/// every byte. The name part may contain `a-z 0-9 . -` but no `:`, never
/// starts or ends with `.` or `-`, and has no empty dot segment, so an app id
/// maps to exactly one reverse-DNS name.
pub fn parse(label: &str) -> Result<(Kind, &str), Error> {
    if label.is_empty() || label.len() > MAX_LABEL_BYTES {
        return Err(Error::Malformed);
    }
    if !label
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b':' | b'-'))
    {
        return Err(Error::Malformed);
    }
    let (kind, name) = if let Some(name) = label.strip_prefix(APP_PREFIX) {
        (Kind::App, name)
    } else if let Some(name) = label.strip_prefix(SYSTEM_PREFIX) {
        (Kind::System, name)
    } else {
        return Err(Error::Malformed);
    };
    let edge = |b: u8| matches!(b, b'.' | b'-');
    let ok = !name.is_empty()
        && !name.contains(':')
        && !name.contains("..")
        && !edge(name.as_bytes()[0])
        && !edge(name.as_bytes()[name.len() - 1]);
    if ok {
        Ok((kind, name))
    } else {
        Err(Error::Malformed)
    }
}

/// One interned label: the validated bytes and their length.
#[derive(Clone, Copy)]
struct Slot {
    len: u8,
    bytes: [u8; MAX_LABEL_BYTES],
}

impl Slot {
    const EMPTY: Slot = Slot {
        len: 0,
        bytes: [0; MAX_LABEL_BYTES],
    };

    fn as_str(&self) -> &str {
        // The bytes were validated as ASCII by `parse` before being stored.
        core::str::from_utf8(&self.bytes[..self.len as usize]).unwrap_or("")
    }
}

struct Table {
    used: usize,
    slots: [Slot; CAPACITY],
}

static TABLE: Mutex<Table> = Mutex::new(Table {
    used: 0,
    slots: [Slot::EMPTY; CAPACITY],
});

/// The id of `label`, adding it when new.
///
/// Equal strings always return the same id. A malformed label or a full table
/// is refused without changing anything.
pub fn intern(label: &str) -> Result<u32, Error> {
    parse(label)?;
    let mut table = TABLE.lock();
    if let Some(index) = table.slots[..table.used]
        .iter()
        .position(|slot| slot.as_str() == label)
    {
        return Ok(index as u32 + 1);
    }
    if table.used >= CAPACITY {
        return Err(Error::TableFull);
    }
    let index = table.used;
    table.slots[index].len = label.len() as u8;
    table.slots[index].bytes[..label.len()].copy_from_slice(label.as_bytes());
    table.used += 1;
    Ok(index as u32 + 1)
}

/// The id of an already-interned `label`, without adding it.
pub fn lookup(label: &str) -> Option<u32> {
    let table = TABLE.lock();
    table.slots[..table.used]
        .iter()
        .position(|slot| slot.as_str() == label)
        .map(|index| index as u32 + 1)
}

/// Run `f` on the string for `id`; `None` for `0` or an unknown id.
pub fn with<R>(id: u32, f: impl FnOnce(&str) -> R) -> Option<R> {
    let table = TABLE.lock();
    let index = (id as usize).checked_sub(1)?;
    (index < table.used).then(|| f(table.slots[index].as_str()))
}

/// The label string for `id`, copied out.
pub fn name_of(id: u32) -> Option<String> {
    with(id, |label| String::from(label))
}

/// The kind of `id`, or `None` when unlabelled or unknown.
pub fn kind_of(id: u32) -> Option<Kind> {
    with(id, |label| parse(label).ok().map(|(kind, _)| kind)).flatten()
}

/// Number of interned labels.
pub fn count() -> usize {
    TABLE.lock().used
}

/// The label string for `id` as seen by `actor_slot`.
///
/// Reading a label is allowed to a `CAP_SETUID` holder (init, login) and to
/// the labelled task itself, so an app can learn its own label but not
/// enumerate its neighbours'. Unknown ids and `0` are `None`.
pub fn read_name(
    actor_slot: usize,
    id: u32,
) -> Result<Option<String>, super::credentials::TransitionError> {
    use super::credentials::{self, TransitionError, CAP_SETUID};
    let actor = credentials::of(actor_slot);
    if !actor.has_cap(CAP_SETUID) && (id == 0 || actor.label_id != id) {
        return Err(TransitionError::NotPrivileged);
    }
    Ok(name_of(id))
}

/// Forget every label. Test isolation only: a running system never shrinks
/// the table, since live credentials hold its ids.
#[cfg(lazyos_tests)]
pub fn reset_for_tests() {
    let mut table = TABLE.lock();
    table.used = 0;
    table.slots = [Slot::EMPTY; CAPACITY];
}
