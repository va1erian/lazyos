//! A typed per-app settings store over `confd` (`docs/client-api-plan.md`
//! 4.2).
//!
//! `T` is any serde struct with a `Default`. Each leaf field is one `confd`
//! key under `user/<uid>/<app>/`, named by its path (`window.last_size` is
//! `.../window/last_size`), so a setting that already lived at such a key
//! keeps its value. A key that is missing, or holds the wrong kind of value,
//! reads as the field's default: a half-written store never fails the app.
//!
//! Loading happens once, when the store is opened (before the first frame,
//! one bounded `confd` read per field). Saving never does I/O on the caller's
//! thread: only the fields that changed are queued to a worker
//! ([`Saver::Worker`]); a process that exits calls [`flush`] first.
//! Scalars (bool, integers, strings) are native `confd` values; a float or a
//! list is stored as its JSON text in a string key, also inside an `Option`.
//! A map field and an `Option<struct>` are not supported (their keys are not
//! known up front, and data must not name `confd` keys): saving one is a
//! debug assertion.
//!
//! A read that fails (a `confd` that times out) is not a missing key: the
//! field keeps its default for this run and the default is never written over
//! the real value; a field the user changed meanwhile is. A value that does
//! not fit its field costs that field only.
//!
//! Two instances of an app share one subtree and neither follows the other's
//! changes: each writes only the fields it changed, so the last writer of a
//! field wins and a field the other never touched is safe.

use std::cell::{Ref, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value as Json;
use xui_work::{Queue, Sink};

pub use confd::Value;

/// The part of `confd` the store needs.
pub trait KeyValue {
    /// `Ok(None)` is a key that is not there; `Err` a read that failed (a
    /// timeout), which is not the same thing.
    fn get(&self, key: &str) -> Result<Option<Value>, String>;
    fn set(&self, key: &str, value: Value) -> Result<(), String>;
    fn delete(&self, key: &str) -> Result<(), String>;
    /// The kernel-stamped uid that owns `user/<uid>/..`; `None` keeps the
    /// settings in memory.
    fn uid(&self) -> Option<u32>;
}

/// `user/<uid>/<app>/<name>`: where an app's setting lives in `confd`. The one
/// place that spells the layout; `name` may hold `/` for a nested key.
pub fn user_key(uid: u32, app: &str, name: &str) -> String {
    format!("user/{uid}/{app}/{name}")
}

/// Builds another handle on the store for the worker thread.
pub type Factory = Arc<dyn Fn() -> Box<dyn KeyValue> + Send + Sync>;

/// Where a store writes.
#[derive(Clone)]
pub enum Saver {
    /// On the worker thread of the store, through a handle `Factory` builds
    /// there.
    Worker(Factory),
    /// On the calling thread: host tests, where the store is a plain map.
    Inline(Rc<dyn KeyValue>),
}

/// One change on its way to the store: a key and its new value, `None` for
/// a key to delete.
type Change = (String, Option<Value>);

enum Sender {
    Inline(Rc<dyn KeyValue>),
    Worker(Queue<Vec<Change>>),
    /// The worker thread could not start: settings stay in memory.
    Memory,
}

/// A typed settings value with a change-only, off-thread save.
pub struct Settings<T> {
    prefix: Option<String>,
    value: Rc<RefCell<T>>,
    template: Json,
    stored: RefCell<BTreeMap<String, Value>>,
    /// Keys whose read failed: the default is never written to them, so a
    /// slow `confd` cannot make it overwrite the real value. A key leaves the
    /// set once a value of the user's has been written to it, after which it
    /// is an ordinary key (setting it back to the default saves it).
    unread: RefCell<BTreeSet<String>>,
    sender: Sender,
}

impl<T: Serialize + DeserializeOwned + Default> Settings<T> {
    /// Opens the store of `app` (the subtree name under `user/<uid>/`) and
    /// reads it.
    pub fn open(kv: &dyn KeyValue, app: &str, saver: Saver) -> Settings<T> {
        let prefix = kv
            .uid()
            .map(|uid| user_key(uid, app, "").trim_end_matches('/').to_owned());
        let template = serde_json::to_value(T::default()).unwrap_or(Json::Null);
        let mut stored = BTreeMap::new();
        let mut unread = BTreeSet::new();
        let tree = match &prefix {
            Some(prefix) => decode::<T>(&template, prefix, kv, &mut stored, &mut unread),
            None => template.clone(),
        };
        let value = serde_json::from_value(tree).unwrap_or_default();
        Settings {
            sender: sender(saver),
            prefix,
            value: Rc::new(RefCell::new(value)),
            template,
            stored: RefCell::new(stored),
            unread: RefCell::new(unread),
        }
    }

    /// The current value.
    pub fn get(&self) -> Ref<'_, T> {
        self.value.borrow()
    }

    /// Replaces the value and saves what changed.
    pub fn set(&self, value: T) {
        *self.value.borrow_mut() = value;
        self.save();
    }

    /// Changes the value in place and saves what changed.
    pub fn update(&self, change: impl FnOnce(&mut T)) {
        change(&mut self.value.borrow_mut());
        self.save();
    }

    /// The `confd` key of `name` in this app's subtree (`user/<uid>/<app>/..`);
    /// `None` without a uid.
    pub fn key(&self, name: &str) -> Option<String> {
        self.prefix
            .as_ref()
            .map(|prefix| format!("{prefix}/{name}"))
    }

    fn save(&self) {
        let Some(prefix) = &self.prefix else {
            return;
        };
        let Ok(now) = serde_json::to_value(&*self.value.borrow()) else {
            return;
        };
        let mut leaves = BTreeMap::new();
        encode(&now, &self.template, prefix, &mut leaves);
        self.keep_unread_values(&mut leaves);
        let mut stored = self.stored.borrow_mut();
        let mut changes: Vec<Change> = Vec::new();
        for (key, value) in &leaves {
            if stored.get(key) != value.as_ref() {
                changes.push((key.clone(), value.clone()));
            }
        }
        for (key, value) in leaves {
            match value {
                Some(value) => stored.insert(key, value),
                None => stored.remove(&key),
            };
        }
        if !changes.is_empty() {
            self.send(changes);
        }
    }

    /// Drops the leaves of unreadable keys that still hold their default: a
    /// value the user changed since is theirs to write, the default is not.
    /// A key whose leaf survives is being written now, so it is readable (as
    /// far as the store goes) from then on.
    fn keep_unread_values(&self, leaves: &mut BTreeMap<String, Option<Value>>) {
        let mut unread = self.unread.borrow_mut();
        if unread.is_empty() {
            return;
        }
        let mut defaults = BTreeMap::new();
        let prefix = self.prefix.as_deref().unwrap_or_default();
        encode(&self.template, &self.template, prefix, &mut defaults);
        leaves.retain(|key, value| !unread.contains(key) || defaults.get(key) != Some(value));
        unread.retain(|key| !leaves.contains_key(key));
    }

    fn send(&self, changes: Vec<Change>) {
        match &self.sender {
            Sender::Inline(kv) => {
                apply(kv.as_ref(), &changes);
            }
            Sender::Worker(queue) => {
                pending(1);
                if !queue.send(changes) {
                    pending(-1);
                }
            }
            Sender::Memory => {}
        }
    }
}

fn sender(saver: Saver) -> Sender {
    match saver {
        Saver::Inline(kv) => Sender::Inline(kv),
        Saver::Worker(factory) => {
            // Nobody listens for a finished save: the sink only gives the
            // worker somewhere to post its (empty) answers.
            let bell = Sink::from_fn(|()| true);
            // A handle per batch: the factory is cheap (a client over a
            // service the thread already reaches) and the handle need not
            // be `Send`.
            let queue = Queue::start(&bell, "settings-save", move |changes: Vec<Change>| {
                apply(factory().as_ref(), &changes);
                pending(-1);
            });
            queue.map_or(Sender::Memory, Sender::Worker)
        }
    }
}

/// Batches handed to a save worker and not yet written, process-wide.
static PENDING: (Mutex<usize>, Condvar) = (Mutex::new(0), Condvar::new());

fn pending(delta: isize) {
    let mut count = PENDING.0.lock().unwrap_or_else(|e| e.into_inner());
    *count = count.saturating_add_signed(delta);
    PENDING.1.notify_all();
}

/// Waits up to `limit` for the queued saves to reach the store. A process that
/// exits (`std::process::exit` ends the worker threads) calls this first, or
/// the settings changed in its last moments are lost.
pub fn flush(limit: Duration) {
    let deadline = Instant::now() + limit;
    let mut count = PENDING.0.lock().unwrap_or_else(|e| e.into_inner());
    while *count > 0 {
        let Some(left) = deadline.checked_duration_since(Instant::now()) else {
            return;
        };
        count = PENDING
            .1
            .wait_timeout(count, left)
            .unwrap_or_else(|e| e.into_inner())
            .0;
    }
}

/// Writes the changes; a refused write only means the next start reads the
/// old value.
fn apply(kv: &dyn KeyValue, changes: &[Change]) {
    for (key, value) in changes {
        let _ = match value {
            Some(value) => kv.set(key, value.clone()),
            None => kv.delete(key),
        };
    }
}

/// One value the store held, with where it goes in the settings JSON.
struct Stored {
    key: String,
    fields: Vec<String>,
    value: Value,
}

/// Reads every leaf `template` has from the store into `found`. A key whose
/// read failed (`confd` timed out) is put in `unread`, not treated as missing:
/// the next save must not write a default over a value that is really there.
fn read_leaves(
    template: &Json,
    fields: &mut Vec<String>,
    prefix: &str,
    kv: &dyn KeyValue,
    found: &mut Vec<Stored>,
    unread: &mut BTreeSet<String>,
) {
    if let Json::Object(children) = template {
        for (name, child) in children {
            fields.push(name.clone());
            read_leaves(child, fields, prefix, kv, found, unread);
            fields.pop();
        }
        return;
    }
    let key = format!("{prefix}/{}", fields.join("/"));
    match kv.get(&key) {
        Ok(Some(value)) => found.push(Stored {
            key,
            fields: fields.clone(),
            value,
        }),
        Ok(None) => {}
        Err(_) => {
            unread.insert(key);
        }
    }
}

/// The settings JSON of what the store holds, leaf by leaf: a missing,
/// mistyped or unreadable leaf keeps the default's value and never costs the
/// others theirs. `stored` collects what was read, to diff the next save
/// against.
fn decode<T: DeserializeOwned>(
    template: &Json,
    prefix: &str,
    kv: &dyn KeyValue,
    stored: &mut BTreeMap<String, Value>,
    unread: &mut BTreeSet<String>,
) -> Json {
    let mut found = Vec::new();
    read_leaves(template, &mut Vec::new(), prefix, kv, &mut found, unread);
    let mut tree = template.clone();
    for leaf in found {
        let shape = at(template, &leaf.fields);
        // The first reading of the value the field's type accepts: an
        // `Option<f64>` is `null` in the template but text in the store.
        let accepted = readings(shape, &leaf.value).into_iter().find(|reading| {
            let mut probe = template.clone();
            put(&mut probe, &leaf.fields, reading.clone());
            serde_json::from_value::<T>(probe).is_ok()
        });
        if let Some(reading) = accepted {
            put(&mut tree, &leaf.fields, reading);
            stored.insert(leaf.key, leaf.value);
        }
    }
    tree
}

fn at<'a>(json: &'a Json, fields: &[String]) -> &'a Json {
    fields
        .iter()
        .fold(json, |json, name| json.get(name).unwrap_or(&Json::Null))
}

fn put(json: &mut Json, fields: &[String], value: Json) {
    let mut slot = json;
    for name in fields {
        let Some(next) = slot.get_mut(name) else {
            return;
        };
        slot = next;
    }
    *slot = value;
}

/// A stored value as the JSON a field shaped like `template` could take,
/// likeliest first.
fn readings(template: &Json, value: &Value) -> Vec<Json> {
    let mut out = Vec::new();
    match (template, value) {
        (Json::Bool(_) | Json::Null, Value::Bool(flag)) => out.push(Json::Bool(*flag)),
        (Json::Number(_) | Json::Null, Value::I64(n)) => out.push(Json::from(*n)),
        (Json::Number(_) | Json::Null, Value::U64(n)) => out.push(Json::from(*n)),
        (Json::String(_) | Json::Null, Value::Str(text)) => {
            out.push(Json::String(text.clone()));
        }
        _ => {}
    }
    if let (Json::Number(_) | Json::Array(_) | Json::Null, Value::Str(text)) = (template, value) {
        // A float or a list is stored as its JSON text.
        out.extend(
            serde_json::from_str::<Json>(text)
                .ok()
                .filter(|json| json.is_number() || json.is_array()),
        );
    }
    out
}

/// The leaves of `now` by key; `None` marks a field that is `null` (an unset
/// `Option`), whose key is deleted. A nested object `template` does not have
/// (a map's entries, an `Option<Struct>`) cannot be read back; it is a
/// programming error, refused here instead of written as keys named by data.
fn encode(now: &Json, template: &Json, path: &str, out: &mut BTreeMap<String, Option<Value>>) {
    match now {
        Json::Object(fields) => {
            for (name, field) in fields {
                let Some(shape) = template.get(name) else {
                    debug_assert!(
                        false,
                        "settings: {path}/{name}: map and Option<struct> fields are not supported"
                    );
                    continue;
                };
                encode(field, shape, &format!("{path}/{name}"), out);
            }
        }
        Json::Null => {
            out.insert(path.to_owned(), None);
        }
        Json::Bool(flag) => {
            out.insert(path.to_owned(), Some(Value::Bool(*flag)));
        }
        Json::Number(n) => {
            let value = if let Some(n) = n.as_i64() {
                Value::I64(n)
            } else if let Some(n) = n.as_u64() {
                Value::U64(n)
            } else {
                Value::Str(n.to_string())
            };
            out.insert(path.to_owned(), Some(value));
        }
        Json::String(text) => {
            out.insert(path.to_owned(), Some(Value::Str(text.clone())));
        }
        Json::Array(_) => {
            out.insert(path.to_owned(), Some(Value::Str(now.to_string())));
        }
    }
}

/// A [`KeyValue`] in memory, for host tests.
#[derive(Default)]
pub struct MemKv {
    map: RefCell<BTreeMap<String, Value>>,
    uid: Option<u32>,
    fail_reads: std::cell::Cell<bool>,
}

impl MemKv {
    /// An empty store for user `uid`.
    pub fn new(uid: u32) -> MemKv {
        MemKv {
            map: RefCell::default(),
            uid: Some(uid),
            fail_reads: std::cell::Cell::default(),
        }
    }

    /// An empty store with no uid: settings stay in memory.
    pub fn anonymous() -> MemKv {
        MemKv::default()
    }

    /// Stores `value` at `key` directly (a value an older build wrote).
    pub fn put(&self, key: &str, value: Value) {
        self.map.borrow_mut().insert(key.to_owned(), value);
    }

    /// Makes every later read fail, as a `confd` that times out does.
    pub fn fail_reads(&self, fail: bool) {
        self.fail_reads.set(fail);
    }

    /// The keys held now.
    pub fn keys(&self) -> Vec<String> {
        self.map.borrow().keys().cloned().collect()
    }
}

impl KeyValue for MemKv {
    fn get(&self, key: &str) -> Result<Option<Value>, String> {
        if self.fail_reads.get() {
            return Err("read failed".into());
        }
        Ok(self.map.borrow().get(key).cloned())
    }

    fn set(&self, key: &str, value: Value) -> Result<(), String> {
        self.put(key, value);
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<(), String> {
        self.map.borrow_mut().remove(key);
        Ok(())
    }

    fn uid(&self) -> Option<u32> {
        self.uid
    }
}

#[cfg(test)]
mod tests;
