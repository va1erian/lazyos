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

#[test]
fn only_root_and_mountd_may_provide_filesystems() {
    assert_eq!(FS_PROVIDER_UIDS, &[0, MOUNTD_UID]);
    const { assert!(MOUNTD_UID < 1000) };
}

fn smb(share: &str) -> Result<Request, &'static str> {
    validate_smb("nas", "10.0.2.2", 0, share, "chaton", "pw d=x")
}

#[test]
fn an_smb_request_names_a_share_and_a_user() {
    let r = smb("share").unwrap();
    assert_eq!(
        (r.kind, r.port, r.share.as_str()),
        (Kind::Smb, SMB_PORT, "share")
    );
    assert_eq!(smb("My Files").unwrap().share, "My Files");
    for share in [
        "", ".", "..", "a/b", "a\\b", "c:", "x*", "q?", "\"", "<", ">", "|", "t\n",
    ] {
        assert!(smb(share).is_err(), "{share:?}");
    }
    assert!(smb(&"s".repeat(SHARE_MAX + 1)).is_err());
    assert!(smb(&"s".repeat(SHARE_MAX)).is_ok());
    assert!(
        validate_smb("nas", "h", 0, "s", "", "").is_err(),
        "an anonymous share"
    );
    // The FTP rules still apply to the rest.
    assert!(validate_smb("NAS", "h", 0, "s", "u", "").is_err());
    assert!(validate_smb("nas", "-h", 0, "s", "u", "").is_err());
    assert_eq!(
        validate_smb("nas", "h", 1445, "s", "u", "").unwrap().port,
        1445
    );
}

#[test]
fn the_kind_picks_the_rules() {
    let ftp = validate_kind("", "site", "h", 0, "", "", "").unwrap();
    assert_eq!((ftp.kind, ftp.port), (Kind::Ftp, DEFAULT_PORT));
    assert_eq!(
        validate_kind("ftp", "site", "h", 0, "", "", "").unwrap(),
        ftp
    );
    assert!(validate_kind("ftp", "site", "h", 0, "share", "", "").is_err());
    let smb = validate_kind("smb", "nas", "h", 0, "s", "u", "p").unwrap();
    assert_eq!(smb.kind, Kind::Smb);
    assert!(validate_kind("nfs", "n", "h", 0, "", "", "").is_err());
    assert_eq!(Kind::Smb.name(), KIND_SMB);
    assert_eq!(Kind::Ftp.default_port(), DEFAULT_PORT);
}

#[test]
fn an_smb_daemon_gets_its_password_in_the_environment_only() {
    let r = smb("share").unwrap();
    let argv = daemon_args("/system/bin/smbfuse", &r, 1000, 100);
    assert_eq!(
        argv,
        vec![
            "/system/bin/smbfuse",
            "-p",
            "445",
            "-U",
            "chaton",
            "//10.0.2.2/share",
            "name=nas",
            "owner=1000:100",
        ]
    );
    assert!(!argv.iter().any(|a| a.contains("pw")), "{argv:?}");
    assert_eq!(daemon_env(&r), vec!["LAZYOS_SMB_PASSWORD=pw d=x"]);
    assert!(daemon_env(&request("site")).is_empty());
    assert!(exit_reason(8).contains("share"));
}

#[test]
fn an_smb_entry_keeps_its_kind_and_share_but_no_password() {
    let mut table = Table::new();
    table.add(&smb("share").unwrap(), 1000, 7, 0);
    let entry = &table.entries()[0];
    assert_eq!((entry.kind, entry.share.as_str()), (Kind::Smb, "share"));
    let shown = alloc::format!("{:?}", table.entries());
    assert!(!shown.contains("pw d=x"), "{shown}");
}

mod volumes {
    use crate::volumes::*;
    use alloc::vec::Vec;

    fn disk(id: u32, generation: u64) -> DiskView {
        DiskView {
            id,
            generation,
            writable: true,
            claimed: false,
        }
    }

    const USER: (u32, u32) = (1000, 1000);

    #[test]
    fn a_new_disk_starts_one_daemon_for_the_session_user() {
        let mut v = Volumes::new();
        let plan = v.plan(&[disk(0, 1)], Some(USER));
        assert_eq!(
            plan,
            [Action::Start {
                disk: 0,
                generation: 1,
                owner: USER
            }]
        );
        // Asking again before the caller reports changes nothing.
        assert!(v.plan(&[disk(0, 1)], Some(USER)).is_empty());
        v.started(0, 1, 77);
        assert!(v.plan(&[disk(0, 1)], Some(USER)).is_empty());
        assert_eq!(v.entries()[0].state, State::Running);
    }

    #[test]
    fn nothing_mounts_without_a_session_or_on_a_claimed_disk() {
        let mut v = Volumes::new();
        assert!(v.plan(&[disk(0, 1)], None).is_empty());
        let claimed = DiskView {
            claimed: true,
            ..disk(1, 1)
        };
        assert!(v.plan(&[claimed], Some(USER)).is_empty());
        // The session arrives: the disk that was waiting starts.
        assert_eq!(v.plan(&[disk(0, 1)], Some(USER)).len(), 1);
    }

    #[test]
    fn an_exit_is_the_answer_for_that_plug_until_it_is_replugged() {
        let mut v = Volumes::new();
        v.plan(&[disk(2, 5)], Some(USER));
        v.started(2, 5, 9);
        let ended = v.exited(9, code::NO_VOLUME).unwrap();
        assert_eq!(ended.state, State::Ended(Outcome::NoVolume));
        // Not tried again while it is the same plug.
        assert!(v.plan(&[disk(2, 5)], Some(USER)).is_empty());
        // Unplugged: forgotten. Plugged again (a new generation): tried.
        assert!(v.plan(&[], Some(USER)).is_empty());
        assert!(v.entries().is_empty());
        assert_eq!(v.plan(&[disk(2, 6)], Some(USER)).len(), 1);
    }

    #[test]
    fn a_slot_given_to_another_disk_stops_the_old_daemon() {
        let mut v = Volumes::new();
        v.plan(&[disk(0, 1)], Some(USER));
        v.started(0, 1, 40);
        let plan = v.plan(&[disk(0, 2)], Some(USER));
        assert_eq!(
            plan,
            [
                Action::Stop { pid: 40 },
                Action::Start {
                    disk: 0,
                    generation: 2,
                    owner: USER
                }
            ]
        );
    }

    #[test]
    fn logging_out_stops_every_daemon_and_the_next_user_gets_their_own() {
        let mut v = Volumes::new();
        v.plan(&[disk(0, 1), disk(1, 1)], Some(USER));
        v.started(0, 1, 10);
        v.started(1, 1, 11);
        let stops = v.plan(&[disk(0, 1), disk(1, 1)], None);
        assert_eq!(stops, [Action::Stop { pid: 10 }, Action::Stop { pid: 11 }]);
        assert!(v.entries().is_empty());
        let other = (1001, 1001);
        let plan = v.plan(&[disk(0, 1), disk(1, 1)], Some(other));
        assert_eq!(plan.len(), 2);
        assert!(plan
            .iter()
            .all(|a| matches!(a, Action::Start { owner, .. } if *owner == other)));
    }

    #[test]
    fn exit_codes_read_as_words() {
        let mut v = Volumes::new();
        v.plan(&[disk(0, 1)], Some(USER));
        v.started(0, 1, 3);
        let ended = v.exited(3, code::LOST).unwrap();
        assert_eq!(ended.state, State::Ended(Outcome::Removed));
        v.plan(&[disk(1, 1)], Some(USER));
        v.started(1, 1, 4);
        let ended = v.exited(4, code::DAMAGED).unwrap();
        assert!(
            matches!(&ended.state, State::Ended(Outcome::Failed(s)) if s.contains("cannot be read"))
        );
        assert!(v.exited(99, 0).is_none());
    }

    #[test]
    fn at_most_eight_volumes() {
        let mut v = Volumes::new();
        let disks: Vec<DiskView> = (0..12).map(|i| disk(i, 1)).collect();
        assert_eq!(v.plan(&disks, Some(USER)).len(), MAX_VOLUMES);
    }

    #[test]
    fn the_command_line_names_the_plug_and_owner() {
        assert_eq!(
            daemon_args("fatfuse", 3, 12, (1000, 1000)),
            ["fatfuse", "disk=3:12", "owner=1000:1000"]
        );
    }
}
