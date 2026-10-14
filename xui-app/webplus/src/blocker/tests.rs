//! Settings, the decision per navigation and the site names.
use super::*;

#[test]
fn a_site_is_the_host_without_www_port_and_credentials() {
    for (url, site) in [
        ("https://www.Example.COM/a?b#c", "example.com"),
        ("http://news.example.com:8080/", "news.example.com"),
        ("https://user:pw@example.com/x", "example.com"),
        ("HTTP://example.com", "example.com"),
        ("https://192.168.0.1/", "192.168.0.1"),
        ("https://example.com./", "example.com"),
        ("https://ads_.example.com/", "ads_.example.com"),
    ] {
        assert_eq!(site_of(url).as_deref(), Some(site), "{url}");
    }
    for url in [
        "about:blank",
        "data:text/html,<p>",
        "file:///etc/passwd",
        "mailto:a@b.c",
        "https://",
        "https://[::1]/",
        "https://exa mple.com/",
        "https://-bad.example/",
        "https://a..b/",
        "ftp://example.com/",
    ] {
        assert_eq!(site_of(url), None, "{url}");
    }
}

/// A store that answers with a fixed set of settings and remembers the last save.
struct Stored(Settings, std::sync::Mutex<Option<Settings>>);

impl Persist for Stored {
    fn load(&self) -> Settings {
        self.0.clone()
    }
    fn save(&self, settings: &Settings) {
        *self.1.lock().unwrap() = Some(settings.clone());
    }
}

#[test]
fn a_store_that_arrives_late_replaces_the_defaults_unless_the_user_changed_something() {
    let off = Settings {
        enabled: false,
        ..Settings::default()
    };
    let late = Arc::new(Stored(off.clone(), std::sync::Mutex::new(None)));
    let blocker = Blocker::volatile();
    assert!(blocker.settings().enabled, "on until the store says");
    blocker.attach(late.clone());
    assert!(!blocker.settings().enabled);
    blocker.set_enabled(true);
    assert_eq!(late.1.lock().unwrap().as_ref().map(|s| s.enabled), Some(true));

    // A switch the user flipped before the store came keeps their choice, and is saved there from now on.
    let blocker = Blocker::volatile();
    blocker.set_enabled(true);
    blocker.set_site_excepted("example.com", true);
    let late = Arc::new(Stored(off, std::sync::Mutex::new(None)));
    blocker.attach(late.clone());
    assert!(blocker.settings().enabled);
    assert!(blocker.site_excepted("example.com"));
    blocker.set_enabled(false);
    assert!(late.1.lock().unwrap().is_some());
}

#[test]
fn attaching_a_store_wakes_the_window() {
    let blocker = Blocker::volatile();
    let woken = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let counter = Arc::clone(&woken);
    blocker.on_change(move || {
        counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    });
    blocker.attach(Arc::new(Volatile));
    assert_eq!(woken.load(std::sync::atomic::Ordering::Relaxed), 1);
}

#[test]
fn an_exception_covers_the_site_and_its_subdomains_only() {
    assert!(covers("example.com", "example.com"));
    assert!(covers("example.com", "news.example.com"));
    assert!(covers("example.com", "a.b.example.com"));
    assert!(!covers("example.com", "notexample.com"));
    assert!(!covers("example.com", "example.com.evil.net"));
    assert!(!covers("news.example.com", "example.com"));
}

#[test]
fn the_decision_follows_the_switch_the_exceptions_and_the_one_shot() {
    let blocker = Blocker::volatile();
    assert!(blocker.decide("https://news.example.com/"));
    assert!(
        !blocker.decide("about:history"),
        "our own pages are not blocked"
    );
    assert!(!blocker.decide("data:text/html,x"));
    // A host the site list cannot name is still blocked: an address must not switch it off.
    for url in [
        "https://[::1]/",
        "http://[2001:db8::1]:8080/x",
        "https://exa mple.com/",
    ] {
        assert_eq!(site_of(url), None, "{url}");
        assert!(blocker.decide(url), "{url}");
    }

    assert_eq!(
        blocker.set_site_excepted("WWW.Example.com", true),
        Some("example.com".to_owned())
    );
    assert!(!blocker.decide("https://news.example.com/"));
    assert!(!blocker.decide("https://example.com/"));
    assert!(blocker.decide("https://other.org/"));

    blocker.set_site_excepted("example.com", false);
    assert!(blocker.decide("https://news.example.com/"));

    blocker.skip_once("other.org");
    assert!(!blocker.decide("https://other.org/x"));
    assert!(blocker.decide("https://example.com/"));
    blocker.leave_unless(Some("example.com"));
    assert!(
        blocker.decide("https://other.org/x"),
        "the one-shot ended with the visit"
    );

    blocker.set_enabled(false);
    assert!(!blocker.decide("https://example.com/"));
    assert_eq!(blocker.set_site_excepted("not a site!", true), None);
}

#[test]
fn ending_an_exception_ends_the_one_that_covers_the_site() {
    let blocker = Blocker::volatile();
    blocker.set_site_excepted("example.com", true);
    blocker.set_site_excepted("news.example.com", false);
    assert!(blocker.decide("https://news.example.com/"));
    assert!(blocker.settings().exceptions.is_empty());
}

#[test]
fn settings_read_back_only_what_is_valid() {
    let s = Settings::from_texts(
        Some("false"),
        "example.com  www.Other.org not/a/site -x.y bad..host",
        "easylist nosuchlist",
    );
    assert!(!s.enabled);
    assert_eq!(s.exceptions_text(), "example.com other.org");
    assert_eq!(s.lists_off_text(), "easylist");
    assert!(Settings::from_texts(None, "", "").enabled);
    assert!(Settings::from_texts(Some("garbage"), "", "").enabled);
}

struct Recording(Mutex<Vec<Settings>>);
impl Persist for Recording {
    fn load(&self) -> Settings {
        Settings::from_texts(None, "kept.example", "")
    }
    fn save(&self, settings: &Settings) {
        self.0.lock().unwrap().push(settings.clone());
    }
}

#[test]
fn every_change_is_saved_and_the_saved_settings_load_again() {
    let store = Arc::new(Recording(Mutex::new(Vec::new())));
    let blocker = Blocker::new(store.clone());
    assert!(!blocker.decide("https://kept.example/"));
    blocker.set_enabled(false);
    blocker.set_site_excepted("a.example", true);
    blocker.set_list_enabled("easylist", false);
    let saved = store.0.lock().unwrap();
    assert_eq!(saved.len(), 3);
    let last = saved.last().unwrap();
    assert!(!last.enabled && last.lists_off.contains("easylist"));
    assert_eq!(last.exceptions_text(), "a.example kept.example");
    let again = Settings::from_texts(
        Some(if last.enabled { "true" } else { "false" }),
        &last.exceptions_text(),
        &last.lists_off_text(),
    );
    assert_eq!(&again, last);
}

#[test]
fn a_list_switch_reaches_the_lists_table() {
    let blocker = Blocker::volatile();
    assert!(blocker.lists()[0].enabled);
    blocker.set_list_enabled("easylist", false);
    assert!(!blocker.lists()[0].enabled);
    blocker.set_list_enabled("nosuchlist", false);
    assert_eq!(blocker.settings().lists_off.len(), 1);
}
