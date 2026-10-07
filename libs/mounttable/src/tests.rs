use alloc::vec;

use crate::*;

fn request(name: &str) -> Request {
    validate(name, "10.0.2.2", 2121, "lazy", "os").unwrap()
}

#[test]
fn a_well_formed_request_passes() {
    let r = validate("ftp-1_a", "files.example.org", 0, "", "").unwrap();
    assert_eq!(r.port, DEFAULT_PORT);
    assert_eq!(r.user, "");
    assert_eq!(request("x").port, 2121);
}

#[test]
fn names_that_are_not_one_directory_are_refused() {
    for name in [
        "",
        "UPPER",
        "a/b",
        "..",
        "a b",
        "dot.name",
        &"n".repeat(NAME_MAX + 1),
    ] {
        assert!(validate(name, "h", 21, "", "").is_err(), "{name:?}");
    }
    assert!(validate(&"n".repeat(NAME_MAX), "h", 21, "", "").is_ok());
}

#[test]
fn hosts_that_would_be_options_or_carry_a_port_are_refused() {
    for host in [
        "",
        "-v",
        "a=b",
        "h:21",
        "a b",
        "h\n",
        &"h".repeat(HOST_MAX + 1),
    ] {
        assert!(validate("n", host, 21, "", "").is_err(), "{host:?}");
    }
}

#[test]
fn ports_out_of_range_are_refused() {
    assert!(validate("n", "h", 65536, "", "").is_err());
    assert_eq!(validate("n", "h", 65535, "", "").unwrap().port, 65535);
}

#[test]
fn credentials_are_bounded_and_printable() {
    assert!(validate("n", "h", 21, "a\u{7}", "p").is_err());
    assert!(validate("n", "h", 21, "a", "p\0").is_err());
    assert!(validate("n", "h", 21, &"u".repeat(CREDENTIAL_MAX + 1), "").is_err());
    assert!(validate("n", "h", 21, "", "secret").is_err());
    // Spaces and `=` are fine: each is its own argv item.
    assert!(validate("n", "h", 21, "a user", "p=a ss").is_ok());
}

#[test]
fn daemon_args_name_every_option() {
    let argv = daemon_args("/system/bin/ftpfuse", &request("site"), 1000, 100);
    assert_eq!(
        argv,
        vec![
            "/system/bin/ftpfuse",
            "10.0.2.2:2121",
            "user=lazy",
            "pass=os",
            "name=site",
            "owner=1000:100",
        ]
    );
    let anonymous = validate("pub", "h", 0, "", "").unwrap();
    let argv = daemon_args("ftpfuse", &anonymous, 0, 0);
    assert_eq!(argv, vec!["ftpfuse", "h:21", "name=pub", "owner=0:0"]);
}

#[test]
fn exit_statuses_have_reasons() {
    assert!(exit_reason(5).contains("log in"));
    assert!(exit_reason(6).contains("in use"));
    assert_eq!(exit_reason(137), "the daemon was stopped (signal 9)");
    assert_eq!(exit_reason(42), "the daemon exited with status 42");
}

#[test]
fn a_mount_connects_then_mounts() {
    let mut table = Table::new();
    let r = request("a");
    table.admit(&r).unwrap();
    table.add(&r, 1000, 7, 100);
    assert_eq!(table.connecting().collect::<vec::Vec<_>>(), ["a"]);
    assert_eq!(table.next_deadline(), Some(100 + MOUNT_TICKS));
    table.mounted("a");
    assert_eq!(table.entries()[0].state, State::Mounted);
    assert_eq!(table.connecting().count(), 0);
    assert_eq!(table.next_deadline(), None);
    assert_eq!(table.daemons().collect::<vec::Vec<_>>(), [7]);
}

#[test]
fn a_daemon_exit_fails_its_mount_with_the_reason() {
    let mut table = Table::new();
    table.add(&request("a"), 0, 7, 0);
    assert!(table.exited(8, 5).is_none());
    assert_eq!(table.exited(7, 5).map(|e| e.name.as_str()), Some("a"));
    let entry = &table.entries()[0];
    assert_eq!(entry.state.name(), "failed");
    assert!(entry.state.detail().contains("log in"));
    assert_eq!(entry.pid, None);
    // A mount that is never mounted does not become mounted later.
    table.mounted("a");
    assert_eq!(table.entries()[0].state.name(), "failed");
}

#[test]
fn a_slow_daemon_expires_and_is_stopped() {
    let mut table = Table::new();
    table.add(&request("a"), 0, 7, 0);
    table.add(&request("b"), 0, 8, 1000);
    assert!(table.expire(MOUNT_TICKS - 1).is_empty());
    assert_eq!(
        table.expire(MOUNT_TICKS),
        [(alloc::string::String::from("a"), 7)]
    );
    assert_eq!(table.entries()[0].state.detail(), TIMED_OUT);
    // Its exit after the kill finds no pid and keeps the timeout reason.
    assert!(table.exited(7, 137).is_none());
    assert_eq!(table.entries()[0].state.detail(), TIMED_OUT);
    assert_eq!(table.next_deadline(), Some(1000 + MOUNT_TICKS));
}

#[test]
fn names_are_unique_and_slots_bounded() {
    let mut table = Table::new();
    for i in 0..MAX_MOUNTS {
        let r = request(&alloc::format!("m{i}"));
        table.admit(&r).unwrap();
        table.add(&r, 0, i as u64, 0);
    }
    assert_eq!(table.admit(&request("m0")), Err(Error::Exists));
    assert_eq!(table.admit(&request("other")), Err(Error::Full));
}

#[test]
fn only_the_owner_or_root_removes() {
    let mut table = Table::new();
    table.add(&request("a"), 1000, 7, 0);
    table.add(&request("b"), 1000, 8, 0);
    assert_eq!(table.remove("a", 1001), Err(Error::Denied));
    assert_eq!(table.remove("zz", 0), Err(Error::NotFound));
    assert_eq!(table.remove("a", 1000), Ok(Some(7)));
    table.exited(8, 3);
    assert_eq!(table.remove("b", 0), Ok(None));
    assert!(table.entries().is_empty());
}

#[test]
fn the_table_never_keeps_a_password() {
    let mut table = Table::new();
    table.add(&request("a"), 0, 7, 0);
    let shown = alloc::format!("{:?}", table.entries());
    assert!(!shown.contains("os\""), "{shown}");
}
