use alloc::string::{String, ToString};
use alloc::vec;

use crate::ops::OpError;
use crate::policy::{authorize, Allowed, Change, Who};
use crate::ratelimit::{self, Key, Limiter};
use crate::*;

const SECRET: &str = "argon2id:19456:2:1:abababababababababababababababab:\
                      5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a";

fn shipped() -> String {
    alloc::format!(
        "# seed\nnext:1002\ngroup:admin:10\n\
         user:user:1000:1000:/home/user:sh::{SECRET}\r\n\
         user:admin:1001:1001:/home/admin:sh:admin:{SECRET}\n"
    )
}

fn db() -> Db {
    parse(shipped().as_bytes()).unwrap()
}

#[test]
fn the_seed_parses_with_its_admin() {
    let db = db();
    assert_eq!(db.users.len(), 2);
    assert_eq!(db.next_uid, 1002);
    assert!(db.user("admin").unwrap().is_admin());
    assert!(!db.user("user").unwrap().is_admin());
    assert_eq!(db.admins(), 1);
    assert_eq!(db.by_uid(1000).unwrap().name, "user");
    assert_eq!(
        db.user("user").unwrap().secret.as_ref().unwrap().to_text(),
        SECRET
    );
    assert!(!db.needs_setup());
}

#[test]
fn text_round_trips() {
    let db = db();
    assert_eq!(parse(db.to_text().as_bytes()).unwrap(), db);
}

#[test]
fn views_are_the_public_columns() {
    let db = db();
    assert_eq!(
        db.passwd_view(),
        "user:1000:1000:x:/home/user:sh\nadmin:1001:1001:x:/home/admin:sh\n"
    );
    assert_eq!(db.group_view(), "admin:10:admin\n");
    assert!(!db.passwd_view().contains("argon2id"));
    assert_eq!(passwd::parse(db.passwd_view().as_bytes()).unwrap().len(), 2);
}

#[test]
fn an_empty_database_waits_for_its_owner() {
    let db = parse(b"group:admin:10\n").unwrap();
    assert!(db.needs_setup());
    assert_eq!(db.next_uid, FIRST_UID);
    assert_eq!(parse(b"").unwrap().users.len(), 0);
}

#[test]
fn every_kind_of_bad_record_fails_closed() {
    let cases: [(&str, &str); 20] = [
        ("user:root:0:0:/root:sh::!", "field=name"),
        ("user:bob:0:1000:/h:sh::!", "field=uid"),
        ("user:bob:908:1000:/h:sh::!", "field=uid"),
        ("user:bob:60000:1000:/h:sh::!", "field=uid"),
        ("user:bob:1000:0:/h:sh::!", "field=gid"),
        ("user:bob:1000:10:/h:sh::!", "field=gid"),
        ("user:_svc:1000:1000:/h:sh::!", "field=name"),
        ("user:Bob:1000:1000:/h:sh::!", "field=name"),
        ("user:bob:1000:1000:home:sh::!", "field=home"),
        ("user:bob:1000:1000:/h:s h::!", "field=shell"),
        ("user:bob:1000:1000:/h:sh::plain", "field=secret"),
        ("user:bob:1000:1000:/h:sh:admin,admin:!", "field=groups"),
        ("user:bob:1000:1000:/h:sh", "field=count"),
        ("group:admin:x", "field=gid"),
        ("next:5", "field=next"),
        ("wheel:1", "field=kind"),
        ("user:bob:1000:1000:/h:sh:nogroup:!", "unknown-group"),
        (
            "user:a:1000:1000:/h:sh::!\nuser:a:1001:1001:/h:sh::!",
            "duplicate-name",
        ),
        (
            "user:a:1000:1000:/h:sh::!\nuser:b:1000:1001:/h:sh::!",
            "duplicate-uid",
        ),
        ("group:a:1\ngroup:b:1", "duplicate-gid"),
    ];
    for (text, reason) in cases {
        let error = parse(text.as_bytes()).unwrap_err().to_string();
        assert!(error.contains(reason), "{text:?}: {error}");
    }
    assert_eq!(parse(&[0xff]), Err(LoadError::NotText));
    let big = vec![b'#'; DB_MAX + 1];
    assert_eq!(parse(&big), Err(LoadError::Oversize(DB_MAX + 1)));
}

#[test]
fn create_hands_out_fresh_uids_and_never_reuses_one() {
    let mut db = db();
    let bob = db.create("bob", false, None).unwrap();
    assert_eq!((bob.uid, bob.gid), (1002, 1002));
    assert_eq!(bob.home, "/home/bob");
    assert_eq!(bob.shell, LOGIN_SHELL);
    assert!(bob.secret.is_none());
    db.delete("bob").unwrap();
    let carol = db.create("carol", true, None).unwrap();
    assert_eq!(carol.uid, 1003, "bob's uid is not handed out again");
    assert!(carol.is_admin());
    assert_eq!(parse(db.to_text().as_bytes()).unwrap(), db);
}

#[test]
fn create_refuses_bad_and_taken_names() {
    let mut db = db();
    assert_eq!(db.create("Bad", false, None), Err(OpError::BadName));
    assert_eq!(db.create("_accounts", false, None), Err(OpError::BadName));
    assert_eq!(db.create("root", false, None), Err(OpError::BadName));
    assert!(valid_account_name("owner") && !valid_account_name("_svc"));
    assert_eq!(db.create("user", false, None), Err(OpError::Exists));
    assert_eq!(db.create("admin", false, None), Err(OpError::Exists));
}

#[test]
fn the_last_admin_cannot_be_deleted_or_demoted() {
    let mut db = db();
    assert_eq!(db.delete("admin"), Err(OpError::LastAdmin));
    assert_eq!(db.set_admin("admin", false), Err(OpError::LastAdmin));
    db.set_admin("user", true).unwrap();
    assert_eq!(db.admins(), 2);
    db.set_admin("admin", false).unwrap();
    assert!(!db.user("admin").unwrap().is_admin());
    assert_eq!(db.delete("nobody"), Err(OpError::NotFound));
    assert_eq!(db.delete("user"), Err(OpError::LastAdmin));
}

#[test]
fn the_first_admin_brings_the_admin_group() {
    let mut db = parse(b"").unwrap();
    let owner = db.create("owner", true, None).unwrap();
    assert!(owner.is_admin());
    assert_eq!(db.groups[0].gid, ADMIN_GID);
    assert_eq!(parse(db.to_text().as_bytes()).unwrap(), db);
}

#[test]
fn set_secret_replaces_the_verifier() {
    let mut db = db();
    let mut verifier = Verifier::parse(SECRET).unwrap();
    verifier.hash = [1; 32];
    db.set_secret("user", verifier.clone()).unwrap();
    assert_eq!(db.user("user").unwrap().secret, Some(verifier));
}

#[test]
fn only_elevd_manages_and_users_change_their_own_password() {
    let db = db();
    let user = Who::User { uid: 1000 };
    for change in [
        Change::Create { admin: false },
        Change::Delete,
        Change::SetAdmin,
    ] {
        assert_eq!(authorize(&db, Who::Elevd, change), Ok(Allowed::Now));
        assert!(authorize(&db, user, change).is_err());
        assert!(authorize(&db, Who::User { uid: 1001 }, change).is_err());
        assert!(authorize(&db, Who::Greeter, change).is_err());
    }
    let own = Change::SetPassword {
        target: "user",
        with_old: true,
    };
    assert_eq!(authorize(&db, user, own), Ok(Allowed::WithOldPassword));
    let without_old = Change::SetPassword {
        target: "user",
        with_old: false,
    };
    assert!(authorize(&db, user, without_old).is_err());
    let other = Change::SetPassword {
        target: "admin",
        with_old: true,
    };
    assert!(authorize(&db, user, other).is_err());
    assert_eq!(authorize(&db, Who::Elevd, other), Ok(Allowed::Now));
}

#[test]
fn the_greeter_creates_only_the_first_owner() {
    let empty = parse(b"").unwrap();
    let create_admin = Change::Create { admin: true };
    assert_eq!(
        authorize(&empty, Who::Greeter, create_admin),
        Ok(Allowed::Now)
    );
    assert!(authorize(&empty, Who::Greeter, Change::Create { admin: false }).is_err());
    assert!(authorize(&db(), Who::Greeter, create_admin).is_err());
    assert!(authorize(&empty, Who::User { uid: 1000 }, create_admin).is_err());
}

#[test]
fn the_brake_frees_three_failures_then_doubles() {
    assert_eq!(ratelimit::delay(3), 0);
    assert_eq!(ratelimit::delay(4), ratelimit::BASE_DELAY);
    assert_eq!(ratelimit::delay(5), 2 * ratelimit::BASE_DELAY);
    assert_eq!(ratelimit::delay(40), ratelimit::MAX_DELAY);
    let mut limiter = Limiter::new();
    let keys = [Key::Name("admin".to_string()), Key::Caller(1000)];
    for now in 0..3 {
        assert_eq!(limiter.check(&keys, now), Ok(()));
        limiter.failed(&keys, now);
    }
    assert_eq!(limiter.check(&keys, 3), Ok(()));
    limiter.failed(&keys, 3);
    assert_eq!(limiter.check(&keys, 4), Err(ratelimit::BASE_DELAY - 1));
    // Another caller trying the same name is held by the name's key...
    assert!(limiter
        .check(&[Key::Name("admin".into()), Key::Caller(7)], 4)
        .is_err());
    // ...and the same caller trying another name by its own.
    assert!(limiter
        .check(&[Key::Name("user".into()), Key::Caller(1000)], 4)
        .is_err());
    assert_eq!(limiter.check(&keys, 3 + ratelimit::BASE_DELAY), Ok(()));
    limiter.succeeded(&keys);
    assert_eq!(limiter.failures(&keys[0], 200), 0);
}

#[test]
fn a_key_is_forgotten_after_a_quiet_spell() {
    let mut limiter = Limiter::new();
    let key = [Key::Caller(5)];
    for now in 0..6 {
        limiter.failed(&key, now);
    }
    assert!(limiter.check(&key, 6).is_err());
    let later = 5 + ratelimit::FORGET_AFTER;
    assert_eq!(limiter.check(&key, later), Ok(()));
    limiter.failed(&key, later);
    assert_eq!(limiter.failures(&key[0], later), 1);
}

#[test]
fn flooding_names_never_evicts_a_locked_caller() {
    let mut limiter = Limiter::new();
    let caller = Key::Caller(1000);
    for now in 0..6 {
        limiter.failed(core::slice::from_ref(&caller), now);
    }
    for n in 0..(2 * ratelimit::MAX_SLOTS) {
        limiter.failed(&[Key::Name(alloc::format!("n{n}"))], 10);
    }
    assert!(limiter.len() <= ratelimit::MAX_SLOTS);
    assert!(limiter.check(&[caller], 11).is_err());
}
