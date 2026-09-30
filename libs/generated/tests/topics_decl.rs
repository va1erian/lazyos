//! Tests for declared topics (issue #307): typed pub/sub helpers, concrete
//! name construction, malformed-input rejection and the declaration table.

use messenger_generated::os_lazy_clipboard_v1 as clipboard;
use messenger_generated::os_lazy_confd_v1 as confd;
use messenger_generated::os_lazy_mimed_v1 as mimed;
use messenger_generated::os_lazy_sysmond_v1 as sysmond;
use messenger_generated::{declared_topic, topics};

/// A publishing transport that records the call instead of sending it.
#[derive(Default)]
struct MockPublisher {
    topic: String,
    payload: Vec<u8>,
    retained: bool,
    calls: usize,
}

/// The mock transports' error: the real `user::Error` would pull a syscall
/// layer into this host test.
#[derive(Debug, PartialEq)]
struct MockError(&'static str);

impl From<topics::TopicError> for MockError {
    fn from(error: topics::TopicError) -> Self {
        match error {
            topics::TopicError::BadSegment => MockError("bad segment"),
            topics::TopicError::MissingSegment => MockError("missing segment"),
            topics::TopicError::ExtraSegment => MockError("extra segment"),
            topics::TopicError::TooLong => MockError("too long"),
            topics::TopicError::Encode(_) => MockError("encode"),
        }
    }
}

impl topics::Publish for MockPublisher {
    type Error = MockError;

    fn publish_topic(
        &mut self,
        topic: &str,
        payload: &[u8],
        retained: bool,
    ) -> Result<u64, Self::Error> {
        self.topic = String::from(topic);
        self.payload = payload.to_vec();
        self.retained = retained;
        self.calls += 1;
        Ok(1)
    }
}

/// A subscribing transport that records the filter instead of sending it.
#[derive(Default)]
struct MockSubscriber {
    filter: String,
    qos: u32,
}

impl topics::Subscribe for MockSubscriber {
    type Error = MockError;
    type Subscription = u32;

    fn subscribe_topic(&mut self, filter: &str, qos: u32) -> Result<u32, Self::Error> {
        self.filter = String::from(filter);
        self.qos = qos;
        Ok(7)
    }
}

fn meta() -> clipboard::OfferMeta {
    clipboard::OfferMeta {
        token: 5,
        owner: String::from("clipcopy"),
        session: 3,
        mimes: vec![String::from("text/plain")],
        lazy: false,
        tick: 9,
    }
}

#[test]
fn clipboard_publish_builds_a_concrete_name_and_roundtrips_the_payload() {
    let mut publisher = MockPublisher::default();
    assert_eq!(
        clipboard::publish_session_clipboard_changed(&mut publisher, "3", &meta()).unwrap(),
        1
    );
    assert_eq!(publisher.topic, "session/3/clipboard/changed");
    assert!(publisher.retained);
    assert_eq!(
        clipboard::decode_session_clipboard_changed(&publisher.payload).unwrap(),
        meta()
    );
}

#[test]
fn clipboard_publish_rejects_wildcards_slashes_and_empty_segments() {
    for bad in ["+", "#", "a/b", "", "a#b", "3/4"] {
        let mut publisher = MockPublisher::default();
        assert!(
            clipboard::publish_session_clipboard_changed(&mut publisher, bad, &meta()).is_err(),
            "publish accepted {bad:?}"
        );
        assert_eq!(publisher.calls, 0, "publish sent {bad:?} to the transport");
    }
}

#[test]
fn clipboard_subscribe_accepts_wildcards_and_narrow_concrete_sessions() {
    let mut subscriber = MockSubscriber::default();
    assert_eq!(
        clipboard::subscribe_session_clipboard_changed(&mut subscriber, "+").unwrap(),
        7
    );
    assert_eq!(subscriber.filter, "session/+/clipboard/changed");
    assert_eq!(subscriber.qos, topics::QOS_LATEST);

    clipboard::subscribe_session_clipboard_changed(&mut subscriber, "42").unwrap();
    assert_eq!(subscriber.filter, "session/42/clipboard/changed");

    // `#` cannot sit in the middle of a pattern.
    assert!(clipboard::subscribe_session_clipboard_changed(&mut subscriber, "#").is_err());
}

#[test]
fn confd_tail_placeholder_joins_literal_segments() {
    let change = confd::Change {
        path: String::from("sys/net/mtu"),
        deleted: false,
    };
    let mut publisher = MockPublisher::default();
    confd::publish_system_confd_changed(&mut publisher, "sys/net/mtu", &change).unwrap();
    assert_eq!(publisher.topic, "system/confd/changed/sys/net/mtu");
    assert!(!publisher.retained);
    assert_eq!(
        confd::decode_system_confd_changed(&publisher.payload).unwrap(),
        change
    );
}

#[test]
fn confd_publish_rejects_wildcards_empty_and_malformed_tails() {
    let change = confd::Change {
        path: String::from("sys/x"),
        deleted: true,
    };
    for bad in ["", "sys//x", "sys/+/x", "sys/x/#", "#", "sys/x/"] {
        let mut publisher = MockPublisher::default();
        assert!(
            confd::publish_system_confd_changed(&mut publisher, bad, &change).is_err(),
            "publish accepted {bad:?}"
        );
        assert_eq!(publisher.calls, 0, "publish sent {bad:?} to the transport");
    }
}

#[test]
fn confd_subscribe_accepts_tail_wildcards() {
    let mut subscriber = MockSubscriber::default();
    confd::subscribe_system_confd_changed(&mut subscriber, "sys/#").unwrap();
    assert_eq!(subscriber.filter, "system/confd/changed/sys/#");

    confd::subscribe_system_confd_changed(&mut subscriber, "#").unwrap();
    assert_eq!(subscriber.filter, "system/confd/changed/#");

    confd::subscribe_system_confd_changed(&mut subscriber, "sys/time/zone").unwrap();
    assert_eq!(subscriber.filter, "system/confd/changed/sys/time/zone");
}

#[test]
fn declaration_table_matches_concrete_topics_and_derives_permissions() {
    let changed = declared_topic("session/7/clipboard/changed").expect("clipboard topic");
    assert_eq!(changed.interface, "os.lazy.clipboard.v1");
    assert_eq!(changed.payload, "OfferMeta");
    assert!(changed.retained);
    assert_eq!(changed.qos, topics::QOS_LATEST);
    assert_eq!(
        changed.publish_permission,
        "publish:session/+/clipboard/changed"
    );
    assert_eq!(
        changed.subscribe_permission,
        "subscribe:session/+/clipboard/changed"
    );

    let change = declared_topic("system/confd/changed/sys/time/zone").expect("confd topic");
    assert_eq!(change.payload, "Change");
    assert!(!change.retained);
    assert_eq!(change.publish_permission, "publish:system/confd/changed/#");
    assert_eq!(
        change.subscribe_permission,
        "subscribe:system/confd/changed/#"
    );

    let tick = declared_topic("time/tick").expect("tick topic");
    assert_eq!(tick.payload, "Tick");
    assert!(tick.retained);

    assert!(declared_topic("not/a/topic").is_none());
}

#[test]
fn oversized_and_non_ascii_names_are_rejected() {
    let change = confd::Change {
        path: String::from("sys/x"),
        deleted: false,
    };
    for bad in [
        "a".repeat(200),
        "a/b/c/d/e/f/g/h/i".to_string(),
        "caf\u{00e9}".to_string(),
    ] {
        let mut publisher = MockPublisher::default();
        assert!(
            confd::publish_system_confd_changed(&mut publisher, &bad, &change).is_err(),
            "publish accepted a malformed tail"
        );
        assert_eq!(publisher.calls, 0);
    }
}

#[test]
fn malformed_payloads_are_rejected() {
    let body = confd::encode_system_confd_changed(&confd::Change {
        path: String::from("sys/a"),
        deleted: false,
    })
    .unwrap();
    assert!(confd::decode_system_confd_changed(&body[..body.len() - 2]).is_err());
    assert!(confd::decode_system_confd_changed(&[0xff, 0xff, 0xff, 0xff]).is_err());
    assert!(confd::decode_system_confd_changed(&[0x01]).is_err());
}

fn open_event() -> mimed::OpenEvent {
    mimed::OpenEvent {
        path: String::from("NOTES.TXT"),
        mime: String::from("text/plain"),
        verb: String::from("open"),
    }
}

#[test]
fn open_event_publish_builds_a_concrete_app_topic_and_roundtrips() {
    let mut publisher = MockPublisher::default();
    mimed::publish_system_events_open(&mut publisher, "editor", &open_event()).unwrap();
    assert_eq!(publisher.topic, "system/events/open/editor");
    assert!(!publisher.retained);
    assert_eq!(
        mimed::decode_system_events_open(&publisher.payload).unwrap(),
        open_event()
    );
}

#[test]
fn open_event_publish_rejects_wildcards_slashes_and_empty_segments() {
    for bad in ["", "+", "#", "a/b", "a#b", "editor/2", "caf\u{00e9}"] {
        let mut publisher = MockPublisher::default();
        assert!(
            mimed::publish_system_events_open(&mut publisher, bad, &open_event()).is_err(),
            "publish accepted {bad:?}"
        );
        assert_eq!(publisher.calls, 0, "publish sent {bad:?} to the transport");
    }
}

#[test]
fn stats_topics_are_retained_literal_names() {
    let memory = sysmond::MemoryStats {
        ticks: 1,
        frames_total: 2,
        frames_live: 3,
        frames_free: 4,
        slab_live: 5,
        slab_peak: 6,
        heap_used: 7,
        heap_total: 8,
    };
    let mut publisher = MockPublisher::default();
    sysmond::publish_system_stats_memory(&mut publisher, &memory).unwrap();
    assert_eq!(publisher.topic, "system/stats/memory");
    assert!(publisher.retained);
    assert_eq!(
        sysmond::decode_system_stats_memory(&publisher.payload).unwrap(),
        memory
    );

    let tasks = sysmond::TasksStats {
        live: 1,
        tasks: vec![sysmond::TaskRow {
            pid: 9,
            ppid: 0,
            state: String::from("run"),
            wait: String::new(),
            class: String::from("norm"),
            cpu: 12,
            name: String::from("init"),
        }],
    };
    let mut publisher = MockPublisher::default();
    sysmond::publish_system_stats_tasks(&mut publisher, &tasks).unwrap();
    assert_eq!(publisher.topic, "system/stats/tasks");
    assert!(publisher.retained);
    assert_eq!(
        sysmond::decode_system_stats_tasks(&publisher.payload).unwrap(),
        tasks
    );
}

#[test]
fn clipboard_audit_topics_are_unretained_literal_names() {
    let paste = clipboard::PasteEvent {
        seq: 3,
        uid: 1000,
        session: 2,
        mime: String::from("text/plain"),
        app: 5,
        bytes: 11,
        lazy: true,
    };
    let mut publisher = MockPublisher::default();
    clipboard::publish_system_events_clipboard_paste(&mut publisher, &paste).unwrap();
    assert_eq!(publisher.topic, "system/events/clipboard/paste");
    assert!(!publisher.retained);
    assert_eq!(
        clipboard::decode_system_events_clipboard_paste(&publisher.payload).unwrap(),
        paste
    );

    let denial = clipboard::ClipboardDenial {
        seq: 1,
        uid: 1000,
        session: 2,
        mime: String::from("text/plain"),
        app: 5,
        token: 42,
    };
    let mut publisher = MockPublisher::default();
    clipboard::publish_system_events_security_clipboard(&mut publisher, &denial).unwrap();
    assert_eq!(publisher.topic, "system/events/security/clipboard");
    assert!(!publisher.retained);
    assert_eq!(
        clipboard::decode_system_events_security_clipboard(&publisher.payload).unwrap(),
        denial
    );
}

#[test]
fn new_topics_appear_in_the_declaration_table() {
    let memory = declared_topic("system/stats/memory").expect("memory topic");
    assert_eq!(memory.interface, "os.lazy.sysmond.v1");
    assert_eq!(memory.payload, "MemoryStats");
    assert!(memory.retained);
    assert_eq!(memory.publish_permission, "publish:system/stats/memory");

    let tasks = declared_topic("system/stats/tasks").expect("tasks topic");
    assert_eq!(tasks.payload, "TasksStats");
    assert!(tasks.retained);

    let open = declared_topic("system/events/open/editor").expect("open topic");
    assert_eq!(open.interface, "os.lazy.mimed.v1");
    assert_eq!(open.payload, "OpenEvent");
    assert!(!open.retained);
    assert_eq!(open.publish_permission, "publish:system/events/open/+");
    assert_eq!(open.subscribe_permission, "subscribe:system/events/open/+");

    let paste = declared_topic("system/events/clipboard/paste").expect("paste topic");
    assert_eq!(paste.payload, "PasteEvent");
    assert!(!paste.retained);

    let denial = declared_topic("system/events/security/clipboard").expect("denial topic");
    assert_eq!(denial.payload, "ClipboardDenial");
    assert!(!denial.retained);
}
