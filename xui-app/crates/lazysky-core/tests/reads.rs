//! Profiles, notifications, the unread count, and settings saved by the
//! core.

mod common;


use common::*;
use lazysky_core::{Event, Request, Settings};
use serde_json::json;

#[test]
fn profile_notifications_unread_and_seen() {
    let server = pds(|seen| {
        if seen.path.contains("getProfile") {
            return Some((
                200,
                json!({"did": "did:plc:a", "handle": "a.test", "displayName": "Alice",
                       "followersCount": 12, "followsCount": 3, "postsCount": 40})
                .to_string(),
            ));
        }
        if seen.path.contains("listNotifications") {
            return Some((
                200,
                json!({"cursor": "n2", "notifications": [
                    {"uri": "at://did:plc:b/app.bsky.feed.like/1", "cid": "c",
                     "author": {"did": "did:plc:b", "handle": "b.test", "displayName": "Bob"},
                     "reason": "like", "record": {}, "isRead": false}]})
                .to_string(),
            ));
        }
        if seen.path.contains("getUnreadCount") {
            return Some((200, json!({"count": 4}).to_string()));
        }
        if seen.path.contains("updateSeen") {
            return Some((200, "{}".to_string()));
        }
        None
    });
    let rig = rig(&server);
    rig.core.send(Request::Profile {
        seq: 2,
        actor: "a.test".into(),
    });
    let Event::Profile { profile, .. } = rig.next() else {
        panic!("no profile")
    };
    assert_eq!(
        (profile.followers, profile.follows, profile.posts),
        (12, 3, 40)
    );
    assert!(server.to("getProfile")[0].path.contains("actor=a.test"));

    rig.core.send(Request::Notifications {
        seq: 3,
        cursor: None,
    });
    match rig.next() {
        Event::Notifications { items, cursor, .. } => {
            assert_eq!(items[0].summary(), "Bob liked your post");
            assert_eq!(cursor.as_deref(), Some("n2"));
        }
        other => panic!("{other:?}"),
    }
    rig.core.send(Request::UnreadCount { seq: 4 });
    assert!(matches!(rig.next(), Event::Unread { seq: 4, count: 4 }));
    rig.core.send(Request::MarkSeen { seq: 5 });
    assert!(matches!(rig.next(), Event::Done { seq: 5 }));
    let seen = server.to("updateSeen")[0].json();
    assert!(seen["seenAt"].as_str().unwrap().ends_with('Z'));
}

#[test]
fn settings_are_saved_by_the_core_and_the_service_can_change() {
    let server = pds(|_| None);
    let rig = rig(&server);
    let dir = std::env::temp_dir().join(format!("lazysky-save-{}", std::process::id()));
    // The platform's app dir is the temp dir; the file lands there.
    let settings = Settings {
        theme: lazysky_core::ThemeChoice::Dark,
        max_image_height: 250,
        ..Settings::default()
    };
    let _ = dir;
    rig.core.send(Request::SaveSettings {
        seq: 2,
        settings: settings.clone(),
    });
    assert!(matches!(rig.next(), Event::Done { seq: 2 }));
    assert_eq!(Settings::load(&std::env::temp_dir()).max_image_height, 250);
    assert!(rig
        .log
        .0
        .lock()
        .unwrap()
        .iter()
        .any(|l| l.starts_with("SKY:SETTINGS:SAVED:")));
    let _ = std::fs::remove_file(std::env::temp_dir().join("settings.toml"));

    rig.core.send(Request::Configure {
        seq: 3,
        service: "http://127.0.0.1:1/".into(),
    });
    assert!(matches!(rig.next(), Event::Done { seq: 3 }));
}
