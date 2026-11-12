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
//! ([`Saver::Worker`]). Scalars (bool, integers, strings) are native `confd`
//! values; a float or a list is stored as its JSON text in a string key. A
//! map field is not supported (its keys are not known up front).

use std::cell::{Ref, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value as Json;
use xui_work::{Queue, Sink};

pub use confd::Value;

/// The part of `confd` the store needs.
pub trait KeyValue {
    fn get(&self, key: &str) -> Option<Value>;
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
    sender: Sender,
    saved: Arc<Mutex<Option<Sink<()>>>>,
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
        let tree = match &prefix {
            Some(prefix) => decode(&template, prefix, kv, &mut stored),
            None => template.clone(),
        };
        let value = serde_json::from_value(tree).unwrap_or_default();
        let saved: Arc<Mutex<Option<Sink<()>>>> = Arc::default();
        Settings {
            sender: sender(saver, &saved),
            prefix,
            value: Rc::new(RefCell::new(value)),
            template,
            stored: RefCell::new(stored),
            saved,
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

    /// Rings `sink` each time a save has reached the store, so a page that
    /// shows "saved" (or an app that mirrors the store) hears of it as a
    /// message of its own.
    pub fn on_saved(&self, sink: Sink<()>) {
        *self.saved.lock().unwrap_or_else(|e| e.into_inner()) = Some(sink);
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

    fn send(&self, changes: Vec<Change>) {
        match &self.sender {
            Sender::Inline(kv) => {
                apply(kv.as_ref(), &changes);
                ring(&self.saved);
            }
            Sender::Worker(queue) => {
                queue.send(changes);
            }
            Sender::Memory => {}
        }
    }
}

fn sender(saver: Saver, saved: &Arc<Mutex<Option<Sink<()>>>>) -> Sender {
    match saver {
        Saver::Inline(kv) => Sender::Inline(kv),
        Saver::Worker(factory) => {
            let notify = Arc::clone(saved);
            let bell = Sink::from_fn(move |()| {
                ring(&notify);
                true
            });
            // A handle per batch: the factory is cheap (a client over a
            // service the thread already reaches) and the handle need not
            // be `Send`.
            let queue = Queue::start(&bell, "settings-save", move |changes: Vec<Change>| {
                apply(factory().as_ref(), &changes);
            });
            queue.map_or(Sender::Memory, Sender::Worker)
        }
    }
}

fn ring(saved: &Mutex<Option<Sink<()>>>) {
    if let Some(sink) = saved.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        sink.send(());
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

/// Reads every leaf `template` has from the store: a missing or mistyped
/// value keeps the template's (the default's). `stored` collects what was
/// read, to diff the next save against.
fn decode(
    template: &Json,
    path: &str,
    kv: &dyn KeyValue,
    stored: &mut BTreeMap<String, Value>,
) -> Json {
    if let Json::Object(fields) = template {
        let read = fields
            .iter()
            .map(|(name, field)| {
                (
                    name.clone(),
                    decode(field, &format!("{path}/{name}"), kv, stored),
                )
            })
            .collect();
        return Json::Object(read);
    }
    let Some(value) = kv.get(path) else {
        return template.clone();
    };
    let read = leaf(template, &value);
    if read.is_some() {
        stored.insert(path.to_owned(), value);
    }
    read.unwrap_or_else(|| template.clone())
}

/// A stored value as the JSON of a field shaped like `template`.
fn leaf(template: &Json, value: &Value) -> Option<Json> {
    match (template, value) {
        (Json::Bool(_) | Json::Null, Value::Bool(flag)) => Some(Json::Bool(*flag)),
        (Json::Number(_) | Json::Null, Value::I64(n)) => Some(Json::from(*n)),
        (Json::Number(_) | Json::Null, Value::U64(n)) => Some(Json::from(*n)),
        (Json::String(_) | Json::Null, Value::Str(text)) => Some(Json::String(text.clone())),
        (Json::Number(n), Value::Str(text)) => parse_number(n, text),
        (Json::Array(_), Value::Str(text)) => {
            serde_json::from_str(text).ok().filter(Json::is_array)
        }
        _ => None,
    }
}

fn parse_number(shape: &serde_json::Number, text: &str) -> Option<Json> {
    if shape.is_f64() {
        text.parse::<f64>()
            .ok()
            .and_then(|n| serde_json::Number::from_f64(n).map(Json::Number))
    } else {
        text.parse::<i64>().ok().map(Json::from)
    }
}

/// The leaves of `now` by key; `None` marks a field that is `null` (an unset
/// `Option`), whose key is deleted.
fn encode(now: &Json, template: &Json, path: &str, out: &mut BTreeMap<String, Option<Value>>) {
    match now {
        Json::Object(fields) => {
            for (name, field) in fields {
                let shape = template.get(name).unwrap_or(&Json::Null);
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
}

impl MemKv {
    /// An empty store for user `uid`.
    pub fn new(uid: u32) -> MemKv {
        MemKv {
            map: RefCell::default(),
            uid: Some(uid),
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

    /// The keys held now.
    pub fn keys(&self) -> Vec<String> {
        self.map.borrow().keys().cloned().collect()
    }
}

impl KeyValue for MemKv {
    fn get(&self, key: &str) -> Option<Value> {
        self.map.borrow().get(key).cloned()
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
