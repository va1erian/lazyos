use super::*;

const SOURCE: &str = include_str!("../../../assets/terminfo/xterm-256color.src");

#[test]
fn the_table_sizes_are_the_legacy_ones() {
    assert_eq!((BOOLS.len(), NUMBERS.len(), STRINGS.len()), (44, 39, 414));
}

#[test]
fn known_capabilities_have_their_term_h_indexes() {
    // Spot checks against ncurses' term.h, so a slip in the tables shows here.
    let at = |name| STRINGS.iter().position(|c| *c == name).unwrap();
    assert_eq!(
        [at("cbt"), at("cup"), at("kbs"), at("kf1"), at("kcuu1"), at("khome"), at("kend")],
        [0, 10, 55, 66, 87, 76, 164]
    );
    assert_eq!([at("setaf"), at("setab"), at("kmous"), at("kf63"), at("el1")], [359, 360, 355, 268, 269]);
    assert_eq!([at("kf10"), at("kf2"), at("kf9")], [67, 68, 75], "kf10 sorts after kf1");
    assert_eq!(BOOLS.iter().position(|c| *c == "xenl"), Some(4));
    assert_eq!(BOOLS.iter().position(|c| *c == "bce"), Some(28));
    assert_eq!(NUMBERS.iter().position(|c| *c == "colors"), Some(13));
    assert_eq!(NUMBERS.iter().position(|c| *c == "pairs"), Some(14));
}

#[test]
fn the_entry_round_trips_through_the_compiled_form() {
    let entry = parse_source(SOURCE).unwrap();
    let bytes = compile(&entry);
    assert_eq!(read_compiled(&bytes).unwrap(), entry);
    // A second compile is byte-identical: the build is reproducible.
    assert_eq!(compile(&entry), bytes);
    assert_eq!(&bytes[..2], &[0x1a, 0x01], "the legacy magic, little-endian");
}

#[test]
fn the_entry_says_what_the_terminal_is() {
    let entry = read_compiled(&compile_source(SOURCE).unwrap()).unwrap();
    assert!(entry.names.starts_with("xterm-256color|"));
    assert!(entry.flag("am") && entry.flag("xenl") && entry.flag("bce") && entry.flag("msgr"));
    assert!(!entry.flag("mir"), "no insert mode");
    assert_eq!(entry.number("colors"), Some(256));
    assert_eq!(entry.number("pairs"), Some(32767));
    assert_eq!(entry.number("cols"), Some(80));
    assert_eq!(entry.string("cup"), Some(&b"\x1b[%i%p1%d;%p2%dH"[..]));
    assert_eq!(entry.string("kcuu1"), Some(&b"\x1bOA"[..]));
    assert_eq!(entry.string("kbs"), Some(&b"\x7f"[..]));
    assert_eq!(entry.string("kf12"), Some(&b"\x1b[24~"[..]));
    assert_eq!(entry.string("setaf").map(|s| s.starts_with(b"\x1b[")), Some(true));
    assert_eq!(entry.string("acsc"), None, "no alternate character set");
}

#[test]
fn escapes_in_source_text() {
    let escapes = r"\E[\s\n\r\t\b\f\^\\\,\:^A^?\101";
    let entry = parse_source(&format!("t|test,\n\tbel={escapes},\n")).unwrap();
    assert_eq!(entry.string("bel"), Some(&b"\x1b[ \n\r\t\x08\x0c^\\,:\x01\x7fA"[..]));
    let err = parse_source(&format!("t|test,\n\ta={escapes},\n")).unwrap_err();
    assert!(err.contains("unknown string"), "{err}");
}

#[test]
fn bad_source_is_refused_not_skipped() {
    let bad: [(&str, &str); 12] = [
        ("", "no names"),
        ("t|x,\n\tnosuchcap,\n", "unknown boolean"),
        ("t|x,\n\tnosuch#1,\n", "unknown number"),
        ("t|x,\n\tcols#99999,\n", "too large"),
        ("t|x,\n\tcols#-1,\n", "negative"),
        ("t|x,\n\tcols#abc,\n", "not a number"),
        ("t|x,\n\tbel=\\Q,\n", "unknown escape"),
        ("t|x,\n\tbel=\\0,\n", "NUL"),
        ("t|x,\n\tbel=\\400,\n", "octal over 255"),
        ("t|x,\n\tbel=abc\\", "a trailing backslash"),
        ("t|x,\n\tbel=^", "a trailing caret"),
        ("t|x,\nother|y,\n", "two entries"),
    ];
    for (source, why) in bad {
        assert!(parse_source(source).is_err(), "{why}");
    }
}

#[test]
fn hostile_compiled_input_is_an_error_never_a_panic() {
    let good = compile_source(SOURCE).unwrap();
    assert!(read_compiled(&[]).is_err());
    assert!(read_compiled(&[0x1a]).is_err());
    assert!(read_compiled(&[0xff; 64]).is_err(), "wrong magic");
    for cut in [2, 11, 12, 20, 60, good.len() - 1] {
        assert!(read_compiled(&good[..cut]).is_err(), "cut at {cut}");
    }
    // Every single-byte corruption is handled without a panic.
    for at in 0..good.len() {
        let mut bad = good.clone();
        bad[at] ^= 0xa5;
        let _ = read_compiled(&bad);
    }
    // A negative count is refused.
    let mut bad = good.clone();
    bad[4] = 0xff;
    bad[5] = 0xff;
    assert!(read_compiled(&bad).is_err());
}

#[test]
fn an_entry_with_nothing_set_compiles_to_a_valid_empty_one() {
    let entry = parse_source("t|x,\n").unwrap();
    assert_eq!(read_compiled(&compile(&entry)).unwrap(), entry);
    // Odd name length: the boolean section is padded to an even offset.
    let one = parse_source("t|xy,\n\tam,\n").unwrap();
    let bytes = compile(&one);
    assert_eq!(read_compiled(&bytes).unwrap(), one);
    assert_eq!(bytes.len() % 2, 0);
}

/// The entry as ncurses' `tic` (6.4) compiles the same source: our own source
/// through the reference compiler (`tic -o DIR assets/terminfo/xterm-256color.src`),
/// kept so a regression in the writer is a failing test, not a broken `vi`.
const TIC_REFERENCE: &[u8] = include_bytes!("../tests/fixtures/xterm-256color.tic");

#[test]
fn the_writer_matches_ncurses_tic_byte_for_byte() {
    let ours = compile_source(SOURCE).unwrap();
    assert_eq!(ours.len(), TIC_REFERENCE.len(), "same size as tic's");
    assert!(ours == TIC_REFERENCE, "the compiled entry differs from tic's");
    // And the reader agrees with the reference's content.
    assert_eq!(read_compiled(TIC_REFERENCE).unwrap(), parse_source(SOURCE).unwrap());
}
