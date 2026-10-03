//! Tests for the declared health, service-lifecycle and login topics
//! (issue #307): typed pub/sub helpers, concrete name construction,
//! malformed-input rejection and the declaration table.

use messenger_generated::os_lazy_healthd_v1 as healthd;
use messenger_generated::os_lazy_init_v1 as init;
use messenger_generated::os_lazy_logind_v1 as logind;
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

fn row() -> healthd::HealthRecord {
    healthd::HealthRecord {
        name: String::from("keyd"),
        status: String::from("ok"),
        detail: String::from("pid=4 restarts=0"),
        tick: 12,
    }
}

fn event() -> init::ServiceEvent {
    init::ServiceEvent {
        state: String::from("running"),
        pid: 4,
        restarts: 0,
        status: 0,
        health: String::from("system/health/keyd"),
        detail: String::new(),
    }
}

#[test]
fn health_publish_builds_a_concrete_name_and_roundtrips_the_payload() {
    let mut publisher = MockPublisher::default();
    assert_eq!(
        healthd::publish_system_health(&mut publisher, "keyd", &row()).unwrap(),
        1
    );
    assert_eq!(publisher.topic, "system/health/keyd");
    assert!(publisher.retained);
    assert_eq!(
        healthd::decode_system_health(&publisher.payload).unwrap(),
        row()
    );

    let mut publisher = MockPublisher::default();
    healthd::publish_system_health_summary(&mut publisher, &row()).unwrap();
    assert_eq!(publisher.topic, "system/health/summary");
    assert!(publisher.retained);
}

#[test]
fn health_publish_rejects_wildcards_slashes_and_empty_segments() {
    for bad in ["", "+", "#", "a/b", "a#b", "a+b", "caf\u{00e9}", "3/4"] {
        let mut publisher = MockPublisher::default();
        assert!(
            healthd::publish_system_health(&mut publisher, bad, &row()).is_err(),
            "publish accepted {bad:?}"
        );
        assert_eq!(publisher.calls, 0, "publish sent {bad:?} to the transport");
    }
    let mut publisher = MockPublisher::default();
    assert!(healthd::publish_system_health(&mut publisher, &"x".repeat(200), &row()).is_err());
    assert_eq!(publisher.calls, 0);
}

#[test]
fn health_subscribe_accepts_the_all_services_wildcard() {
    let mut subscriber = MockSubscriber::default();
    healthd::subscribe_system_health(&mut subscriber, "+").unwrap();
    assert_eq!(subscriber.filter, "system/health/+");
    assert_eq!(subscriber.qos, topics::QOS_LATEST);

    healthd::subscribe_system_health(&mut subscriber, "keyd").unwrap();
    assert_eq!(subscriber.filter, "system/health/keyd");
    assert!(healthd::subscribe_system_health(&mut subscriber, "#").is_err());
}

#[test]
fn service_event_publish_and_subscribe_use_the_generated_helpers() {
    let mut publisher = MockPublisher::default();
    init::publish_system_events_service(&mut publisher, "keyd", &event()).unwrap();
    assert_eq!(publisher.topic, "system/events/service/keyd");
    assert!(publisher.retained);
    assert_eq!(
        init::decode_system_events_service(&publisher.payload).unwrap(),
        event()
    );

    for bad in ["", "+", "#", "a/b", "a#b"] {
        let mut publisher = MockPublisher::default();
        assert!(init::publish_system_events_service(&mut publisher, bad, &event()).is_err());
        assert_eq!(publisher.calls, 0);
    }

    let mut subscriber = MockSubscriber::default();
    init::subscribe_system_events_service(&mut subscriber, "+").unwrap();
    assert_eq!(subscriber.filter, "system/events/service/+");
}

#[test]
fn login_session_publish_rejects_bad_ids_and_subscribe_takes_a_wildcard() {
    let session = logind::LoginSession {
        user: String::from("user"),
        uid: 1000,
        pid: 4,
        state: String::from("active"),
        home: String::from("/home/user"),
    };
    let mut publisher = MockPublisher::default();
    logind::publish_system_events_login_session(&mut publisher, "3", &session).unwrap();
    assert_eq!(publisher.topic, "system/events/login/session/3");
    assert_eq!(
        logind::decode_system_events_login_session(&publisher.payload).unwrap(),
        session
    );

    for bad in ["", "+", "#", "a/b", "1/2"] {
        let mut publisher = MockPublisher::default();
        assert!(
            logind::publish_system_events_login_session(&mut publisher, bad, &session).is_err()
        );
        assert_eq!(publisher.calls, 0);
    }

    let mut subscriber = MockSubscriber::default();
    logind::subscribe_system_events_login_session(&mut subscriber, "+").unwrap();
    assert_eq!(subscriber.filter, "system/events/login/session/+");
}

#[test]
fn declaration_table_has_the_expected_patterns_payloads_and_permissions() {
    let summary = declared_topic("system/health/summary").expect("summary topic");
    assert_eq!(summary.name, "system/health/summary");
    assert_eq!(summary.payload, "HealthRecord");
    assert!(summary.retained);

    let per_service = declared_topic("system/health/keyd").expect("health topic");
    assert_eq!(per_service.name, "system/health/+");
    assert_eq!(per_service.payload, "HealthRecord");

    assert!(per_service.retained);

    let service = declared_topic("system/events/service/keyd").expect("service topic");
    assert_eq!(service.name, "system/events/service/+");
    assert_eq!(service.payload, "ServiceEvent");
    assert!(service.retained);
    assert_eq!(
        service.publish_permission,
        "publish:system/events/service/+"
    );
    assert_eq!(
        service.subscribe_permission,
        "subscribe:system/events/service/+"
    );

    let start = declared_topic("system/events/login/start").expect("login start topic");
    assert_eq!(start.payload, "LoginStart");
    assert!(start.retained);
    assert_eq!(
        start.publish_permission,
        "publish:system/events/login/start"
    );

    let session = declared_topic("system/events/login/session/3").expect("login session topic");
    assert_eq!(session.name, "system/events/login/session/+");
    assert_eq!(session.payload, "LoginSession");
    assert!(session.retained);

    let denied = declared_topic("system/events/login/denied").expect("login denied topic");
    assert_eq!(denied.payload, "LoginDenied");
    assert!(denied.retained);

    let end = declared_topic("system/events/login/end").expect("login end topic");
    assert_eq!(end.payload, "LoginEnd");
    assert!(end.retained);
}

#[test]
fn malformed_declared_payloads_are_rejected_without_panicking() {
    let mut bodies: Vec<Vec<u8>> = vec![
        vec![],
        vec![0x00],
        vec![0xff, 0xff, 0xff, 0xff],
        vec![0x01, 0x02, 0x03],
    ];
    bodies.push(healthd::encode_system_health(&row()).unwrap());
    bodies.push(init::encode_system_events_service(&event()).unwrap());
    for body in &mut bodies {
        let truncated = body.len().saturating_sub(2);
        body.truncate(truncated);
    }
    for body in &bodies {
        // Any of these may succeed on a prefix, but none may panic.
        let _ = healthd::decode_system_health(body);
        let _ = healthd::decode_system_health_summary(body);
        let _ = init::decode_system_events_service(body);
        let _ = logind::decode_system_events_login_start(body);
        let _ = logind::decode_system_events_login_session(body);
        let _ = logind::decode_system_events_login_denied(body);
        let _ = logind::decode_system_events_login_end(body);
    }
}
