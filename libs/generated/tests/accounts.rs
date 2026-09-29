//! Round-trip tests for the generated `os.lazy.accounts.v1` stubs (issue #285).

use messenger_generated::os_lazy_accounts_v1::*;

fn user() -> User {
    User {
        name: "guest".into(),
        uid: 1001,
        gid: 100,
        home: "/home/guest".into(),
        shell: "/bin/sh".into(),
    }
}

#[test]
fn lookup_by_name_and_by_uid_are_distinguishable() {
    let by_name = LookupArgs {
        name: Some("guest".into()),
        uid: None,
    };
    let body = encode_lookup_args(&by_name).unwrap();
    assert_eq!(decode_lookup_args(&body).unwrap(), by_name);

    let by_uid = LookupArgs {
        name: None,
        uid: Some(0),
    };
    let body = encode_lookup_args(&by_uid).unwrap();
    let decoded = decode_lookup_args(&body).unwrap();
    assert_eq!(decoded, by_uid);
    assert_eq!(decoded.uid, Some(0));
}

#[test]
fn lookup_reply_found_and_missing() {
    let found = LookupReply {
        found: true,
        user: Some(user()),
    };
    let body = encode_lookup_reply(&found).unwrap();
    assert_eq!(decode_lookup_reply(&body).unwrap(), found);

    let missing = LookupReply::default();
    let body = encode_lookup_reply(&missing).unwrap();
    let decoded = decode_lookup_reply(&body).unwrap();
    assert!(!decoded.found);
    assert!(decoded.user.is_none());
}

#[test]
fn authenticate_roundtrips() {
    let args = AuthenticateArgs {
        name: "root".into(),
        secret: "hunter2".into(),
    };
    let body = encode_authenticate_args(&args).unwrap();
    assert_eq!(decode_authenticate_args(&body).unwrap(), args);
    for ok in [true, false] {
        let body = encode_authenticate_reply(&AuthenticateReply { ok }).unwrap();
        assert_eq!(decode_authenticate_reply(&body).unwrap().ok, ok);
    }
}

#[test]
fn create_roundtrips_with_empty_strings() {
    let args = CreateArgs {
        user: NewUser {
            name: "ann".into(),
            uid: 2000,
            gid: 2000,
            secret: "s3cret".into(),
            home: String::new(),
            shell: String::new(),
        },
    };
    let body = encode_create_args(&args).unwrap();
    assert_eq!(decode_create_args(&body).unwrap(), args);

    let reply = CreateReply {
        ok: false,
        detail: "uid already exists".into(),
    };
    let body = encode_create_reply(&reply).unwrap();
    assert_eq!(decode_create_reply(&body).unwrap(), reply);
}

#[test]
fn truncated_body_is_rejected() {
    let body = encode_create_args(&CreateArgs {
        user: NewUser {
            name: "ann".into(),
            ..NewUser::default()
        },
    })
    .unwrap();
    assert!(decode_create_args(&body[..body.len() - 2]).is_err());
}
