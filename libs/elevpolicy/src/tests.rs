use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::approvals::{Approvals, Caller};
use crate::*;

fn args(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| item.to_string()).collect()
}

fn parse(op: &str, items: &[&str]) -> Result<Operation, &'static str> {
    Operation::parse(op, &args(items))
}

#[test]
fn every_row_parses_and_round_trips() {
    let rows: [(&str, &[&str]); 15] = [
        ("pkg.install", &["/transient/demo.lzp"]),
        ("pkg.update-core", &["/home/user/counter.lzp"]),
        ("pkg.remove", &["org.lazy.demo"]),
        ("conf.set", &["sys/ui/demo", "str", "hello"]),
        ("conf.delete", &["sys/ui/demo"]),
        ("conf.list", &[""]),
        ("conf.get", &["user/1000/ui/accent"]),
        ("conf.elevate", &[]),
        ("time.set", &["1767225600"]),
        ("account.create", &["bob", "s3cret", "user"]),
        ("account.delete", &["bob", "archive"]),
        ("account.admin", &["bob", "1"]),
        ("account.password", &["bob", "n3w"]),
        ("power.policy", &["button", "shutdown"]),
        ("service.restart", &["inputd"]),
    ];
    for (name, items) in rows {
        let op = parse(name, items).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(op.name(), name);
        assert_eq!(Operation::parse(name, &op.args()), Ok(op.clone()), "{name}");
        assert!(!op.summary().is_empty());
    }
    assert_eq!(NAMES.len(), 15);
}

#[test]
fn bad_arguments_are_refused() {
    let cases: [(&str, &[&str]); 16] = [
        ("rm -rf", &[]),
        ("pkg.install", &["relative.lzp"]),
        ("pkg.install", &["/a/../b.lzp"]),
        ("pkg.remove", &["Not.Valid"]),
        ("conf.set", &["sys/ui/demo", "str"]),
        ("conf.set", &["sys//x", "str", "v"]),
        ("conf.set", &["sys/ui/demo", "float", "1.5"]),
        ("conf.set", &["sys/ui/demo", "bytes", "ABCD"]),
        ("time.set", &["-1"]),
        ("time.set", &["99999999999"]),
        ("account.create", &["Bob", "pw", "user"]),
        ("account.create", &["_accounts", "pw", "admin"]),
        ("account.create", &["bob", "", "user"]),
        ("account.delete", &["bob", "shred"]),
        ("account.admin", &["bob", "yes"]),
        ("service.restart", &["../init"]),
    ];
    for (name, items) in cases {
        assert!(parse(name, items).is_err(), "{name} {items:?} was accepted");
    }
    let long = "x".repeat(MAX_ARG + 1);
    assert!(parse("conf.list", &[&long]).is_err());
    assert!(parse("conf.elevate", &["a", "b", "c", "d", "e"]).is_err());
}

#[test]
fn summaries_say_what_changes_and_never_the_password() {
    let op = parse("account.create", &["bob", "hunter22", "admin"]).unwrap();
    assert_eq!(op.summary(), "Create the account 'bob' (an administrator)");
    let op = parse("account.password", &["bob", "hunter22"]).unwrap();
    assert!(!op.summary().contains("hunter22"));
    let op = parse("time.set", &["0"]).unwrap();
    assert_eq!(op.summary(), "Set the clock to 1970-01-01 00:00 UTC");
    let op = parse("conf.set", &["sys/ui/demo", "bytes", "00ff"]).unwrap();
    assert_eq!(op.summary(), "Set the setting sys/ui/demo to 2 bytes");
}

#[test]
fn only_the_elevated_view_stands_every_change_prompts() {
    for name in ["conf.list", "conf.get", "conf.elevate"] {
        let items: &[&str] = match name {
            "conf.set" => &["sys/a", "bool", "true"],
            "conf.elevate" => &[],
            "conf.list" => &[""],
            _ => &["sys/a"],
        };
        assert_eq!(parse(name, items).unwrap().class(), Class::View, "{name}");
    }
    for (name, items) in [
        ("conf.set", &["sys/a", "bool", "true"][..]),
        ("conf.delete", &["sys/a"][..]),
    ] {
        assert_eq!(parse(name, items).unwrap().class(), Class::Once, "{name}");
    }
    let op = parse("account.admin", &["bob", "1"]).unwrap();
    assert_eq!(op.class(), Class::Once);
}

#[test]
fn approvals_belong_to_one_caller_and_expire() {
    let caller = Caller {
        uid: 1000,
        label: 7,
        session: 1,
    };
    let mut approvals = Approvals::new();
    approvals.grant(caller, Class::Once, 0);
    assert!(
        !approvals.covers(caller, Class::View, 1),
        "a one-shot approval stood"
    );
    approvals.grant(caller, Class::View, 0);
    assert!(approvals.covers(caller, Class::View, 1));
    assert!(!approvals.covers(caller, Class::Once, 1));
    // Another program of the same user, or the same in another session.
    assert!(!approvals.covers(Caller { label: 8, ..caller }, Class::View, 1));
    assert!(!approvals.covers(
        Caller {
            session: 2,
            ..caller
        },
        Class::View,
        1
    ));
    assert!(!approvals.covers(caller, Class::View, APPROVAL_TICKS));
    approvals.grant(caller, Class::View, 10);
    approvals.release(caller);
    assert!(!approvals.covers(caller, Class::View, 11));
    approvals.grant(caller, Class::View, 10);
    approvals.end_session(1);
    assert_eq!(approvals.live(11), 0);
}

#[test]
fn the_table_is_bounded() {
    let mut approvals = Approvals::new();
    for uid in 0..100 {
        let caller = Caller {
            uid,
            label: 0,
            session: 1,
        };
        approvals.grant(caller, Class::View, 5);
    }
    assert!(approvals.live(6) <= approvals::MAX_APPROVALS);
}

#[test]
fn elevd_is_its_uid_unlabelled_outside_any_session() {
    assert!(is_elevd(ELEVD_UID, 0, 0));
    assert!(!is_elevd(ELEVD_UID, 3, 0));
    assert!(!is_elevd(ELEVD_UID, 0, 1));
    assert!(!is_elevd(0, 0, 0));
}

#[test]
fn values_round_trip() {
    for value in [
        Value::Bool(true),
        Value::I64(-5),
        Value::U64(7),
        Value::Str(String::from("a b")),
        Value::Bytes(alloc::vec![0, 255, 16]),
    ] {
        let (kind, text) = value_args(&value);
        assert_eq!(parse_value(kind, &text), Ok(value));
    }
}
