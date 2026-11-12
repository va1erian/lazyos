//! Host tests of the typed settings store.

use super::*;
use serde::Deserialize;

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
    assert_eq!(kv.get("user/1000/demo/window/width"), Some(Value::I64(640)));
    assert_eq!(
        kv.get("user/1000/demo/font"),
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
        kv.get("user/7/demo/font"),
        Some(Value::Str("outside".into()))
    );
}

#[test]
fn unsetting_an_option_deletes_its_key() {
    let kv = Rc::new(MemKv::new(7));
    let settings = open(&kv);
    settings.update(|p| p.printer = Some("lp".into()));
    assert!(kv.get("user/7/demo/printer").is_some());
    settings.update(|p| p.printer = None);
    assert!(kv.get("user/7/demo/printer").is_none());
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
fn the_worker_saves_and_rings_the_bell() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    struct Shared(Arc<Mutex<BTreeMap<String, Value>>>);
    impl KeyValue for Shared {
        fn get(&self, key: &str) -> Option<Value> {
            self.0.lock().unwrap().get(key).cloned()
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
    let rung = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&rung);
    settings.on_saved(Sink::from_fn(move |()| {
        counter.fetch_add(1, Ordering::SeqCst);
        true
    }));
    settings.update(|p| p.font = "Serif".into());
    let started = Instant::now();
    while rung.load(Ordering::SeqCst) == 0 && started.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(rung.load(Ordering::SeqCst), 1);
    assert_eq!(
        map.lock().unwrap().get("user/5/demo/font"),
        Some(&Value::Str("Serif".into()))
    );
}
