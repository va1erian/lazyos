//! Round-trip tests for the notification stubs (docs/notify-plan.md):
//! `os.lazy.notify.v1` and its events.

use messenger_generated::os_lazy_notify_events_v1 as events;
use messenger_generated::os_lazy_notify_v1::*;

fn image(side: u32) -> Image {
    Image {
        width: side,
        height: side,
        data: vec![0x33; (side * side * 4) as usize],
    }
}

fn actions() -> Vec<Action> {
    vec![
        Action {
            id: 1,
            label: "Restart".into(),
        },
        Action {
            id: 2,
            label: "Close".into(),
        },
    ]
}

#[test]
fn notify_roundtrips_every_optional_part() {
    let bare = NotifyArgs {
        level: LEVEL_INFO,
        title: "Stick mounted".into(),
        ..NotifyArgs::default()
    };
    let full = NotifyArgs {
        level: LEVEL_ERROR,
        title: "Copy failed".into(),
        body: "No space left on /home".into(),
        icon: Some(Icon {
            lucide: Some("hard-drive".into()),
            image: None,
        }),
        actions: actions(),
        progress: Some(40),
        replaces_id: Some(u64::MAX),
        timeout: Some(0),
    };
    let with_image = NotifyArgs {
        icon: Some(Icon {
            lucide: None,
            image: Some(image(16)),
        }),
        ..full.clone()
    };
    for args in [bare, full, with_image] {
        let body = encode_notify_args(&args).unwrap();
        assert_eq!(decode_notify_args(&body).unwrap(), args);
    }
    let reply = NotifyReply { id: 7 };
    let body = encode_notify_reply(&reply).unwrap();
    assert_eq!(decode_notify_reply(&body).unwrap(), reply);
}

#[test]
fn absent_options_stay_absent() {
    let args = NotifyArgs {
        title: "x".into(),
        progress: None,
        replaces_id: None,
        timeout: None,
        ..NotifyArgs::default()
    };
    let back = decode_notify_args(&encode_notify_args(&args).unwrap()).unwrap();
    assert_eq!(back.progress, None);
    assert_eq!(back.replaces_id, None);
    assert_eq!(back.timeout, None);
    // `Some(0)` is not absent: it means "until dismissed".
    let sticky = NotifyArgs {
        timeout: Some(0),
        progress: Some(0),
        ..args
    };
    let back = decode_notify_args(&encode_notify_args(&sticky).unwrap()).unwrap();
    assert_eq!(back.timeout, Some(0));
    assert_eq!(back.progress, Some(0));
}

#[test]
fn truncated_bodies_are_refused() {
    let args = NotifyArgs {
        title: "Copying".into(),
        body: "photos".into(),
        icon: Some(Icon {
            lucide: None,
            image: Some(image(4)),
        }),
        actions: actions(),
        progress: Some(10),
        ..NotifyArgs::default()
    };
    let body = encode_notify_args(&args).unwrap();
    for cut in 1..body.len() {
        // Never a panic; a cut inside a field header or payload is an error.
        let _ = decode_notify_args(&body[..cut]);
    }
    assert!(decode_notify_args(&body[..body.len() - 1]).is_err());
}

#[test]
fn close_and_listen_roundtrip() {
    let close = CloseArgs { id: 99 };
    let body = encode_close_args(&close).unwrap();
    assert_eq!(decode_close_args(&body).unwrap(), close);

    let (body, objects) = encode_listen_args(&ListenArgs { events: 5 }).unwrap();
    assert_eq!(objects, vec![libmessenger::Object::Channel(5)]);
    let back = decode_listen_args(&body, &[libmessenger::Object::Channel(12)]).unwrap();
    assert_eq!(back.events, 12);
    assert_eq!(LISTEN_OBJECTS, &[libmessenger::ObjectKind::Channel]);
    // A request with no channel is refused by the declared object count.
    assert!(decode_listen_args(&body, &[]).is_err());
}

#[test]
fn events_roundtrip() {
    let activated = events::ActivatedArgs { id: 3 };
    let body = events::encode_activated_args(&activated).unwrap();
    assert_eq!(events::decode_activated_args(&body).unwrap(), activated);
    let invoked = events::ActionInvokedArgs { id: 3, action: 2 };
    let body = events::encode_action_invoked_args(&invoked).unwrap();
    assert_eq!(events::decode_action_invoked_args(&body).unwrap(), invoked);
    let closed = events::ClosedArgs {
        id: u64::MAX,
        reason: CLOSE_REASON_MUTED,
    };
    let body = events::encode_closed_args(&closed).unwrap();
    assert_eq!(events::decode_closed_args(&body).unwrap(), closed);
    assert_ne!(events::INTERFACE_ID, INTERFACE_ID);
}

#[test]
fn level_and_reason_values_are_the_wire_values() {
    assert_eq!(
        [LEVEL_INFO, LEVEL_SUCCESS, LEVEL_WARN, LEVEL_ERROR],
        [0, 1, 2, 3]
    );
    assert_eq!(
        [
            CLOSE_REASON_USER,
            CLOSE_REASON_TIMEOUT,
            CLOSE_REASON_REPLACED,
            CLOSE_REASON_SENDER,
            CLOSE_REASON_RESTART,
            CLOSE_REASON_MUTED,
        ],
        [0, 1, 2, 3, 4, 5]
    );
}
