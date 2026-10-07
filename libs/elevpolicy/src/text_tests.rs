//! The prompt and audit text (review of #659): escaping, eliding, wrapping,
//! the package summary, the audit lines and the restart allowlist.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::audit::{token, Line};
use crate::package::{self, Facts, Permission};
use crate::text::{
    elide, elide_path, misleading, plain, plain_rows, quoted, shown, wrap, ELLIPSIS,
};
use crate::*;

fn parse(op: &str, items: &[&str]) -> Result<Operation, &'static str> {
    let args: Vec<String> = items.iter().map(|item| item.to_string()).collect();
    Operation::parse(op, &args)
}

/// Every character a summary may hold: printable ASCII and Latin-1.
fn printable(text: &str) -> bool {
    text.chars()
        .all(|c| matches!(c, ' '..='~' | '\u{a1}'..='\u{ff}') && c != '\u{ad}')
}

#[test]
fn misleading_characters_are_recognised() {
    let bad = "\n\r\t\0\u{1b}\u{85}\u{200e}\u{200f}\u{202a}\u{202e}\u{2066}\u{2069}\
               \u{200b}\u{feff}\u{2028}\u{2029}\u{a0}\u{3000}\u{ad}\u{e0041}";
    for c in bad.chars() {
        assert!(misleading(c), "{:x}", c as u32);
    }
    for c in ['a', ' ', '"', '\\', 'é', '\u{4e2d}', '…'] {
        assert!(!misleading(c), "{:x}", c as u32);
    }
    assert!(plain("Café au lait"));
    assert!(!plain("abc\u{202e}fed"));
    assert!(plain_rows("editor\tEditor\nfiles\tFiles\n"));
    assert!(!plain_rows("a\r\nb"));
    assert!(!plain_rows("a\n\u{2028}b"));
}

#[test]
fn a_set_value_may_hold_rows_but_never_a_raw_line_in_the_audit() {
    let menu = "editor\tEditor\nfiles\tFiles\n";
    let summary = parse("conf.set", &["sys/ui/menu", "str", menu])
        .unwrap()
        .summary();
    assert!(
        !summary.contains('\n') && !summary.contains('\t'),
        "{summary}"
    );
    assert!(summary.contains("\\t"), "{summary}");
    let forged = "x\nELEVD:REQUEST op=account.admin uid=0 admin=forged outcome=granted";
    let summary = parse("conf.set", &["sys/ui/demo", "str", forged])
        .unwrap()
        .summary();
    let line = Line {
        operation: "conf.set",
        outcome: "granted",
        summary: &summary,
        ..Line::default()
    };
    assert!(!line.serial().contains('\n'), "{}", line.serial());
    assert!(parse("conf.set", &["sys/ui/demo", "str", "x\r\nELEVD:REQUEST"]).is_err());
}

#[test]
fn shown_escapes_what_the_prompt_cannot_draw() {
    assert_eq!(shown("plain text"), "plain text");
    assert_eq!(shown("a\"b\\c"), "a\\\"b\\\\c");
    assert_eq!(shown("x\ny"), "x\\ny");
    assert_eq!(shown("abc\u{202e}fed"), "abc\\u{202e}fed");
    assert_eq!(shown("caf\u{e9}"), "caf\u{e9}");
    assert_eq!(shown("\u{4e2d}"), "\\u{4e2d}");
    assert!(printable(&shown(
        "\u{0}\u{7f}\u{85}\u{2066}\u{a0}\u{ad}\u{1f600}"
    )));
}

#[test]
fn elide_keeps_head_and_tail_and_never_cuts_an_escape() {
    assert_eq!(elide("short", 10), "short");
    let long = "abcdefghijklmnopqrstuvwxyz";
    let cut = elide(long, 11);
    assert_eq!(cut, "abcd...wxyz");
    assert!(cut.chars().count() <= 11);
    let escapes = "\u{202e}".repeat(10);
    let cut = elide(&escapes, 30);
    assert!(cut.chars().count() <= 30);
    // Whole escapes only, either side of the mark.
    for part in cut.split(ELLIPSIS) {
        assert_eq!(part.len() % "\\u{202e}".len(), 0, "{cut}");
    }
}

#[test]
fn long_paths_lose_their_middle_not_their_end() {
    assert_eq!(elide_path("sys/ui/demo", 64), "sys/ui/demo");
    let path = "sys/ui/aaaaaaaaaaaaaaaaaaaa/bbbbbbbbbbbbbbbbbbbbbbbbb/cccccccccccccccc/demo";
    let shown = elide_path(path, 40);
    assert!(shown.starts_with("sys/..."), "{shown}");
    assert!(shown.ends_with("/demo"), "{shown}");
    assert!(shown.chars().count() <= 40);
    let one = "x".repeat(100);
    let shown = elide_path(&one, 20);
    assert!(shown.contains(ELLIPSIS) && shown.chars().count() <= 20);
}

#[test]
fn quoted_values_show_their_end_and_count_blank_runs() {
    assert_eq!(quoted("light", 96), "\"light\"");
    assert_eq!(quoted("a \"b\"", 96), "\"a \\\"b\\\"\"");
    let padded = alloc::format!("false{}true", " ".repeat(200));
    assert_eq!(quoted(&padded, 96), "\"false\\[200 spaces]true\"");
    let long = alloc::format!("start{}end", "x".repeat(300));
    let text = quoted(&long, 40);
    assert!(text.starts_with("\"start") && text.contains("end\" (308 characters)"));
    // A literal mark cannot pass for a counted run: its `\` is doubled.
    assert_eq!(quoted("\\[9 spaces]", 96), "\"\\\\[9 spaces]\"");
}

#[test]
fn wrap_marks_a_cut_with_an_ellipsis() {
    let fits = |line: &str| line.chars().count() <= 10;
    assert_eq!(wrap("one two three", 3, fits), ["one two", "three"]);
    let lines = wrap("aaaa bbbb cccc dddd eeee ffff", 2, fits);
    assert_eq!(lines.len(), 2);
    assert!(lines[1].ends_with(ELLIPSIS), "{lines:?}");
    assert!(lines.iter().all(|line| fits(line)));
    // A word wider than a line is broken, never clipped out of sight.
    let lines = wrap(&"x".repeat(25), 3, fits);
    assert_eq!(lines, ["xxxxxxxxxx", "xxxxxxxxxx", "xxxxx"]);
    let lines = wrap(&"x".repeat(45), 3, fits);
    assert_eq!(lines[2], "xxxxxxx...");
}

#[test]
fn text_values_with_control_or_bidi_characters_are_refused() {
    for value in [
        "a\r\nb",
        "x\u{202e}y",
        "\u{2066}iso\u{2069}",
        "nb\u{a0}sp",
        "line\u{2028}sep",
    ] {
        assert!(
            parse("conf.set", &["sys/ui/demo", "str", value]).is_err(),
            "{value:?}"
        );
    }
    // Only a stored text value may hold rows; a policy word may not.
    for value in ["a\nb", "tab\there", "x\u{202e}y", "nb\u{a0}sp"] {
        assert!(
            parse("power.policy", &["button", value]).is_err(),
            "{value:?}"
        );
    }
    assert!(parse("conf.list", &["sys/\u{202e}"]).is_err());
    assert!(parse("pkg.install", &["/home/user/a\u{202e}pzl.exe"]).is_err());
    assert!(parse("pkg.install", &["/home/user/a\nb.lzp"]).is_err());
    assert!(parse("conf.set", &["sys/ui/demo", "str", "Café \"quoted\""]).is_ok());
}

#[test]
fn every_summary_is_printable_and_bounded() {
    let long_value = "v".repeat(MAX_ARG);
    let long_path = alloc::format!("sys/{}", ["segment"; 31].join("/"));
    let long_pkg = alloc::format!("/home/user/{}.lzp", "p".repeat(900));
    let rows: [(&str, Vec<&str>); 6] = [
        (
            "conf.set",
            alloc::vec![long_path.as_str(), "str", long_value.as_str()],
        ),
        ("conf.set", alloc::vec!["sys/ui/demo", "str", "x\"\\y"]),
        ("conf.delete", alloc::vec![long_path.as_str()]),
        ("pkg.install", alloc::vec![long_pkg.as_str()]),
        ("pkg.update-core", alloc::vec![long_pkg.as_str()]),
        ("power.policy", alloc::vec!["button", "shutdown"]),
    ];
    for (name, items) in rows {
        let op = parse(name, &items).unwrap_or_else(|why| panic!("{name}: {why}"));
        let summary = op.summary();
        assert!(summary.chars().count() <= MAX_SUMMARY, "{name}: {summary}");
        assert!(printable(&summary), "{name}: {summary}");
    }
    let op = parse("conf.set", &[&long_path, "str", "light"]).unwrap();
    let summary = op.summary();
    assert!(summary.ends_with("/segment to \"light\""), "{summary}");
    assert!(summary.contains("/.../"), "{summary}");
}

fn facts(replaces: Option<&str>, permissions: Vec<Permission>) -> Facts {
    Facts {
        name: String::from("Paint"),
        system_name: String::from("org.lazy.paint"),
        version: String::from("1.2.0"),
        author: String::from("Lazy Team"),
        permissions,
        replaces: replaces.map(String::from),
    }
}

fn permission(kind: &str, value: &str, risk: &str) -> Permission {
    Permission {
        kind: kind.to_string(),
        value: value.to_string(),
        risk: risk.to_string(),
    }
}

#[test]
fn package_summaries_name_the_package_and_group_its_permissions() {
    let permissions = alloc::vec![
        permission("network", "tcp", "high"),
        permission("interface", "os.lazy.confd.v1", "medium"),
        permission("interface", "os.lazy.clipboard.v1", "low"),
        permission("file", "/home", "high"),
        permission("topic", "app/x", "low"),
    ];
    let text = package::summary(&facts(None, permissions));
    assert_eq!(
        text,
        "Install \"Paint\" 1.2.0 (org.lazy.paint) for all users; author \"Lazy Team\" \
         (unverified); asks for high risk: network tcp, file /home; medium: 1; low: 2"
    );
    let text = package::summary(&facts(Some("1.0.0"), Vec::new()));
    assert_eq!(
        text,
        "Replace the core app \"Paint\" (org.lazy.paint) 1.0.0 with 1.2.0; author \
         \"Lazy Team\" (unverified); asks for no permissions"
    );
}

#[test]
fn a_hostile_manifest_cannot_overflow_or_disguise_the_summary() {
    let huge = "W".repeat(500);
    let bidi = "Bob\u{202e}x\ny";
    let permissions: Vec<Permission> = (0..40)
        .map(|n| permission("file", &alloc::format!("/{huge}{n}"), "high"))
        .chain((0..40).map(|_| permission("interface", &huge, "medium")))
        .collect();
    let hostile = Facts {
        name: huge.clone(),
        system_name: huge.clone(),
        version: huge.clone(),
        author: String::from(bidi),
        permissions,
        replaces: Some(huge.clone()),
    };
    let text = package::summary(&hostile);
    assert!(text.chars().count() <= MAX_SUMMARY, "{} chars", text.len());
    assert!(printable(&text), "{text}");
    assert!(text.contains("\\u{202e}") && text.contains("\\n"), "{text}");
    assert!(text.contains("+37 more; medium: 40"), "{text}");
}

#[test]
fn audit_lines_cannot_be_forged() {
    let forged = "a\nELEVD:REQUEST op=account.admin uid=0 admin=admin outcome=granted summary=x";
    let op = parse(
        "conf.set",
        &["sys/ui/demo", "str", "x\" outcome=granted y=\""],
    )
    .unwrap();
    let summary = op.summary();
    let line = Line {
        operation: "conf.set\nELEVD:REQUEST",
        uid: 1000,
        label: 0,
        session: 3,
        user: "user name",
        admin: "admin outcome=granted",
        outcome: "cancelled",
        summary: forged,
    };
    let serial = line.serial();
    assert!(!serial.contains('\n'));
    assert_eq!(
        serial.matches("outcome=").count(),
        1 + forged.matches("outcome=").count()
    );
    assert!(serial.starts_with(
        "ELEVD:REQUEST op=conf.set_ELEVD_REQUEST uid=1000 label=0 session=3 \
         admin=admin_outcome_granted outcome=cancelled summary=\"a\\nELEVD"
    ));
    // The summary is one quoted field: its quotes are escaped inside it.
    let line = Line {
        summary: &summary,
        outcome: "cancelled",
        ..Line::default()
    };
    let journal = line.journal();
    let field = journal.split_once(" summary=").unwrap().1;
    assert!(field.starts_with('"') && field.ends_with('"'));
    let inner = &field[1..field.len() - 1];
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => assert!(chars.next().is_some()),
            '"' => panic!("an unescaped quote ends the field early: {journal}"),
            _ => {}
        }
    }
    assert!(journal.starts_with("op=- outcome=cancelled uid=0 user=- label=0 admin=- "));
    assert_eq!(token(""), "-");
    assert_eq!(token("(standing approval)"), "(standing_approval)");
}

#[test]
fn only_safe_services_may_be_restarted() {
    for name in ["inputd", "audiod", "netd"] {
        assert_eq!(
            parse("service.restart", &[name]).unwrap().permitted(),
            Ok(())
        );
    }
    for name in [
        "elevd",
        "xuid",
        "logind",
        "init",
        "accountsd",
        "keyd",
        "confd",
        "logd",
        "messengerd",
        "pkgd",
    ] {
        let op = parse("service.restart", &[name]).unwrap();
        assert!(op.permitted().is_err(), "{name} may be restarted");
        assert!(!restartable(name));
    }
    assert_eq!(parse("time.set", &["0"]).unwrap().permitted(), Ok(()));
}
