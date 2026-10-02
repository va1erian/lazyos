use super::*;

fn digest(seed: char) -> String {
    core::iter::repeat(seed).take(64).collect()
}

fn shipped(name: &str, version: &str, seed: char) -> Shipped {
    Shipped {
        system_name: name.into(),
        version: version.into(),
        digest: digest(seed),
        autostart: false,
    }
}

fn row(name: &str, version: &str, seed: char, core: bool) -> Current {
    Current {
        system_name: name.into(),
        version: version.into(),
        digest: digest(seed),
        core,
    }
}

#[test]
fn the_decision_table() {
    let ship = [
        shipped("os.lazy.editor", "0.1.0", 'a'),  // missing
        shipped("os.lazy.files", "0.1.0", 'b'),   // same digest, core
        shipped("os.lazy.paint", "0.2.0", 'c'),   // older installed
        shipped("os.lazy.docs", "0.1.0", 'd'),    // newer user-installed
        shipped("os.lazy.sysmon", "0.1.0", 'e'),  // same version, rebuilt
        shipped("os.lazy.counter", "0.1.0", 'f'), // same digest, not yet core
    ];
    let rows = [
        row("os.lazy.files", "0.1.0", 'b', true),
        row("os.lazy.paint", "0.1.0", '1', true),
        row("os.lazy.docs", "0.3.0", '2', true),
        row("os.lazy.sysmon", "0.1.0", '3', true),
        row("os.lazy.counter", "0.1.0", 'f', false),
        row("os.lazy.widget", "0.1.0", '4', true), // no longer shipped
        row("org.lazy.counter", "1.0.0", '5', false), // a user app: untouched
    ];
    assert_eq!(
        plan(&ship, &rows),
        [
            Action::MarkCore("os.lazy.counter".into()),
            Action::Keep {
                system_name: "os.lazy.docs".into(),
                installed: "0.3.0".into(),
                shipped: "0.1.0".into(),
            },
            Action::Install("os.lazy.editor".into()),
            Action::Upgrade("os.lazy.paint".into()),
            Action::Upgrade("os.lazy.sysmon".into()),
            Action::Demote("os.lazy.widget".into()),
        ]
    );
}

#[test]
fn an_unparseable_installed_version_is_replaced_not_kept() {
    let ship = [shipped("os.lazy.paint", "0.1.0", 'a')];
    let rows = [row("os.lazy.paint", "garbage", 'b', true)];
    assert_eq!(
        plan(&ship, &rows),
        [Action::Upgrade("os.lazy.paint".into())]
    );
    // 1.0 and 1.0.0 are the same version: not newer, so the shipped one wins.
    let ship = [shipped("os.lazy.paint", "1.0.0", 'a')];
    let rows = [row("os.lazy.paint", "1.0", 'b', true)];
    assert_eq!(
        plan(&ship, &rows),
        [Action::Upgrade("os.lazy.paint".into())]
    );
    // A pre-release is older than its release.
    let rows = [row("os.lazy.paint", "1.0.0-rc1", 'b', true)];
    assert_eq!(
        plan(&ship, &rows),
        [Action::Upgrade("os.lazy.paint".into())]
    );
}

#[test]
fn a_provisioned_set_is_a_no_op() {
    let ship = [
        shipped("os.lazy.editor", "0.1.0", 'a'),
        shipped("os.lazy.files", "0.1.0", 'b'),
    ];
    let rows = [
        row("os.lazy.files", "0.1.0", 'b', true),
        row("os.lazy.editor", "0.1.0", 'a', true),
    ];
    assert!(plan(&ship, &rows).is_empty());
}

#[test]
fn the_stamp_depends_on_names_and_digests_not_order() {
    let a = shipped("os.lazy.editor", "0.1.0", 'a');
    let b = shipped("os.lazy.files", "0.1.0", 'b');
    let one = stamp(&[a.clone(), b.clone()]);
    assert_eq!(one, stamp(&[b.clone(), a.clone()]));
    assert_eq!(one.len(), 64);
    let mut rebuilt = b.clone();
    rebuilt.digest = digest('c');
    assert_ne!(one, stamp(&[a.clone(), rebuilt]));
    assert_ne!(one, stamp(&[a.clone()]));
    // The version is not part of it: the digest already changes with it.
    let mut relabelled = b.clone();
    relabelled.version = "9.9.9".into();
    assert_eq!(one, stamp(&[a, relabelled]));
}

#[test]
fn up_to_date_needs_the_stamp_and_every_core_row() {
    let ship = [shipped("os.lazy.editor", "0.1.0", 'a')];
    let good = [row("os.lazy.editor", "0.1.0", 'a', true)];
    let stored = stamp(&ship);
    assert!(up_to_date(Some(&stored), &ship, &good));
    assert!(!up_to_date(None, &ship, &good));
    assert!(!up_to_date(Some("0"), &ship, &good));
    // A row someone deleted, or one no longer marked core, forces a pass.
    assert!(!up_to_date(Some(&stored), &ship, &[]));
    let user = [row("os.lazy.editor", "0.1.0", 'a', false)];
    assert!(!up_to_date(Some(&stored), &ship, &user));
}

#[test]
fn the_index_round_trips_and_rejects_bad_lines() {
    let mut terminal = shipped("os.lazy.terminal", "0.1.0", 'c');
    terminal.autostart = true;
    let ship = vec![
        shipped("os.lazy.editor", "0.1.0", 'a'),
        shipped("os.lazy.files", "0.1.0-rc.1", 'b'),
        terminal,
    ];
    let text = format_index(&ship);
    assert!(text.contains(" autostart\n"));
    assert_eq!(parse_index(&text), Ok(ship.clone()));
    let commented = format!("# core packages\n\n{text}");
    assert_eq!(parse_index(&commented), Ok(ship));
    for (bad, line) in [
        ("os.lazy.editor 0.1.0\n", 1),
        ("os.lazy.editor 0.1.0 abc\n", 1),
        (&*format!("Editor 0.1.0 {}\n", digest('a')), 1),
        (&*format!("os.lazy.editor 01.0 {}\n", digest('a')), 1),
        (&*format!("os.lazy.editor 0.1.0 {} extra\n", digest('a')), 1),
        (&*format!("os.lazy.editor 0.1.0 {} autostart x\n", digest('a')), 1),
        (&*format!("os.lazy.editor 0.1.0 {}\n", digest('A')), 1),
        (
            &*format!(
                "os.lazy.a 0.1.0 {}\nos.lazy.a 0.1.0 {}\n",
                digest('a'),
                digest('b')
            ),
            2,
        ),
    ] {
        assert_eq!(parse_index(bad).map_err(|e| e.line), Err(line), "{bad}");
    }
}

#[test]
fn package_file_names_are_system_names() {
    assert_eq!(
        package_file_name("os.lazy.paint.lzp"),
        Some("os.lazy.paint")
    );
    assert_eq!(package_file_name("os.lazy.paint.zip"), None);
    assert_eq!(package_file_name("PAINT.LZP"), None);
    assert_eq!(package_file_name("index"), None);
    assert_eq!(package_file_name("../x.lzp"), None);
}

#[test]
fn core_apps_cannot_be_removed_or_downgraded() {
    let refused = removal("Paint", true).unwrap_err();
    assert!(refused.contains("Paint is part of LazyOS"), "{refused}");
    assert!(refused.contains("hide it from the menu in Settings"));
    assert_eq!(removal("Counter", false), Ok(()));

    assert_eq!(downgrade("Paint", "0.2.0", "0.2.0"), Ok(()));
    assert_eq!(downgrade("Paint", "0.2.0", "0.2.1"), Ok(()));
    assert_eq!(downgrade("Paint", "0.2.0", "0.10.0"), Ok(()));
    assert!(downgrade("Paint", "0.2.0", "0.1.9").is_err());
    assert!(downgrade("Paint", "1.0.0", "1.0.0-rc1").is_err());
    assert!(downgrade("Paint", "1.0.0", "bogus").is_err());
}

#[test]
fn the_done_line_is_the_documented_marker() {
    let tally = Tally {
        installed: 12,
        upgraded: 0,
        kept: 1,
        failed: 0,
    };
    assert_eq!(
        tally.done_line(),
        "PKGD:PROVISION:DONE installed=12 upgraded=0 kept=1 failed=0\n"
    );
}

#[test]
fn installs_come_largest_first() {
    let mut actions = vec![
        Action::Demote("os.lazy.a".into()),
        Action::Install("os.lazy.small".into()),
        Action::Upgrade("os.lazy.big".into()),
        Action::Install("os.lazy.mid".into()),
    ];
    let size = |name: &str| match name {
        "os.lazy.big" => 300,
        "os.lazy.mid" => 200,
        _ => 100,
    };
    largest_first(&mut actions, size);
    assert_eq!(
        actions,
        [
            Action::Upgrade("os.lazy.big".into()),
            Action::Install("os.lazy.mid".into()),
            Action::Install("os.lazy.small".into()),
            Action::Demote("os.lazy.a".into()),
        ]
    );
}

#[test]
fn autostart_packages_come_first() {
    let mut actions = vec![
        Action::Install("os.lazy.docs".into()),
        Action::Demote("os.lazy.old".into()),
        Action::Install("os.lazy.terminal".into()),
        Action::Upgrade("os.lazy.editor".into()),
    ];
    let autostart = |name: &str| name == "os.lazy.terminal";
    assert_eq!(autostart_steps(&actions, autostart), 1);
    autostart_first(&mut actions, autostart);
    assert_eq!(
        actions,
        [
            Action::Install("os.lazy.terminal".into()),
            Action::Install("os.lazy.docs".into()),
            Action::Demote("os.lazy.old".into()),
            Action::Upgrade("os.lazy.editor".into()),
        ]
    );
}
