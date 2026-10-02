//! Round-trip tests for the generated `os.lazy.init.v1` stubs (issue #301).

use messenger_generated::os_lazy_init_v1::*;

fn status(i: u64) -> ServiceStatus {
    ServiceStatus {
        name: format!("svc{i}"),
        state: if i.is_multiple_of(2) {
            "running"
        } else {
            "restarting"
        }
        .into(),
        pid: 100 + i,
        restarts: i,
        deps: if i == 0 {
            String::new()
        } else {
            format!("svc{}", i - 1)
        },
        health: "ok".into(),
    }
}

fn app(i: u64) -> AppInfo {
    AppInfo {
        id: format!("app{i}"),
        name: format!("App {i}"),
        path: format!("APP{i}.ELF"),
        restart: "always".into(),
        verbs: if i.is_multiple_of(2) {
            vec!["open".into(), "edit".into()]
        } else {
            Vec::new()
        },
        installed: i.is_multiple_of(3),
        origin: if i.is_multiple_of(3) { "core" } else { "system" }.into(),
        category: "utilities".into(),
        hidden: i.is_multiple_of(5),
        autostart: i == 1,
        icon: if i.is_multiple_of(3) {
            format!("app{i}/0.1.0-0000000{}/icons/app-32.png", i % 10)
        } else {
            String::new()
        },
    }
}

#[test]
fn services_reply_roundtrips_empty_and_many() {
    for count in [0usize, 1, 80] {
        let reply = ServicesReply {
            services: (0..count as u64).map(status).collect(),
        };
        let body = encode_services_reply(&reply).unwrap();
        assert_eq!(decode_services_reply(&body).unwrap(), reply);
    }
}

#[test]
fn launch_args_and_reply_roundtrip() {
    let args = LaunchArgs {
        app: "editor".into(),
        args: "--new".into(),
        session: 7,
    };
    assert_eq!(
        decode_launch_args(&encode_launch_args(&args).unwrap()).unwrap(),
        args
    );
    let reply = LaunchReply {
        app: "editor".into(),
        pid: 42,
        session: 7,
    };
    assert_eq!(
        decode_launch_reply(&encode_launch_reply(&reply).unwrap()).unwrap(),
        reply
    );
}

#[test]
fn empty_launch_args_roundtrip() {
    let args = LaunchArgs::default();
    assert_eq!(
        decode_launch_args(&encode_launch_args(&args).unwrap()).unwrap(),
        args
    );
}

#[test]
fn list_apps_roundtrips_empty_and_many() {
    for count in [0usize, 1, 80] {
        let reply = ListAppsReply {
            apps: (0..count as u64).map(app).collect(),
        };
        let body = encode_list_apps_reply(&reply).unwrap();
        assert_eq!(decode_list_apps_reply(&body).unwrap(), reply);
    }
}

#[test]
fn truncated_bodies_are_rejected() {
    let services = ServicesReply {
        services: vec![status(1)],
    };
    let body = encode_services_reply(&services).unwrap();
    assert!(decode_services_reply(&body[..body.len() - 3]).is_err());

    let apps = ListAppsReply { apps: vec![app(1)] };
    let body = encode_list_apps_reply(&apps).unwrap();
    assert!(decode_list_apps_reply(&body[..body.len() - 3]).is_err());
}

#[test]
fn unknown_trailing_fields_are_ignored() {
    let mut body = libmessenger::Encoder::new();
    body.string(1, "svc").unwrap();
    body.string(2, "running").unwrap();
    body.u64(3, 5).unwrap();
    body.u64(4, 1).unwrap();
    body.string(5, "dep").unwrap();
    body.string(6, "ok").unwrap();
    body.u64(99, 123).unwrap();
    let decoded = decode_service_status(&body.finish()).unwrap();
    assert_eq!(decoded.name, "svc");
    assert_eq!(decoded.state, "running");
    assert_eq!(decoded.pid, 5);
}

#[test]
fn error_field_is_ignored_by_decoders() {
    // A service failure carries only the structured error field (id 15);
    // a decoder must yield defaults rather than fail on it.
    let mut body = libmessenger::Encoder::new();
    body.error(15, 22, "bad").unwrap();
    assert_eq!(
        decode_services_reply(&body.finish()).unwrap(),
        ServicesReply::default()
    );
}

fn event(i: u64) -> ServiceEvent {
    ServiceEvent {
        state: if i.is_multiple_of(2) {
            "running"
        } else {
            "restarting"
        }
        .into(),
        pid: 100 + i,
        restarts: i,
        status: i * 3,
        health: format!("system/health/svc{i}"),
        detail: if i == 0 {
            String::new()
        } else {
            format!("attempt={i}")
        },
    }
}

#[test]
fn service_event_roundtrips_empty_and_many() {
    for (state, detail) in [("running", ""), ("failed", "restart budget exhausted")] {
        let event = ServiceEvent {
            state: state.into(),
            pid: 7,
            restarts: 2,
            status: 3,
            health: "system/health/svc".into(),
            detail: detail.into(),
        };
        let body = encode_system_events_service(&event).unwrap();
        assert_eq!(decode_system_events_service(&body).unwrap(), event);
    }
    let event = event(1);
    let body = encode_system_events_service(&event).unwrap();
    assert_eq!(decode_system_events_service(&body).unwrap(), event);
}

#[test]
fn service_event_rejects_truncated_and_garbage_bodies() {
    let body = encode_system_events_service(&event(1)).unwrap();
    assert!(decode_system_events_service(&body[..body.len() - 2]).is_err());
    assert!(decode_system_events_service(&[0xff, 0xff, 0xff, 0xff]).is_err());
    assert!(decode_system_events_service(&[0x01]).is_err());
}

#[test]
fn service_event_topic_pattern_and_qos_are_declared() {
    assert_eq!(TOPIC_SYSTEM_EVENTS_SERVICE, "system/events/service/+");
    assert_eq!(
        TOPIC_SYSTEM_EVENTS_SERVICE_QOS,
        messenger_generated::topics::QOS_LATEST
    );
}

#[test]
fn stop_args_and_reply_roundtrip() {
    let args = StopArgs {
        app: "org.lazy.counter".into(),
    };
    assert_eq!(
        decode_stop_args(&encode_stop_args(&args).unwrap()).unwrap(),
        args
    );
    let reply = StopReply { stopped: 3 };
    assert_eq!(
        decode_stop_reply(&encode_stop_reply(&reply).unwrap()).unwrap(),
        reply
    );
    assert_eq!(decode_stop_args(&[]).unwrap(), StopArgs::default());
}
