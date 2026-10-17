//! The converter against an independent Adblock Plus matcher: for random patterns and
//! URLs, the emitted WebKit URL filter must match exactly when the filter does.
//!
//! Not "exactly" in one place, on purpose: a trailing `^` also matches the end of the
//! address in Adblock Plus and only a separator character in the converter, so the
//! reference matcher is told the converter's rule (`end_sep_ok = false`), and a second
//! check pins the direction of the difference (the converter never blocks more).

mod common;

use common::{abp_matches, fuzz_cases, fuzz_seed, parse_url_filter, Rng};
use lazyadblock::{convert, Options};
use serde_json::Value;

/// The `url-filter` the converter makes for one pattern with no options, or None when
/// the pattern was dropped.
fn converted(pattern: &str) -> Option<String> {
    let out = convert(pattern, &Options::default());
    let list = out.lists.first()?;
    let rules: Vec<Value> = serde_json::from_str(&list.json).unwrap();
    Some(
        rules[0]["trigger"]["url-filter"]
            .as_str()
            .unwrap()
            .to_owned(),
    )
}

const WORDS: [&str; 16] = [
    "ads",
    "banner",
    "example.com",
    "ad.js",
    "track",
    "pixel.gif",
    "/",
    "/ads/",
    "?id=",
    "&x=",
    "-",
    "_",
    "com",
    "a.b.example.com",
    ".",
    "%20",
];

fn random_pattern(rng: &mut Rng) -> String {
    let mut p = String::new();
    match rng.below(4) {
        0 => p.push_str("||"),
        1 => p.push('|'),
        _ => {}
    }
    for i in 0..1 + rng.below(4) {
        if i > 0 && rng.chance(40) {
            p.push(if rng.chance(50) { '*' } else { '^' });
        }
        p.push_str(rng.pick(&WORDS));
    }
    match rng.below(6) {
        0 => p.push('^'),
        1 => p.push('|'),
        2 => p.push('*'),
        _ => {}
    }
    unambiguous(p)
}

/// A pattern that starts and ends with `/` is a regular expression in Adblock Plus, not
/// a path: not what these generators mean, so a `*` ends it.
fn unambiguous(mut p: String) -> String {
    if p.len() >= 2 && p.starts_with('/') && p.ends_with('/') {
        p.push('*');
    }
    p
}

const HOSTS: [&str; 8] = [
    "example.com",
    "ads.example.com",
    "a.b.example.com",
    "notexample.com",
    "example.com.evil.net",
    "cdn.site.org",
    "site.org",
    "Example.COM",
];
const SEGMENTS: [&str; 12] = [
    "ads",
    "banner",
    "ad.js",
    "track",
    "pixel.gif",
    "index.html",
    "x",
    "a-b",
    "Ad.JS",
    "%20",
    "img",
    "ads.example.com",
];

fn random_url(rng: &mut Rng) -> String {
    let mut u = String::new();
    u.push_str(if rng.chance(70) {
        "https://"
    } else {
        "http://"
    });
    u.push_str(rng.pick(&HOSTS));
    if rng.chance(20) {
        u.push_str(":8080");
    }
    u.push('/');
    for i in 0..rng.below(4) {
        if i > 0 {
            u.push('/');
        }
        u.push_str(rng.pick(&SEGMENTS));
    }
    if rng.chance(40) {
        u.push('?');
        u.push_str(rng.pick(&[
            "id=1",
            "x=ads",
            "u=http://example.com/ads",
            "a=b&c=d",
            "ad.js",
        ]));
    }
    u
}

#[test]
fn urls_match_exactly_when_the_filter_does() {
    let seed = fuzz_seed(0x5eed_ad_b10c);
    let mut rng = Rng(seed);
    let (mut dropped, mut compared, mut matched) = (0, 0, 0);
    for _ in 0..fuzz_cases(3000) {
        let pattern = random_pattern(&mut rng);
        let Some(filter) = converted(&pattern) else {
            dropped += 1;
            continue;
        };
        let re = parse_url_filter(&filter, false).unwrap_or_else(|e| {
            panic!("seed {seed:#x}: WebKit would refuse {filter:?} (from {pattern:?}): {e}")
        });
        for _ in 0..12 {
            let url = random_url(&mut rng);
            let got = re.is_match(&url);
            let want = abp_matches(&pattern, &url, false, false);
            let real = abp_matches(&pattern, &url, false, true);
            assert_eq!(
                got, want,
                "seed {seed:#x}: pattern {pattern:?} -> {filter:?} on {url:?}: converter {got}, reference {want}"
            );
            assert!(!got || real, "over-blocking: {pattern:?} on {url:?}");
            compared += 1;
            matched += usize::from(got);
        }
    }
    // The generator must reach both outcomes, or the check is empty.
    assert!(
        compared > 1000 && matched > 50 && matched < compared,
        "{compared} compared, {matched} matched"
    );
    assert!(dropped < compared / 4, "{dropped} dropped");
}

/// A pattern cut out of `url`, with separators and runs turned into `^` and `*`, and
/// anchors where the cut starts at one. Such a pattern matches `url` more often than
/// not, so the near misses (`url` with one character changed) are the interesting part.
fn pattern_from(rng: &mut Rng, url: &str) -> String {
    let b = url.as_bytes();
    let host_start = url.find("://").unwrap() + 3;
    let a = rng.below(b.len() - 1);
    let z = a + 1 + rng.below((b.len() - a).min(14));
    let mut p = String::new();
    let at_label = a == host_start || (a > host_start && b[a - 1] == b'.');
    if a == 0 && rng.chance(60) {
        p.push('|');
    } else if at_label && rng.chance(60) {
        p.push_str("||");
    }
    for &c in &b[a..z] {
        if matches!(c, b'/' | b'?' | b'&' | b'=' | b':') && rng.chance(50) {
            p.push('^');
        } else if rng.chance(8) {
            p.push('*');
        } else if matches!(c, b'*' | b'^' | b'|' | b'$') {
            // The URL generator never makes these; a literal here would be a pattern operator.
        } else {
            p.push(c as char);
        }
    }
    if z == b.len() && rng.chance(50) {
        p.push('|');
    }
    unambiguous(p)
}

fn one_char_off(rng: &mut Rng, url: &str) -> String {
    let mut b = url.as_bytes().to_vec();
    // The scheme, `://` and the host's first character stay: a request URL has a host.
    let from = url.find("://").unwrap() + 4;
    let i = from + rng.below(b.len() - from);
    b[i] = *rng.pick(b"abcxyz019-_.%/?&=:~#");
    String::from_utf8(b).unwrap()
}

#[test]
fn patterns_cut_from_urls_agree_on_near_misses() {
    let seed = fuzz_seed(0x00c0_ffee);
    let mut rng = Rng(seed);
    let (mut compared, mut matched) = (0, 0);
    for _ in 0..fuzz_cases(3000) {
        let url = random_url(&mut rng);
        let pattern = pattern_from(&mut rng, &url);
        let Some(filter) = converted(&pattern) else {
            continue;
        };
        let re = parse_url_filter(&filter, false).unwrap_or_else(|e| {
            panic!("seed {seed:#x}: WebKit would refuse {filter:?} (from {pattern:?}): {e}")
        });
        let mut candidates = vec![url.clone()];
        for _ in 0..10 {
            candidates.push(one_char_off(&mut rng, &url));
        }
        for candidate in candidates {
            let got = re.is_match(&candidate);
            let want = abp_matches(&pattern, &candidate, false, false);
            assert_eq!(
                got, want,
                "seed {seed:#x}: pattern {pattern:?} -> {filter:?} on {candidate:?}: converter {got}, reference {want}"
            );
            assert!(
                !got || abp_matches(&pattern, &candidate, false, true),
                "over-blocking"
            );
            compared += 1;
            matched += usize::from(got);
        }
    }
    // Both sides of the boundary must be populated, or the near misses are not near.
    assert!(
        matched > compared / 10 && matched < compared * 9 / 10,
        "{compared} compared, {matched} matched"
    );
}

#[test]
fn the_anchors_behave_on_known_cases() {
    // (pattern, url, matches)
    let cases = [
        ("||example.com^", "https://example.com/", true),
        ("||example.com^", "https://ads.example.com/x", true),
        ("||example.com^", "https://notexample.com/", false),
        ("||example.com^", "https://example.com.evil.net/", false),
        (
            "||example.com^",
            "https://site.org/?u=http://example.com/",
            false,
        ),
        ("||example.com/ads", "http://a.b.example.com/ads/1", true),
        ("|http://example.com", "http://example.com/x", true),
        ("|http://example.com", "https://example.com/x", false),
        ("ads.js|", "https://site.org/ads.js", true),
        ("ads.js|", "https://site.org/ads.js?x=1", false),
        ("/ads/*/banner", "https://site.org/ads/1/banner.png", true),
        ("banner^", "https://site.org/banner?x", true),
        ("banner^", "https://site.org/banners", false),
        ("-ad-", "https://site.org/a-ad-b", true),
    ];
    for (pattern, url, expect) in cases {
        let filter = converted(pattern).unwrap();
        let re = parse_url_filter(&filter, false).unwrap();
        assert_eq!(re.is_match(url), expect, "{pattern} on {url} ({filter})");
        assert_eq!(
            abp_matches(pattern, url, false, false),
            expect,
            "reference: {pattern} on {url}"
        );
    }
}

#[test]
fn case_sensitivity_follows_match_case() {
    let out = convert("/AdFrame$match-case\n", &Options::default());
    let rules: Vec<Value> = serde_json::from_str(&out.lists[0].json).unwrap();
    let t = &rules[0]["trigger"];
    let re = parse_url_filter(t["url-filter"].as_str().unwrap(), true).unwrap();
    assert!(re.is_match("https://x.org/AdFrame"));
    assert!(!re.is_match("https://x.org/adframe"));
    let insensitive = parse_url_filter("adframe", false).unwrap();
    assert!(insensitive.is_match("https://x.org/ADFRAME"));
}

#[test]
fn regex_filters_in_the_subset_match_like_regexes() {
    let filter = converted(r"/ad[0-9]+\.js$/").unwrap();
    let re = parse_url_filter(&filter, false).unwrap();
    assert!(re.is_match("https://x.org/ad12.js"));
    assert!(!re.is_match("https://x.org/ad.js"));
    assert!(!re.is_match("https://x.org/ad12.js?v=2"));
    let filter = converted(r"/^https?:\/\/[a-z]+\.example\/ads/").unwrap();
    let re = parse_url_filter(&filter, false).unwrap();
    assert!(re.is_match("https://cdn.example/ads/1"));
    assert!(!re.is_match("https://cdn.example/news/ads/1"));
}
