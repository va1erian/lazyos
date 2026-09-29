//! Round-trip tests for the generated `os.lazy.init.v1` stubs (issue #301).

use messenger_generated::os_lazy_init_v1::*;

fn status(i: u64) -> ServiceStatus {
    ServiceStatus {
        name: format!("svc{i}"),
        state: if i % 2 == 0 { "running" } else { "restarting" }.into(),
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
        verbs: if i % 2 == 0 {
            vec!["open".into(), "edit".into()]
        } else {
            Vec::new()
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
