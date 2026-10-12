//! `net.wifi.add` and `net.wifi.forget` (`wifi.rs`).

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::*;

fn parse(op: &str, items: &[&str]) -> Result<Operation, &'static str> {
    let args: Vec<String> = items.iter().map(|item| item.to_string()).collect();
    Operation::parse(op, &args)
}

#[test]
fn wifi_add_is_checked_and_never_shows_the_passphrase() {
    let raw = "f42c6fc52df0ebef9ebb4b90b38a5f902e83fe1b135a70e23aed762e9710a12e";
    for good in [
        &["LazyWPA2", "wpa2", "0", "lazy-wpa2-pass"][..],
        &["raw", "wpa2", "0", raw][..],
        &["Caf\u{e9}", "wpa2", "0", "caf\u{e9} au lait"][..],
        &["Open", "open", "1", ""][..],
        &[&"x".repeat(32), "open", "0", ""][..],
    ] {
        let op = parse("net.wifi.add", good).unwrap();
        assert_eq!(op.class(), Class::Once, "a change asks every time");
        assert!(op.permitted().is_ok());
        let summary = op.summary();
        assert!(
            !summary.contains("lazy-wpa2-pass") && !summary.contains(raw),
            "{summary}"
        );
        assert!(!summary.contains("au lait"), "{summary}");
    }
    let op = parse("net.wifi.add", &["LazyWPA2", "wpa2", "0", "lazy-wpa2-pass"]).unwrap();
    assert_eq!(
        op.summary(),
        "Save the Wi-Fi network \"LazyWPA2\" (WPA2 password) for all users, before login too"
    );
    let op = parse("net.wifi.add", &["Hid", "open", "1", ""]).unwrap();
    assert_eq!(
        op.summary(),
        "Save the hidden Wi-Fi network \"Hid\" (open) for all users, before login too"
    );
}

#[test]
fn wifi_add_refuses_what_wlanmd_or_keyd_would() {
    let long = "x".repeat(33);
    let short = "x".repeat(7);
    let too_long = "x".repeat(64);
    for bad in [
        &[][..],
        &["LazyWPA2", "wpa2", "0"][..],
        &["", "open", "0", ""][..],
        &[&long, "open", "0", ""][..],
        &["a\nb", "open", "0", ""][..],
        &["a\u{202e}b", "open", "0", ""][..],
        &["LazyWPA2", "wpa3", "0", "lazy-wpa2-pass"][..],
        &["LazyWPA2", "wpa2", "yes", "lazy-wpa2-pass"][..],
        &["LazyWPA2", "wpa2", "0", ""][..],
        &["LazyWPA2", "wpa2", "0", &short][..],
        &["LazyWPA2", "wpa2", "0", &too_long][..],
        &["LazyWPA2", "wpa2", "0", "bad\npassword"][..],
        &["Open", "open", "0", "a-password"][..],
    ] {
        assert!(parse("net.wifi.add", bad).is_err(), "{bad:?} was accepted");
    }
    // Five arguments is more than any operation takes.
    assert!(parse("net.wifi.add", &["a", "open", "0", "", "x"]).is_err());
}

#[test]
fn wifi_forget_takes_only_an_id_wlanmd_makes() {
    assert!(parse("net.wifi.forget", &["wabcdef012345"]).is_ok());
    let op = parse("net.wifi.forget", &["w0123456789ab"]).unwrap();
    assert_eq!(op.class(), Class::Once);
    assert_eq!(
        op.summary(),
        "Forget the Wi-Fi network 'w0123456789ab' kept for all users"
    );
    for bad in [
        &[][..],
        &["w0123456789AB"][..],
        &["w0123456789a"][..],
        &["x0123456789ab"][..],
        &["w0123456789abc"][..],
        &["../conf"][..],
        &["w0123456789ab", "extra"][..],
    ] {
        assert!(
            parse("net.wifi.forget", bad).is_err(),
            "{bad:?} was accepted"
        );
    }
}
