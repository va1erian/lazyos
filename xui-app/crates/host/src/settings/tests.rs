//! Host tests of the typed settings store.

use super::*;
use serde::Deserialize;
use std::sync::Mutex;

#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
struct Window {
    width: u32,
    height: u32,
}

#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
struct Prefs {
    font: String,
    wrap: bool,
    scale: f64,
    printer: Option<String>,
    recent: Vec<String>,
    window: Window,
}

fn open(kv: &Rc<MemKv>) -> Settings<Prefs> {
    Settings::open(kv.as_ref(), "demo", Saver::Inline(kv.clone()))
}

#[test]
fn an_empty_store_reads_as_the_default() {
    let kv = Rc::new(MemKv::new(1000));
    assert_eq!(*open(&kv).get(), Prefs::default());
}

#[test]
fn a_change_lands_in_one_key_per_field_and_reads_back() {
    let kv = Rc::new(MemKv::new(1000));
    let settings = open(&kv);
    settings.update(|p| {
        p.font = "Mono".into();
        p.scale = 1.5;
        p.recent = vec!["/a".into(), "/b".into()];
        p.window.width = 640;
        p.printer = Some("lp".into());
    });
    assert_eq!(
        kv.get("user/1000/demo/window/width").unwrap(),
        Some(Value::I64(640))
    );
    assert_eq!(
        kv.get("user/1000/demo/font").unwrap(),
        Some(Value::Str("Mono".into()))
    );
    let again = open(&kv);
    assert_eq!(again.get().scale, 1.5);
    assert_eq!(again.get().recent, ["/a", "/b"]);
    assert_eq!(again.get().window.width, 640);
    assert_eq!(again.get().printer.as_deref(), Some("lp"));
}

#[test]
fn a_value_an_older_build_wrote_is_kept() {
    let kv = Rc::new(MemKv::new(7));
    kv.put("user/7/demo/printer", Value::Str("old".into()));
    kv.put("user/7/demo/wrap", Value::Bool(true));
    let settings = open(&kv);
    assert_eq!(settings.get().printer.as_deref(), Some("old"));
    assert!(settings.get().wrap);
}

#[test]
fn a_value_of_the_wrong_kind_is_the_default() {
    let kv = Rc::new(MemKv::new(7));
    kv.put("user/7/demo/wrap", Value::Str("yes".into()));
    kv.put("user/7/demo/window/width", Value::Bool(true));
    let settings = open(&kv);
    assert!(!settings.get().wrap);
    assert_eq!(settings.get().window.width, 0);
}

#[test]
fn only_what_changed_is_written() {
    let kv = Rc::new(MemKv::new(7));
    let settings = open(&kv);
    settings.update(|p| p.wrap = true);
    let before = kv.keys();
    assert!(before.contains(&"user/7/demo/wrap".to_owned()));
    kv.put("user/7/demo/font", Value::Str("outside".into()));
    // An unchanged field is not written back over another writer's value.
    settings.update(|p| p.wrap = true);
    assert_eq!(
        kv.get("user/7/demo/font").unwrap(),
        Some(Value::Str("outside".into()))
    );
}

#[test]
fn unsetting_an_option_deletes_its_key() {
    let kv = Rc::new(MemKv::new(7));
    let settings = open(&kv);
    settings.update(|p| p.printer = Some("lp".into()));
    assert!(kv.get("user/7/demo/printer").unwrap().is_some());
    settings.update(|p| p.printer = None);
    assert!(kv.get("user/7/demo/printer").unwrap().is_none());
}

#[test]
fn without_a_uid_the_settings_live_in_memory() {
    let kv = Rc::new(MemKv::anonymous());
    let settings = open(&kv);
    settings.update(|p| p.font = "x".into());
    assert_eq!(settings.get().font, "x");
    assert!(kv.keys().is_empty());
    assert_eq!(settings.key("a"), None);
}

#[test]
fn the_worker_saves_off_the_calling_thread() {
    use std::time::{Duration, Instant};

    struct Shared(Arc<Mutex<BTreeMap<String, Value>>>);
    impl KeyValue for Shared {
        fn get(&self, key: &str) -> Result<Option<Value>, String> {
            Ok(self.0.lock().unwrap().get(key).cloned())
        }
        fn set(&self, key: &str, value: Value) -> Result<(), String> {
            self.0.lock().unwrap().insert(key.to_owned(), value);
            Ok(())
        }
        fn delete(&self, key: &str) -> Result<(), String> {
            self.0.lock().unwrap().remove(key);
            Ok(())
        }
        fn uid(&self) -> Option<u32> {
            Some(5)
        }
    }
    let map = Arc::new(Mutex::new(BTreeMap::new()));
    let factory_map = Arc::clone(&map);
    let factory: Factory = Arc::new(move || Box::new(Shared(Arc::clone(&factory_map))));
    let settings: Settings<Prefs> =
        Settings::open(&Shared(Arc::clone(&map)), "demo", Saver::Worker(factory));
    settings.update(|p| p.font = "Serif".into());
    let started = Instant::now();
    while !map.lock().unwrap().contains_key("user/5/demo/font")
        && started.elapsed() < Duration::from_secs(5)
    {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        map.lock().unwrap().get("user/5/demo/font"),
        Some(&Value::Str("Serif".into()))
    );
}

#[test]
fn a_failed_read_is_not_a_missing_key_and_is_never_written_over() {
    let kv = Rc::new(MemKv::new(7));
    kv.put("user/7/demo/font", Value::Str("Mono".into()));
    kv.put("user/7/demo/wrap", Value::Bool(true));
    kv.fail_reads(true);
    let settings = open(&kv);
    assert_eq!(settings.get().font, "");
    kv.fail_reads(false);
    settings.update(|p| p.scale = 2.0);
    // The save wrote the field the user changed and left the unreadable
    // ones at their default alone.
    assert_eq!(
        kv.get("user/7/demo/font").unwrap(),
        Some(Value::Str("Mono".into()))
    );
    assert_eq!(kv.get("user/7/demo/wrap").unwrap(), Some(Value::Bool(true)));
    assert!(kv.get("user/7/demo/scale").unwrap().is_some());
}

#[test]
fn an_unread_key_the_user_set_and_then_reset_to_the_default_is_saved() {
    let kv = Rc::new(MemKv::new(7));
    kv.put("user/7/demo/font", Value::Str("Mono".into()));
    kv.fail_reads(true);
    let settings = open(&kv);
    kv.fail_reads(false);
    // Left at the default, the unreadable key is not written over.
    settings.update(|p| p.scale = 2.0);
    assert_eq!(
        kv.get("user/7/demo/font").unwrap(),
        Some(Value::Str("Mono".into()))
    );
    // The user's own value is written...
    settings.update(|p| p.font = "Serif".into());
    assert_eq!(
        kv.get("user/7/demo/font").unwrap(),
        Some(Value::Str("Serif".into()))
    );
    // ...and choosing the default again is a choice too.
    settings.update(|p| p.font = String::new());
    assert_eq!(
        kv.get("user/7/demo/font").unwrap(),
        Some(Value::Str(String::new()))
    );
}

#[test]
fn an_unread_option_set_and_then_cleared_is_deleted() {
    let kv = Rc::new(MemKv::new(7));
    kv.put("user/7/opt/count", Value::I64(3));
    kv.fail_reads(true);
    let settings: Settings<Optional> =
        Settings::open(kv.as_ref(), "opt", Saver::Inline(kv.clone()));
    kv.fail_reads(false);
    settings.update(|p| p.count = Some(9));
    assert!(kv.get("user/7/opt/count").unwrap().is_some());
    settings.update(|p| p.count = None);
    assert_eq!(kv.get("user/7/opt/count").unwrap(), None);
}

#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
struct Optional {
    ratio: Option<f64>,
    list: Option<Vec<String>>,
    count: Option<u32>,
    name: String,
}

#[test]
fn optional_floats_and_lists_round_trip() {
    let kv = Rc::new(MemKv::new(3));
    let settings: Settings<Optional> =
        Settings::open(kv.as_ref(), "opt", Saver::Inline(kv.clone()));
    let want = Optional {
        ratio: Some(1.5),
        list: Some(vec!["a".into(), "b".into()]),
        count: Some(4),
        name: "n".into(),
    };
    settings.set(want.clone());
    let again: Settings<Optional> = Settings::open(kv.as_ref(), "opt", Saver::Inline(kv.clone()));
    assert_eq!(*again.get(), want);
}

#[test]
fn one_bad_leaf_does_not_reset_the_others() {
    let kv = Rc::new(MemKv::new(3));
    kv.put("user/3/opt/ratio", Value::Str("not a number".into()));
    kv.put("user/3/opt/count", Value::Str("lots".into()));
    kv.put("user/3/opt/name", Value::Str("kept".into()));
    let settings: Settings<Optional> =
        Settings::open(kv.as_ref(), "opt", Saver::Inline(kv.clone()));
    assert_eq!(settings.get().name, "kept");
    assert_eq!(settings.get().ratio, None);
}

#[derive(Serialize, Deserialize, Default)]
struct WithMap {
    by_name: BTreeMap<String, u32>,
}

#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "not supported")]
fn a_map_field_is_refused() {
    let kv = Rc::new(MemKv::new(3));
    let settings: Settings<WithMap> = Settings::open(kv.as_ref(), "map", Saver::Inline(kv.clone()));
    settings.update(|p| {
        p.by_name.insert("../x".into(), 1);
    });
}

#[test]
fn flush_waits_for_the_queued_saves() {
    use std::time::Duration;
    struct Slow(Arc<Mutex<BTreeMap<String, Value>>>);
    impl KeyValue for Slow {
        fn get(&self, _: &str) -> Result<Option<Value>, String> {
            Ok(None)
        }
        fn set(&self, key: &str, value: Value) -> Result<(), String> {
            std::thread::sleep(Duration::from_millis(150));
            self.0.lock().unwrap().insert(key.to_owned(), value);
            Ok(())
        }
        fn delete(&self, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn uid(&self) -> Option<u32> {
            Some(5)
        }
    }
    let map = Arc::new(Mutex::new(BTreeMap::new()));
    let factory_map = Arc::clone(&map);
    let factory: Factory = Arc::new(move || Box::new(Slow(Arc::clone(&factory_map))));
    let settings: Settings<Prefs> =
        Settings::open(&Slow(Arc::clone(&map)), "demo", Saver::Worker(factory));
    settings.update(|p| p.font = "Serif".into());
    assert!(map.lock().unwrap().is_empty());
    flush(Duration::from_secs(5));
    assert!(map.lock().unwrap().contains_key("user/5/demo/font"));
}
