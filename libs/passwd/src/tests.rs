//! Host tests: the shipped file parses, and every way of breaking it fails
//! closed with a reason.

use alloc::format;
use alloc::string::ToString;
use alloc::vec::Vec;

use super::*;

/// The file the image ships (`build_support/passwd`).
const SHIPPED: &[u8] = include_bytes!("../../../build_support/passwd");

fn reason(bytes: &[u8]) -> alloc::string::String {
    parse(bytes).unwrap_err().to_string()
}

#[test]
fn the_shipped_file_has_admin_and_user() {
    let entries = parse(SHIPPED).unwrap();
    let rows: Vec<(&str, u32, u32, &str, &str)> = entries
        .iter()
        .map(|e| (e.name.as_str(), e.uid, e.gid, e.home.as_str(), e.shell.as_str()))
        .collect();
    assert_eq!(
        rows,
        [
            ("admin", 0, 0, "/home/admin", "sh"),
            ("user", 1000, 1000, "/home/user", "sh"),
        ]
    );
    assert!(entries.iter().all(|e| !e.secret.is_empty()));
}

#[test]
fn comments_blank_lines_and_crlf_are_tolerated() {
    let text = b"# accounts\n\nadmin:0:0:s:/home/admin:sh\r\n\n";
    assert_eq!(parse(text).unwrap().len(), 1);
}

#[test]
fn an_empty_file_has_no_valid_row() {
    assert_eq!(parse(b""), Err(LoadError::NoValidRow));
    assert_eq!(parse(b"\n\n# only a comment\n"), Err(LoadError::NoValidRow));
    assert_eq!(reason(b""), "no-valid-row");
}

#[test]
fn a_file_with_only_garbage_has_no_account() {
    assert_eq!(
        parse(b"this is not a passwd file\n"),
        Err(LoadError::BadRow { line: 1, field: "count" })
    );
}

#[test]
fn one_bad_row_rejects_the_whole_file() {
    // A good first row does not survive a corrupt second one.
    let text = b"admin:0:0:s:/home/admin:sh\nuser:x:1000:s:/home/user:sh\n";
    assert_eq!(reason(text), "bad-row line=2 field=uid");
}

#[test]
fn an_oversize_file_is_refused() {
    let mut text = Vec::new();
    while text.len() <= PASSWD_MAX {
        text.extend_from_slice(b"# padding padding padding padding\n");
    }
    text.extend_from_slice(b"admin:0:0:s:/home/admin:sh\n");
    assert_eq!(parse(&text), Err(LoadError::Oversize(text.len())));
    assert!(reason(&text).starts_with("oversize bytes="));
}

#[test]
fn a_duplicate_uid_is_refused() {
    let text = b"admin:0:0:s:/home/admin:sh\nevil:0:0:s:/home/evil:sh\n";
    assert_eq!(parse(text), Err(LoadError::DuplicateUid { line: 2, uid: 0 }));
    assert_eq!(reason(text), "duplicate-uid line=2 uid=0");
}

#[test]
fn a_duplicate_name_is_refused() {
    let text = b"user:1000:1000:s:/home/user:sh\nuser:1001:1001:t:/home/user:sh\n";
    assert_eq!(parse(text), Err(LoadError::DuplicateName { line: 2 }));
}

#[test]
fn a_uid_outside_u32_is_refused() {
    for uid in ["4294967296", "99999999999999999999", "-1", "+5", " 5", ""] {
        let text = format!("user:{uid}:1000:s:/home/user:sh\n");
        assert_eq!(
            parse(text.as_bytes()),
            Err(LoadError::BadRow { line: 1, field: "uid" }),
            "{uid:?}"
        );
    }
    let max = b"user:4294967295:4294967295:s:/home/user:sh\n";
    assert_eq!(parse(max).unwrap()[0].uid, u32::MAX);
}

#[test]
fn every_field_is_checked() {
    let cases: &[(&str, &str)] = &[
        ("admin:0:0:s:/home/admin", "count"),
        ("admin:0:0:s:/home/admin:sh:extra", "count"),
        (":0:0:s:/home/x:sh", "name"),
        ("Admin:0:0:s:/home/x:sh", "name"),
        ("../x:0:0:s:/home/x:sh", "name"),
        ("a b:0:0:s:/home/x:sh", "name"),
        ("abcdefghijklmnopqrstuvwxyzabcdefg:0:0:s:/home/x:sh", "name"),
        ("admin:0:g:s:/home/admin:sh", "gid"),
        ("admin:0:0::/home/admin:sh", "secret"),
        ("admin:0:0:s:home/admin:sh", "home"),
        ("admin:0:0:s:/home/../etc:sh", "home"),
        ("admin:0:0:s:/home//admin:sh", "home"),
        ("admin:0:0:s:/home/admin:", "shell"),
        ("admin:0:0:s:/home/admin:s h", "shell"),
    ];
    for (row, field) in cases {
        let text = format!("{row}\n");
        assert_eq!(
            parse(text.as_bytes()),
            Err(LoadError::BadRow { line: 1, field }),
            "{row:?}"
        );
    }
}

#[test]
fn non_utf8_is_refused() {
    assert_eq!(parse(b"admin:0:0:\xff:/home/admin:sh\n"), Err(LoadError::NotText));
}

#[test]
fn reasons_for_io_failures() {
    assert_eq!(LoadError::Missing.to_string(), "missing");
    assert_eq!(LoadError::Unreadable(13).to_string(), "unreadable errno=13");
}

#[test]
fn names_are_plain_path_components() {
    for good in ["admin", "user", "_svc", "a-b_c9"] {
        assert!(valid_name(good), "{good}");
    }
    for bad in ["", ".", "..", "a/b", "Root", "9lives", "a.b"] {
        assert!(!valid_name(bad), "{bad}");
    }
}
