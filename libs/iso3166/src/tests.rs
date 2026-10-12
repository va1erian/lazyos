use super::*;

#[test]
fn the_table_is_sorted_complete_and_plain() {
    assert_eq!(COUNTRIES.len(), 249);
    assert!(COUNTRIES.windows(2).all(|w| w[0].0 < w[1].0));
    for (code, name) in COUNTRIES {
        assert_eq!(code.len(), 2, "{code}");
        assert!(code.bytes().all(|b| b.is_ascii_uppercase()), "{code}");
        assert!(!name.is_empty() && name.len() <= 48, "{code}");
        assert!(
            name.bytes().all(|b| (0x20..0x7f).contains(&b)),
            "{code}: {name}"
        );
    }
}

#[test]
fn assigned_codes_are_countries_and_made_up_ones_are_not() {
    for code in [b"US", b"FR", b"JP", b"DE", b"GB", b"AD", b"ZW", b"SS"] {
        assert!(is_country(code), "{code:?}");
    }
    for code in [b"ZZ", b"UK", b"EU", b"XK", b"AA", b"00", b"QQ", b"us"] {
        assert!(!is_country(code), "{code:?}");
    }
}

#[test]
fn names_are_looked_up_by_code() {
    assert_eq!(name(b"FR"), Some("France"));
    assert_eq!(name(b"US"), Some("United States"));
    assert_eq!(name(b"00"), None);
    assert_eq!(display_name(b"00"), Some("World"));
    assert_eq!(display_name(b"ZZ"), None);
}

#[test]
fn parse_accepts_world_and_countries_in_any_case() {
    assert_eq!(parse("00"), Some(*b"00"));
    assert_eq!(parse("fr"), Some(*b"FR"));
    assert_eq!(parse("Fr"), Some(*b"FR"));
    for bad in ["", "F", "FRA", "ZZ", "0", "000", " F", "F\u{e9}", "\0\0"] {
        assert_eq!(parse(bad), None, "{bad:?}");
    }
}
