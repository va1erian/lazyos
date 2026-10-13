//! The interned label table (application package system, phase 1).
//!
//! A [`super::credentials::Cred`] carries a small `label_id`; this table maps
//! the id back to the bounded string that names it. Labels have exactly four
//! forms, and nothing else is accepted:
//!
//! * `app:<reverse.dns.name>` -- a sandboxed application (`app:com.example.notes`),
//! * `system:<name>` -- a platform service (`system:netdrv`),
//! * `dev:<reverse.dns.name>` -- an app being developed, run from an IDE with the
//!   permissions its manifest declares (`dev:com.example.notes`, issue #529). It
//!   owns the same `app.<name>.*` services and `app/<name>/` topics as the
//!   installed app; a labelled IDE may spawn into it only when its own rules
//!   allow that exact label ([`super::devspawn`]),
//! * `app:<reverse.dns.name>/<name>` -- a *child* of an installed app
//!   (`app:org.lazy.webkit/webprocess`, issue #832, [`super::children`]): a
//!   helper program that runs with no more than the app holds. It owns no
//!   names or topics, and its parent label must already be interned (the
//!   link is part of the label, read back by [`parent_of`]).
//!
//! Id `0` is "unlabelled" and never names a string. The table is bounded
//! ([`CAPACITY`]) and append-only: interning an equal string returns the
//! existing id, so a label id is stable for the life of the boot and a
//! credential that recorded it never dangles. Only the credential gate
//! (`process::creds`, which requires `CAP_SETUID`) and the policy loader
//! (`CAP_IPC_CONTROL`) reach [`intern`] (a labelled IDE's spawn into a `dev:`
//! label only [`lookup`]s: it can never add one); a full table refuses new labels
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
/// Prefix of a development label (an app run from an IDE).
pub const DEV_PREFIX: &str = "dev:";

/// Longest child name (the part after the `/`), in bytes; the same bound as
/// `lazypkg::MAX_PROCESS_NAME`, so `app:<system_name>/<name>` always fits
/// [`MAX_LABEL_BYTES`] (4 + 128 + 1 + 24).
pub const MAX_CHILD_NAME_BYTES: usize = 24;

/// Why a label string or an interning request was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    /// Empty, too long, wrong form or a byte outside the label's charset.
    Malformed,
    /// The table already holds [`CAPACITY`] distinct labels.
    TableFull,
    /// A child label (`app:<id>/<name>`) whose app label is not interned.
    NoParent,
}

/// What a valid label names.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// `app:<id>`.
    App,
    /// `system:<name>`.
    System,
    /// `dev:<id>`: the app `<id>` run from an IDE under its manifest's rules.
    Dev,
    /// `app:<id>/<name>`: a helper program of the installed app `<id>`.
    Child,
}

/// Whether `part` is a dotted app id or a `system:`/`dev:` name: bytes of
/// `[a-z0-9.-]`, never starting or ending with `.` or `-`, no empty dot
/// segment, so an app id maps to exactly one reverse-DNS name.
fn valid_name(part: &str) -> bool {
    let edge = |b: u8| matches!(b, b'.' | b'-');
    !part.is_empty()
        && !part.contains("..")
        && !edge(part.as_bytes()[0])
        && !edge(part.as_bytes()[part.len() - 1])
        && part
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-'))
}

/// Whether `name` is a child name: `[a-z0-9]` first, then `[a-z0-9_-]`, at
/// most [`MAX_CHILD_NAME_BYTES`] bytes (the manifest grammar,
/// `lazypkg::valid_process_name`).
fn valid_child_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= MAX_CHILD_NAME_BYTES
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'))
}

/// Split a label into its kind and the part after the prefix, validating
/// every byte. The name part may contain `a-z 0-9 . -`, never starts or ends
/// with `.` or `-`, and has no empty dot segment, so an app id maps to
/// exactly one reverse-DNS name. An `app:` label may add `/<child>` (see
/// [`valid_child_name`]); the part returned for a [`Kind::Child`] is then
/// `<id>/<child>`.
pub fn parse(label: &str) -> Result<(Kind, &str), Error> {
    if label.is_empty() || label.len() > MAX_LABEL_BYTES {
        return Err(Error::Malformed);
    }
    let (kind, name) = if let Some(name) = label.strip_prefix(APP_PREFIX) {
        (Kind::App, name)
    } else if let Some(name) = label.strip_prefix(SYSTEM_PREFIX) {
        (Kind::System, name)
    } else if let Some(name) = label.strip_prefix(DEV_PREFIX) {
        (Kind::Dev, name)
    } else {
        return Err(Error::Malformed);
    };
    match (kind, name.split_once('/')) {
        (Kind::App, Some((id, child))) if valid_name(id) && valid_child_name(child) => {
            Ok((Kind::Child, name))
        }
        (_, Some(_)) => Err(Error::Malformed),
        (_, None) if valid_name(name) => Ok((kind, name)),
        (_, None) => Err(Error::Malformed),
    }
}

/// The parent label (`app:<id>`) of a child label string, `None` when
/// `label` is not a child label.
pub fn parent_label(label: &str) -> Option<&str> {
    match parse(label) {
        Ok((Kind::Child, _)) => label.rsplit_once('/').map(|(parent, _)| parent),
        _ => None,
    }
}

/// One interned label: the validated bytes and their length, its kind and,
/// for a child, the id of the app label it belongs to (`0` otherwise).
#[derive(Clone, Copy)]
struct Slot {
    len: u8,
    kind: Kind,
    parent: u32,
    bytes: [u8; MAX_LABEL_BYTES],
}

impl Slot {
    const EMPTY: Slot = Slot {
        len: 0,
        kind: Kind::App,
        parent: 0,
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
/// Equal strings always return the same id. A malformed label, a child whose
/// app label is not interned, or a full table is refused without changing
/// anything.
pub fn intern(label: &str) -> Result<u32, Error> {
    let (kind, _) = parse(label)?;
    let mut table = TABLE.lock();
    if let Some(index) = table.slots[..table.used]
        .iter()
        .position(|slot| slot.as_str() == label)
    {
        return Ok(index as u32 + 1);
    }
    let parent = match kind {
        Kind::Child => {
            let parent = parent_label(label).ok_or(Error::Malformed)?;
            let index = table.slots[..table.used]
                .iter()
                .position(|slot| slot.as_str() == parent)
                .ok_or(Error::NoParent)?;
            index as u32 + 1
        }
        _ => 0,
    };
    if table.used >= CAPACITY {
        return Err(Error::TableFull);
    }
    let index = table.used;
    table.slots[index].len = label.len() as u8;
    table.slots[index].kind = kind;
    table.slots[index].parent = parent;
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
    let table = TABLE.lock();
    let index = (id as usize).checked_sub(1)?;
    (index < table.used).then(|| table.slots[index].kind)
}

/// The app label a child label `id` belongs to; `None` for every other
/// label, `0` and unknown ids. The link is fixed when the child is interned.
pub fn parent_of(id: u32) -> Option<u32> {
    let table = TABLE.lock();
    let index = (id as usize).checked_sub(1)?;
    (index < table.used)
        .then(|| table.slots[index].parent)
        .filter(|parent| *parent != 0)
}

/// Most distinct child labels one app may ever have interned (the table is
/// append-only, so a label stays after its binding goes: an app that renamed
/// its tables on every upgrade would otherwise fill it for everyone).
pub const MAX_CHILD_LABELS_PER_APP: usize = 8;

/// How many child labels of the app label `parent` are interned.
pub fn child_label_count(parent: u32) -> usize {
    let table = TABLE.lock();
    table.slots[..table.used]
        .iter()
        .filter(|slot| slot.parent == parent && slot.kind == Kind::Child)
        .count()
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
    {
        let mut table = TABLE.lock();
        table.used = 0;
        table.slots = [Slot::EMPTY; CAPACITY];
    }
    // A binding names a label id; with the ids gone it would dangle.
    super::children::reset_for_tests();
}
