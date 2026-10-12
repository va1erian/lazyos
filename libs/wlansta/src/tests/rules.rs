//! The authorization table, request checks, profile keys, SSID display and
//! the merged network list.

use std::format;
use std::string::String;
use std::vec;
use std::vec::Vec;

use super::ap::{Ap, AP, AP2};
use super::*;
use crate::policy::{self, Caller, Denied, SCOPE_SYSTEM, SCOPE_USER};
use crate::profile::{self, Value};
use crate::{ssid, Viewer};

const USER: Caller = Caller { uid: 1000, elevd: false };
const OTHER: Caller = Caller { uid: 1001, elevd: false };
const ELEVD: Caller = Caller { uid: 909, elevd: true };
const ME: Owner = Owner::User(1000);

#[test]
fn only_elevd_adds_or_forgets_system_networks() {
    assert_eq!(policy::add_owner(USER, SCOPE_USER), Ok(Owner::User(1000)));
    assert_eq!(policy::add_owner(USER, SCOPE_SYSTEM), Err(Denied::Perm));
    assert_eq!(policy::add_owner(ELEVD, SCOPE_SYSTEM), Ok(Owner::System));
    assert_eq!(policy::add_owner(ELEVD, SCOPE_USER), Err(Denied::Perm));
    assert_eq!(policy::add_owner(USER, 7), Err(Denied::Invalid));
    assert!(policy::may_forget(ELEVD, Owner::System));
    assert!(!policy::may_forget(USER, Owner::System));
    assert!(policy::may_forget(USER, Owner::User(1000)));
    assert!(!policy::may_forget(OTHER, Owner::User(1000)));
    assert!(!policy::may_forget(ELEVD, Owner::User(1000)));
}

#[test]
fn a_user_network_is_its_owners_and_a_system_one_everyones() {
    assert!(policy::may_use(USER, Owner::System));
    assert!(policy::may_use(OTHER, Owner::System));
    assert!(policy::may_use(USER, Owner::User(1000)));
    assert!(!policy::may_use(OTHER, Owner::User(1000)));
    assert!(!policy::may_use(ELEVD, Owner::User(1000)));
}

#[test]
fn new_networks_need_a_valid_ssid_and_exactly_the_right_secret() {
    use Security::*;
    assert_eq!(policy::check_new(ME, "", "home", Wpa2Psk, Some("password")), Ok(()));
    assert_eq!(policy::check_new(ME, "", "home", Wpa2Psk, Some(&"a".repeat(63))), Ok(()));
    assert_eq!(policy::check_new(ME, "", "home", Wpa2Psk, Some(&"aB".repeat(32))), Ok(()));
    assert_eq!(policy::check_new(ME, "", "cafe", Open, None), Ok(()));
    // UTF-8 passphrases of 8 to 63 octets, as access points take them.
    assert_eq!(policy::check_new(ME, "", "home", Wpa2Psk, Some("caf\u{e9} au lait")), Ok(()));
    assert_eq!(policy::check_new(ME, "", "home", Wpa2Psk, Some(&"\u{e9}".repeat(31))), Ok(()));
    for (ssid, security, secret) in [
        ("", Open, None),
        (&"x".repeat(33)[..], Open, None),
        ("home", Wpa2Psk, None),
        ("home", Wpa2Psk, Some("short")),
        ("home", Wpa2Psk, Some(&"z".repeat(64)[..])),
        ("home", Wpa2Psk, Some(&"g".repeat(64)[..])),
        ("home", Wpa2Psk, Some("tab\tinside")),
        ("home", Wpa2Psk, Some("line\u{85}break")),
        ("home", Wpa2Psk, Some(&"\u{e9}".repeat(32)[..])),
        ("cafe", Open, Some("password")),
        ("x", Unsupported, None),
    ] {
        assert_eq!(policy::check_new(ME, "", ssid, security, secret), Err(Denied::Invalid), "{ssid} {secret:?}");
    }
    // A user never names the id; a system network names the id its secret
    // was stored under by `elevd`, and never carries the passphrase.
    assert_eq!(policy::check_new(ME, HOME, "cafe", Open, None), Err(Denied::Invalid));
    let sys = Owner::System;
    assert_eq!(policy::check_new(sys, HOME, "home", Wpa2Psk, None), Ok(()));
    assert_eq!(policy::check_new(sys, HOME, "cafe", Open, None), Ok(()));
    assert_eq!(policy::check_new(sys, HOME, "home", Wpa2Psk, Some("password")), Err(Denied::Invalid));
    assert_eq!(policy::check_new(sys, "", "home", Wpa2Psk, None), Err(Denied::Invalid));
    assert_eq!(policy::check_new(sys, "office", "home", Wpa2Psk, None), Err(Denied::Invalid));
}

#[test]
fn a_request_names_a_profile_among_those_its_caller_may_use() {
    let mut system = profile(HOME, "home", Security::Wpa2Psk);
    system.owner = Owner::System;
    let mut mine = profile(CAFE, "cafe", Security::Open);
    mine.owner = Owner::User(1000);
    let mut theirs = profile(CAFE, "their cafe", Security::Open);
    theirs.owner = Owner::User(1001);
    let all = vec![system, theirs.clone(), mine.clone()];
    // The same id belongs to two users: each gets their own.
    assert_eq!(policy::resolve(USER, &all, CAFE), Ok(&mine));
    assert_eq!(policy::resolve(OTHER, &all, CAFE), Ok(&theirs));
    assert_eq!(policy::resolve(USER, &all, HOME).map(|p| p.owner), Ok(Owner::System));
    let only_theirs = vec![theirs.clone()];
    assert_eq!(policy::resolve(USER, &only_theirs, CAFE), Err(Denied::Perm));
    assert_eq!(policy::resolve(USER, &all, "w0000000000ff"), Err(Denied::Missing));
    // Forget: your own, never a system network, nothing about others'.
    assert_eq!(policy::resolve_forget(USER, &all, CAFE), Ok(Some(&mine)));
    assert_eq!(policy::resolve_forget(USER, &all, HOME), Err(Denied::Perm));
    assert_eq!(policy::resolve_forget(USER, &only_theirs, CAFE), Ok(None));
    assert_eq!(policy::resolve_forget(ELEVD, &all, HOME).map(|p| p.map(|p| p.owner)), Ok(Some(Owner::System)));
    assert_eq!(policy::resolve_forget(ELEVD, &all, CAFE), Ok(None));
    // Disconnect: a system network or none, anyone; a user's, its owner.
    assert!(policy::may_disconnect(OTHER, None));
    assert!(policy::may_disconnect(OTHER, Some(Owner::System)));
    assert!(policy::may_disconnect(USER, Some(Owner::User(1000))));
    assert!(!policy::may_disconnect(OTHER, Some(Owner::User(1000))));
    assert!(!policy::may_disconnect(ELEVD, Some(Owner::User(1000))));
}

#[test]
fn normalizing_puts_the_system_first_and_caps_each_owner() {
    let mut user_dup = profile(HOME, "evil", Security::Open);
    user_dup.owner = Owner::User(1000);
    let mut user_own = profile(CAFE, "cafe", Security::Open);
    user_own.owner = Owner::User(1000);
    let mut other = profile(CAFE, "cafe", Security::Open);
    other.owner = Owner::User(1001);
    let system = profile(HOME, "home", Security::Wpa2Psk);
    let twice = user_own.clone();
    let kept = profile::normalize(vec![user_dup, user_own.clone(), twice, other.clone(), system.clone()], 8);
    assert_eq!(kept, vec![system, user_own, other], "a system id wins; (owner, id) is unique");
    let many: Vec<Profile> = (0..40u8)
        .map(|i| {
            let mut p = profile(&profile::new_id([0, 0, 0, 0, 0, i]), "x", Security::Open);
            p.owner = Owner::User(1000);
            p
        })
        .collect();
    assert_eq!(profile::normalize(many, profile::MAX_PER_OWNER).len(), profile::MAX_PER_OWNER);
}

#[test]
fn profiles_round_trip_through_their_keys() {
    let mut p = profile(HOME, "home net", Security::Wpa2Psk);
    p.owner = Owner::User(1000);
    p.hidden = true;
    p.priority = 3;
    let fields = profile::fields(&p);
    assert_eq!(fields.last().unwrap().0, "ssid", "the ssid is written last");
    let get = |name: &str| fields.iter().find(|(n, _)| *n == name).map(|(_, v)| v.clone());
    assert_eq!(profile::parse(p.owner, HOME, get), Some(p.clone()));
    assert_eq!(
        profile::key(p.owner, HOME, "ssid"),
        "user/1000/net/wifi/networks/w000000000001/ssid"
    );
    assert_eq!(profile::key(Owner::System, HOME, "hidden"), "sys/net/wifi/networks/w000000000001/hidden");
    // Half-written or tampered profiles are ignored.
    assert_eq!(profile::parse(Owner::System, HOME, |_| None), None);
    let bad_security = |n: &str| match n {
        "ssid" => Some(Value::Str(String::from("x"))),
        "security" => Some(Value::Str(String::from("wep"))),
        _ => None,
    };
    assert_eq!(profile::parse(Owner::System, HOME, bad_security), None);
    assert_eq!(profile::parse(Owner::System, "../x", |_| None), None);
}

#[test]
fn keys_name_their_owner_and_id() {
    let keys: Vec<String> = [
        "sys/net/wifi/networks/w000000000001/ssid",
        "sys/net/wifi/networks/w000000000001/security",
        "user/1000/net/wifi/networks/w0000000000ff/ssid",
        "user/abc/net/wifi/networks/w000000000003/ssid",
        "user/1000/net/wifi/networks/evil/ssid",
        "sys/net/wifi/networks/w000000000004/other",
        "sys/net/wifi/country",
    ]
    .iter()
    .map(|k| String::from(*k))
    .collect();
    assert_eq!(
        profile::ids(&keys, 8),
        vec![
            (Owner::System, String::from("w000000000001")),
            (Owner::User(1000), String::from("w0000000000ff")),
        ]
    );
    assert_eq!(profile::new_id([0, 1, 2, 0xab, 0xcd, 0xef]), "w000102abcdef");
    assert!(profile::valid_id(&profile::new_id([0xFF; 6])));
    assert!(!profile::valid_id("W000000000001"));
}

#[test]
fn the_country_is_two_capitals_or_the_world() {
    assert_eq!(profile::country(Some("FR")), "FR");
    assert_eq!(profile::country(Some("00")), "00");
    for bad in [None, Some(""), Some("fr"), Some("FRA"), Some("1A"), Some("É")] {
        assert_eq!(profile::country(bad), "00");
    }
}

#[test]
fn ssids_are_shown_without_controls_and_within_32_octets() {
    assert_eq!(ssid::display(b"home"), "home");
    assert_eq!(ssid::display(b"\0\0\0"), "");
    assert_eq!(ssid::display(b"a\x1b[2Jb\nc\x7f"), "a[2Jbc");
    assert_eq!(ssid::display("x\u{202E}gnp.exe".as_bytes()), "xgnp.exe");
    assert_eq!(ssid::display(&[0xFF; 32]).chars().count(), 10, "U+FFFD is 3 octets");
    assert!(ssid::display(&[0xFF; 32]).len() <= 32);
    // Bidi marks and isolates, separators, tags and annotations go; the
    // joiners that emoji and scripts need stay.
    for hidden in ["\u{061C}", "\u{200E}", "\u{2028}", "\u{2029}", "\u{2067}", "\u{206A}", "\u{FFF9}", "\u{E0041}", "\u{200B}"] {
        assert_eq!(ssid::display(format!("a{hidden}b").as_bytes()), "ab", "{hidden:?}");
    }
    let family = "\u{1F468}\u{200D}\u{1F469}";
    assert_eq!(ssid::display(family.as_bytes()), family);
    assert_eq!(ssid::display("a\u{200C}b".as_bytes()), "a\u{200C}b");
}

#[test]
fn a_typed_name_matches_raw_octets_first() {
    use crate::ssid::Match;
    assert_eq!(ssid::matches(b"home", b"home"), Some(Match::Exact));
    assert_eq!(ssid::matches(b"home", "home\u{200B}".as_bytes()), Some(Match::Display));
    assert_eq!(ssid::matches(b"home", b"house"), None);
    assert_eq!(ssid::matches(b"", b"\0\0"), None, "nothing names a hidden network");
    let plain = Ap::open(AP, "home");
    let mut twin = Ap::open(AP2, "home\u{200B}");
    twin.rssi = -30;
    let mut h = H::new(vec![]);
    let actions = h.sta.scan(h.now, vec![]).unwrap();
    h.answer_scan(actions, &[&plain, &twin]);
    assert_eq!(h.sta.heard_ssid(b"home"), Ok(Some(b"home".to_vec())), "the exact name wins");
    let mut h = H::new(vec![]);
    let actions = h.sta.scan(h.now, vec![]).unwrap();
    h.answer_scan(actions, &[&twin]);
    assert_eq!(h.sta.heard_ssid(b"home"), Ok(Some("home\u{200B}".as_bytes().to_vec())));
    let other = Ap::open(AP, "home\u{2060}");
    let actions = h.sta.scan(h.now, vec![]).unwrap();
    h.answer_scan(actions, &[&twin, &other]);
    assert_eq!(h.sta.heard_ssid(b"home"), Err(crate::Ambiguous));
    assert_eq!(h.sta.heard_ssid(b"absent"), Ok(None));
}

#[test]
fn networks_merge_bsses_and_show_only_usable_ids() {
    let near = Ap::open(AP, "cafe");
    let mut far = Ap::open(AP2, "cafe");
    far.rssi = -90;
    let mut mine = profile(CAFE, "cafe", Security::Open);
    mine.owner = Owner::User(1000);
    let mut h = H::new(vec![mine]);
    let actions = h.sta.scan(h.now, vec![]).unwrap();
    h.answer_scan(actions, &[&far, &near]);
    let listed = h.sta.networks(Viewer::User(1000));
    assert_eq!(listed.len(), 1);
    let n = &listed[0];
    assert_eq!((n.ssid.as_str(), n.bss_count, n.rssi_dbm, n.freq_mhz), ("cafe", 2, -50, 2437));
    assert_eq!(n.id, CAFE);
    assert!(!n.connected);
    assert_eq!(h.sta.networks(Viewer::User(1001))[0].id, "");
    assert_eq!(h.sta.networks(Viewer::System)[0].id, "");
}

#[test]
fn frequencies_follow_the_band() {
    assert_eq!(Band::Ghz2.freq_mhz(1), 2412);
    assert_eq!(Band::Ghz2.freq_mhz(14), 2484);
    assert_eq!(Band::Ghz5.freq_mhz(36), 5180);
    assert_eq!(Band::Ghz6.freq_mhz(1), 5955);
    assert_eq!(Band::Ghz2.freq_mhz(0), 0);
    assert_eq!(Band::Ghz2.freq_mhz(15), 0);
}
