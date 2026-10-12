//! The Terminal's settings from `confd`: read at start, and read again when
//! the user's keys change. The preferences dialog (T6), the Config app and
//! `rhai` (`sys::confd::set`) all write `user/<uid>/term/...`; this follows
//! them. Parsing and validation are `lazyterm::settings` (host-tested).

use lazyterm::settings::{Settings, Value, KEYS};
use xui_app::platform::confd_store::ConfdStore;
use xui_app::platform::messenger::Service;
use xui_app::platform::topic_feed::TopicFeed;
use messenger_generated::os_lazy_messenger_topics_v1 as topics;
use xui_app::sys;
use xui_settings::store::{ConfigStore, Value as StoreValue};

/// Ticks (100 Hz) between looks at the change topic.
const LOOK_TICKS: u64 = 50;
/// Ticks between re-reads of the desktop's motion setting (it has no topic
/// of its own here): ten seconds.
const MOTION_TICKS: u64 = 1000;

/// Everything the Terminal reads from the store.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Conf {
    pub settings: Settings,
    /// `sys/ui/motion` is `none`: no animation, so no blinking cursor.
    pub motion_none: bool,
}

/// `confd`'s registered name: a store call blocks (up to ten seconds) while it
/// is not registered, so nothing is read unless it already is.
const CONFD: &str = "os.lazy.confd";

pub struct Follower {
    store: ConfdStore,
    available: bool,
    uid: Option<u32>,
    changes: Option<TopicFeed>,
    next_motion: u64,
    current: Conf,
}

fn convert(value: StoreValue) -> Option<Value> {
    match value {
        StoreValue::Bool(b) => Some(Value::Bool(b)),
        StoreValue::I64(n) => Some(Value::Int(n)),
        StoreValue::U64(n) => Some(Value::Int(i64::try_from(n).unwrap_or(i64::MAX))),
        StoreValue::Str(s) => Some(Value::Text(s)),
        StoreValue::Bytes(_) => None,
    }
}

impl Follower {
    pub fn new() -> Follower {
        let store = ConfdStore::new();
        let available = Service::try_connect(CONFD).is_some();
        let uid = if available { store.uid() } else { None };
        let changes = uid.map(|uid| {
            TopicFeed::new(
                format!("user/{uid}/confd/changed/#"),
                topics::QOS_LATEST,
                8,
                LOOK_TICKS,
            )
        });
        Follower {
            store,
            available,
            uid,
            changes,
            next_motion: 0,
            current: Conf::default(),
        }
    }

    /// Read everything now.
    pub fn read(&mut self) -> Conf {
        let mut settings = Settings::default();
        if let Some(uid) = self.uid {
            for key in KEYS {
                let path = format!("user/{uid}/term/{key}");
                if let Some(value) = self.store.get(&path).and_then(convert) {
                    settings.apply(key, &value);
                }
            }
        }
        let motion = if self.available { self.motion() } else { String::new() };
        self.next_motion = sys::clock_ticks().saturating_add(MOTION_TICKS);
        self.current = Conf {
            settings,
            motion_none: motion == "none",
        };
        self.current.clone()
    }

    /// The motion setting: the user's own value, else the machine's.
    fn motion(&self) -> String {
        let key = "sys/ui/motion";
        let own = self
            .uid
            .and_then(|uid| uitheme::user_key(uid, key))
            .and_then(|path| self.store.get(&path));
        match uitheme::overlay(self.store.get(key), own) {
            Some(StoreValue::Str(text)) => text,
            _ => String::new(),
        }
    }

    /// Remember the window's grid (`window/last_size`, per user), unless it
    /// is already what is stored. A refused write (the label may not allow
    /// it) is not an error: the size is only a convenience.
    pub fn save_last_size(&mut self, cols: usize, rows: usize) {
        let Some(uid) = self.uid.filter(|_| self.available) else {
            return;
        };
        if self.current.settings.last_size == Some((cols, rows)) {
            return;
        }
        let path = format!("user/{uid}/term/window/last_size");
        let _ = self.store.set(&path, StoreValue::Str(format!("{cols}x{rows}")));
    }

    /// Look for changes; the new configuration when something differs.
    pub fn poll(&mut self) -> Option<Conf> {
        let changed = self
            .changes
            .as_mut()
            .is_some_and(|feed| !feed.poll().is_empty());
        let motion_due = sys::clock_ticks() >= self.next_motion;
        if !changed && !motion_due {
            return None;
        }
        let before = self.current.clone();
        let now = self.read();
        (now != before).then_some(now)
    }
}
