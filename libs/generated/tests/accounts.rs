//! Round-trip tests for the generated `os.lazy.accounts.v1` stubs (issues
//! #285, #624).

use messenger_generated::os_lazy_accounts_v1::*;

fn user() -> User {
    User {
        name: "guest".into(),
        uid: 1001,
        gid: 1001,
        home: "/home/guest".into(),
        shell: "sh".into(),
        admin: true,
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
        name: "admin".into(),
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
fn account_management_roundtrips() {
    let create = CreateArgs {
        name: "ann".into(),
        secret: "s3cret".into(),
        admin: false,
    };
    let body = encode_create_args(&create).unwrap();
    assert_eq!(decode_create_args(&body).unwrap(), create);
    let reply = CreateReply { user: user() };
    let body = encode_create_reply(&reply).unwrap();
    assert_eq!(decode_create_reply(&body).unwrap(), reply);

    let delete = DeleteArgs {
        name: "ann".into(),
        home: "archive".into(),
    };
    let body = encode_delete_args(&delete).unwrap();
    assert_eq!(decode_delete_args(&body).unwrap(), delete);

    // `old` absent (elevd setting anyone's) and present (a user's own).
    for old in [None, Some(String::from("before"))] {
        let args = SetPasswordArgs {
            name: "ann".into(),
            old,
            secret: "after".into(),
        };
        let body = encode_set_password_args(&args).unwrap();
        assert_eq!(decode_set_password_args(&body).unwrap(), args);
    }

    let promote = SetAdminArgs {
        name: "ann".into(),
        admin: true,
    };
    let body = encode_set_admin_args(&promote).unwrap();
    assert_eq!(decode_set_admin_args(&body).unwrap(), promote);

    let list = ListUsersReply {
        users: vec![user(), User::default()],
        setup: false,
    };
    let body = encode_list_users_reply(&list).unwrap();
    assert_eq!(decode_list_users_reply(&body).unwrap(), list);
}

#[test]
fn truncated_body_is_rejected() {
    let body = encode_create_args(&CreateArgs {
        name: "ann".into(),
        ..CreateArgs::default()
    })
    .unwrap();
    assert!(decode_create_args(&body[..body.len() - 2]).is_err());
}
