use super::*;
use crate::filter::{parse_line, Line};

fn hiding(line: &str) -> Result<Hiding, DropReason> {
    match parse_line(line) {
        Line::Hide(h) => Hiding::new(&h),
        _ => panic!("not plain element hiding: {line}"),
    }
}

/// The rules the lines become, as `(selector, if-domain, unless-domain)`, and the stats.
/// A rule as `(selector, if-domain, unless-domain)`.
type Seen = (String, Vec<String>, Vec<String>);

fn convert(lines: &[&str]) -> (Vec<Seen>, Stats) {
    let mut stats = Stats::default();
    let parsed = lines.iter().map(|l| hiding(l).expect(l)).collect();
    let out = rules(parsed, &[], &mut stats)
        .into_iter()
        .map(|r| (r.selector.unwrap(), r.if_domain, r.unless_domain))
        .collect();
    (out, stats)
}

#[test]
fn plain_selectors_are_taken() {
    for s in [
        ".ad",
        "#sidebar-ad",
        "div.ad-wrapper",
        "a[href^=\"https://ads.example/\"]",
        "a[href*='/click?']",
        "[data-ad-slot]",
        "div[id=banner] > .inner + p ~ span",
        ".a .b   .c",
        ".ad.card",
        "*.ad",
        ".-x-ad",
        "html .ad",
    ] {
        assert!(valid_selector(s), "{s}");
    }
}

#[test]
fn what_webkit_or_the_list_could_mean_differently_is_refused() {
    for s in [
        "",
        "div",
        "*",
        "body",
        ".ad body",
        "html > body",
        ".ad:hover",
        ".ad:not(.x)",
        "div:has(> .ad)",
        ".ad, .banner",
        ".ad::before",
        ".a\\:b",
        "#1abc",
        ".caf\u{e9}",
        "a[href=\"x\\\"y\"]",
        "a[href=1]",
        "a[href^=]",
        ".ad >",
        "> .ad",
        ".ad > > .x",
        ".ad.",
        "[",
        "div[a=\"b\"",
        "+js(abort-on-property-read, x)",
    ] {
        assert!(!valid_selector(s), "{s:?}");
    }
    assert!(!valid_selector(&format!(".{}", "a".repeat(MAX_SELECTOR))));
}

#[test]
fn domains_become_if_domain_and_unless_domain() {
    let (out, stats) = convert(&[
        "##.everywhere",
        "a.com,b.org##.listed",
        "~c.net,~d.net##.except",
    ]);
    assert_eq!(stats.hides, 3);
    assert_eq!(
        out,
        [
            (".everywhere".to_owned(), vec![], vec![]),
            (
                ".except".into(),
                vec![],
                vec!["*c.net".into(), "*d.net".into()]
            ),
            (
                ".listed".into(),
                vec!["*a.com".into(), "*b.org".into()],
                vec![]
            ),
        ]
    );
}

#[test]
fn a_mix_of_included_and_excluded_domains_is_dropped() {
    let h = parse_line("a.com,~b.a.com##.ad");
    let Line::Hide(h) = h else { panic!() };
    assert_eq!(Hiding::new(&h), Err(DropReason::MixedDomains));
    for line in [
        "tripadvisor.*##.ad",
        "~tripadvisor.*##.ad",
        "a.com#@#.ad:hover",
        "a.com##.ad:hover",
        "a.com##div",
    ] {
        assert_eq!(hiding(line), Err(DropReason::Cosmetic), "{line}");
    }
}

#[test]
fn selectors_with_one_trigger_share_a_rule_in_chunks() {
    let lines: Vec<String> = (0..SELECTORS_PER_RULE + 6)
        .map(|i| format!("x.com##.ad{i}"))
        .collect();
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    let (out, stats) = convert(&refs);
    assert_eq!(stats.hides, SELECTORS_PER_RULE + 6);
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].0.split(", ").count(), SELECTORS_PER_RULE);
    assert!(out.iter().all(|r| r.1 == ["*x.com"]));
}

#[test]
fn a_repeated_line_hides_once() {
    let (out, stats) = convert(&["x.com##.ad", "x.com##.ad", "##.ad"]);
    assert_eq!((out.len(), stats.hides), (2, 2));
}

#[test]
fn an_exception_takes_a_selector_off_the_domains_it_names() {
    // Generic: the exception is an `unless-domain`.
    let (out, _) = convert(&["##.ad", "a.com#@#.ad"]);
    assert_eq!(out, [(".ad".to_owned(), vec![], vec!["*a.com".to_owned()])]);

    // Listed: the domain leaves the list, and the rule goes with its last domain.
    let (out, _) = convert(&["a.com,b.com##.ad", "a.com#@#.ad"]);
    assert_eq!(out, [(".ad".to_owned(), vec!["*b.com".to_owned()], vec![])]);
    let (out, stats) = convert(&["a.com##.ad", "a.com#@#.ad", "b.com##.other"]);
    assert_eq!(out.len(), 1);
    assert_eq!((out[0].0.as_str(), stats.hides), (".other", 1));

    // Only the exception's own selector is touched, and subdomains follow the parent.
    let (out, _) = convert(&["a.com##.ad", "a.com##.other", "www.a.com#@#.ad"]);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].0, ".other");
}

#[test]
fn an_exception_that_cannot_be_carved_out_hides_less() {
    // `#@#` everywhere cancels the selector everywhere.
    let (out, _) = convert(&["a.com##.ad", "##.ad", "#@#.ad"]);
    assert!(out.is_empty());
    // Everywhere but x.com: not expressible next to a hide, so the selector goes.
    let (out, _) = convert(&["##.ad", "~x.com#@#.ad"]);
    assert!(out.is_empty());
}

#[test]
fn an_exception_for_a_selector_nobody_hides_changes_nothing() {
    let (out, stats) = convert(&["a.com##.ad", "a.com#@#.nothing"]);
    assert_eq!(
        (out.len(), stats.hides, stats.dropped_exceptions),
        (1, 1, 0)
    );
}

fn site_off(line: &str) -> Option<SiteOff> {
    match parse_line(line) {
        Line::Network(f) => SiteOff::new(&f),
        _ => panic!("not a network filter: {line}"),
    }
}

#[test]
fn a_site_level_hiding_exception_is_read_exactly_or_not_at_all() {
    let off = site_off("@@||News.Example^$generichide").unwrap();
    assert_eq!((off.site.as_str(), off.all), ("*news.example", false));
    assert!(site_off("@@||a.example^$elemhide").unwrap().all);
    for line in [
        "||a.example^$generichide",
        "@@||a.example/path$generichide",
        "@@||a.example^$generichide,script",
        "@@||a.example^$~generichide",
        "@@||a.example^",
        "@@://localhost/$generichide",
    ] {
        assert_eq!(site_off(line), None, "{line}");
    }
}

#[test]
fn generichide_spares_a_site_the_generic_rules_only() {
    let mut stats = Stats::default();
    let lines = ["##.ad", "a.example##.ad2", "b.example##.ad3"]
        .iter()
        .map(|l| hiding(l).unwrap())
        .collect();
    let off = site_off("@@||a.example^$generichide").unwrap();
    let out = rules(lines, &[off], &mut stats);
    let seen: Vec<_> = out
        .iter()
        .map(|r| {
            (
                r.selector.clone().unwrap(),
                r.if_domain.clone(),
                r.unless_domain.clone(),
            )
        })
        .collect();
    assert_eq!(
        seen,
        [
            (".ad".to_owned(), vec![], vec!["*a.example".to_owned()]),
            (".ad2".into(), vec!["*a.example".into()], vec![]),
            (".ad3".into(), vec!["*b.example".into()], vec![]),
        ]
    );
}

#[test]
fn elemhide_spares_a_site_every_rule() {
    let mut stats = Stats::default();
    let lines = ["##.ad", "a.example,b.example##.ad2"]
        .iter()
        .map(|l| hiding(l).unwrap())
        .collect();
    let off = site_off("@@||a.example^$elemhide").unwrap();
    let out = rules(lines, &[off], &mut stats);
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].unless_domain, ["*a.example"]);
    assert_eq!(out[1].if_domain, ["*b.example"]);
}

#[test]
fn a_wildcard_domain_is_left_out_of_a_hiding_line_and_never_out_of_an_exception() {
    let (out, _) = convert(&["a.com,tripadvisor.*,b.com##.ad"]);
    assert_eq!(out[0].1, ["*a.com", "*b.com"]);
    assert_eq!(
        hiding("a.com,tripadvisor.*#@#.ad"),
        Err(DropReason::Cosmetic)
    );
    assert_eq!(hiding("~tripadvisor.*##.ad"), Err(DropReason::Cosmetic));
}
