//! Source naming, escaping and the line format.

use logstore::line::{
    boot_detail, clip, escape, format_line, record_hash, unescape, MAX_LINE, TRUNCATED,
};
use logstore::source::{owned_source, source_of, valid_source, KERNEL, SYSTEM};
use logstore::{parse_line, verify, Broken};

#[test]
fn sources_come_from_the_service_segment() {
    assert_eq!(source_of("system/events/service/logd"), "service");
    assert_eq!(source_of("system/events/login/start"), "login");
    assert_eq!(source_of("system/events/clipboard/paste"), "clipboard");
    assert_eq!(source_of("system/health/confd"), "confd");
    assert_eq!(source_of("system/health/summary"), "summary");
    assert_eq!(source_of("system/health/net_d-2"), "net_d-2");
    assert_eq!(source_of("system/events/security/denial"), KERNEL);
    assert_eq!(source_of("system/events/security/clipboard"), "security");
}

#[test]
fn hostile_topics_go_to_system() {
    let long = "a".repeat(200);
    let cases = [
        String::from("system/events/../x"),
        String::from("system/events/A B/x"),
        String::from("system/events/Upper/x"),
        String::from("system/events/a.b/x"),
        String::from("system/events//x"),
        String::from("system/events/x"),
        String::from("system/events/x/"),
        String::from("system/health/"),
        String::from("system/health/a/b"),
        String::from("system/health/.."),
        String::from("system/health/x\ty"),
        format!("system/events/{long}/x"),
        format!("system/health/{long}"),
        String::from("system/events/pkg/install"),
        String::from("system/health/pkg"),
        String::from("other/topic"),
        String::from(""),
    ];
    for topic in cases {
        assert_eq!(source_of(&topic), SYSTEM, "{topic:?}");
    }
    assert_eq!(
        source_of(&format!("system/health/{}", "b".repeat(32))).len(),
        32
    );
}

#[test]
fn source_names_are_checked() {
    assert!(valid_source("a"));
    assert!(valid_source("abc_def-09"));
    assert!(valid_source(&"z".repeat(32)));
    assert!(!valid_source(&"z".repeat(33)));
    assert!(!valid_source(""));
    assert!(!valid_source("A"));
    assert!(!valid_source("a b"));
    assert!(!valid_source("a/b"));
    assert!(!valid_source(".."));
    assert!(!valid_source("é"));
    assert!(valid_source("pkg"));
    assert!(!owned_source("pkg"));
}

#[test]
fn escaping_round_trips() {
    for text in [
        "",
        "plain",
        "a\tb",
        "line\nbreak",
        "back\\slash",
        "\\t literal",
        "\r\n\t\\",
        "ünï",
    ] {
        let escaped = escape(text);
        assert!(!escaped.contains('\t') && !escaped.contains('\n') && !escaped.contains('\r'));
        assert_eq!(unescape(&escaped).as_deref(), Some(text));
    }
    assert_eq!(escape("a\tb\nc\\"), "a\\tb\\nc\\\\");
    assert_eq!(unescape("dangling\\"), None);
    assert_eq!(unescape("bad\\x"), None);
}

#[test]
fn a_line_is_five_fields_that_parse_back() {
    let detail = "user=a\tb\nc";
    let hash = record_hash(7, 3, 9, "system/x", detail);
    let line = format_line(3, 9, "system/x", detail, hash);
    assert!(line.ends_with('\n'));
    assert_eq!(line.matches('\t').count(), 4);
    assert_eq!(line.matches('\n').count(), 1);
    let parsed = parse_line(line.trim_end_matches('\n')).unwrap();
    assert_eq!((parsed.seq, parsed.tick, parsed.hash), (3, 9, hash));
    assert_eq!(parsed.topic, "system/x");
    assert_eq!(parsed.detail, detail);
    assert!(parse_line("1\t2\t3\t4").is_none());
    assert!(parse_line("1\t2\t3\t4\tzz").is_none());
    assert!(parse_line("1\t2\t3\t4\t5\t6").is_none());
}

#[test]
fn long_details_are_clipped_and_still_hash() {
    let detail = "x\t".repeat(4000);
    let clipped = clip(&detail, 100);
    assert!(clipped.ends_with(TRUNCATED));
    assert!(escape(&clipped).len() <= 100);
    let short = clip("short", 100);
    assert_eq!(short, "short");
    let hash = record_hash(0, 1, 1, "t", &clipped);
    let line = format_line(1, 1, "t", &clipped, hash);
    assert!(line.len() <= MAX_LINE);
    // The clip never splits a multi-byte character.
    let wide = clip(&"é".repeat(100), 51);
    assert!(wide.len() <= 51);
}

/// Build a journal text by hand: boot line, then records.
fn journal(boot: u64, cont: u64, records: u64) -> (String, u64) {
    let detail = boot_detail(boot, cont);
    let mut head = record_hash(cont, 0, 1, "boot", &detail);
    let mut text = format_line(0, 1, "boot", &detail, head);
    for seq in 1..=records {
        let detail = format!("n={seq}");
        head = record_hash(head, seq, seq, "system/t", &detail);
        text.push_str(&format_line(seq, seq, "system/t", &detail, head));
    }
    (text, head)
}

#[test]
fn verify_checks_the_chain_per_boot() {
    let (first, _) = journal(1, 0, 5);
    let (second, head) = journal(2, 0, 3);
    let text = format!("{first}\n{second}");
    assert_eq!(verify(&text), Ok(8));
    // A rotated file continues the chain through `cont=`.
    let (third, _) = journal(2, head, 2);
    assert_eq!(verify(&third), Ok(2));
    // Tampering is caught.
    let tampered = text.replacen("n=2", "n=9", 1);
    assert_eq!(verify(&tampered), Err(Broken::Chain(2)));
    // A record before any boot line.
    let orphan = text.split_once('\n').unwrap().1;
    assert_eq!(verify(orphan), Err(Broken::NoBoot(0)));
    // A torn line closed by the next boot's newline is tolerated; a torn line
    // in the middle of a boot is not.
    let torn = format!("{}\n{second}", &first[..first.len() - 25]);
    assert_eq!(verify(&torn), Ok(7));
    let closed = format!("{first}garbage\n{first}");
    assert_eq!(verify(&closed), Ok(10));
    let bad = format!("{first}garbage\n{}", first.split_once('\n').unwrap().1);
    assert_eq!(verify(&bad), Err(Broken::Malformed(6)));
    let bad = format!("{first}garbage\n");
    assert_eq!(verify(&bad), Err(Broken::Malformed(6)));
    // A final line without a newline is a write in progress.
    assert_eq!(verify(&format!("{first}7\t7\tsys")), Ok(5));
}
