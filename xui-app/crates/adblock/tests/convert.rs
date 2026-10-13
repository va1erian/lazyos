//! Conversion cases: filter text in, the exact rule JSON out, and what each kind of
//! line counts as. The sample lists are written for these tests (EasyList's text is
//! licensed and is not reproduced here).

mod common;

use common::check_list;
use lazyadblock::{convert, DropReason, Options, WEBKIT_MAX_RULES};
use serde_json::{json, Value};

fn rules(source: &str) -> Vec<Value> {
    let out = convert(source, &Options::default());
    assert!(out.lists.len() <= 1, "unexpected split");
    match out.lists.first() {
        Some(list) => check_list(&list.json, WEBKIT_MAX_RULES),
        None => Vec::new(),
    }
}

fn one(source: &str) -> Value {
    let mut r = rules(source);
    assert_eq!(r.len(), 1, "{source}: {r:?}");
    r.remove(0)
}

const DEFAULT_TYPES: [&str; 8] = [
    "child-document",
    "image",
    "style-sheet",
    "script",
    "font",
    "svg-document",
    "media",
    "raw",
];

#[test]
fn a_domain_rule() {
    assert_eq!(
        one("||ads.example.com^"),
        json!({
            "trigger": {
                "url-filter": r"^[^:]+://+([^:/?#]+\.)?ads\.example\.com[^a-zA-Z0-9_.%-]",
                "resource-type": DEFAULT_TYPES,
            },
            "action": { "type": "block" }
        })
    );
}

#[test]
fn types_party_and_domains() {
    assert_eq!(
        one("/banners/*$script,image,third-party,domain=news.example|blog.example"),
        json!({
            "trigger": {
                "url-filter": "/banners/",
                "resource-type": ["image", "script", "svg-document"],
                "load-type": ["third-party"],
                "if-domain": ["*news.example", "*blog.example"],
            },
            "action": { "type": "block" }
        })
    );
    let r = one("tracker.js$~third-party,domain=~shop.example");
    assert_eq!(r["trigger"]["load-type"], json!(["first-party"]));
    assert_eq!(r["trigger"]["unless-domain"], json!(["*shop.example"]));
}

#[test]
fn match_case_sets_the_flag() {
    let r = one("/AdFrame.$match-case");
    assert_eq!(r["trigger"]["url-filter"], json!("/AdFrame\\."));
    assert_eq!(r["trigger"]["url-filter-is-case-sensitive"], json!(true));
}

#[test]
fn exceptions_follow_the_blocks_and_important_comes_last() {
    let source = "\
@@||cdn.example.com/ok.js\n\
||example.com^$important\n\
||example.com/ads/\n\
||other.example/pixel.gif\n";
    let r = rules(source);
    let actions: Vec<&str> = r
        .iter()
        .map(|x| x["action"]["type"].as_str().unwrap())
        .collect();
    assert_eq!(
        actions,
        ["block", "block", "ignore-previous-rules", "block"]
    );
    // The last block is the important one: it cannot be cancelled by the exception before it.
    assert!(r[3]["trigger"]["url-filter"]
        .as_str()
        .unwrap()
        .contains(r"example\.com["));
}

#[test]
fn a_document_exception_whitelists_the_page() {
    // An exception alone makes no list (it has nothing to cancel), so a block comes with it.
    let r = rules(
        "||ads.example^
@@||trusted.example^$document
",
    );
    assert_eq!(r.len(), 2);
    let r = r[1].clone();
    assert_eq!(r["action"]["type"], json!("ignore-previous-rules"));
    assert_eq!(r["trigger"]["url-filter"], json!(".*"));
    assert_eq!(
        r["trigger"]["if-top-url"],
        json!([r"^[^:]+://+([^:/?#]+\.)?trusted\.example[^a-zA-Z0-9_.%-]"])
    );
}

#[test]
fn badfilter_removes_the_filter_it_names() {
    let out = convert(
        "||a.example^$script\n||b.example^\n||a.example^$script,badfilter\n",
        &Options::default(),
    );
    assert_eq!(out.stats.badfiltered, 1);
    assert_eq!(out.stats.blocks, 1);
    let r = check_list(&out.lists[0].json, WEBKIT_MAX_RULES);
    assert!(r[0]["trigger"]["url-filter"]
        .as_str()
        .unwrap()
        .contains("b"));
}

#[test]
fn duplicates_are_removed_and_counted() {
    let out = convert(
        "||a.example^\n||a.example^\n||A.example^\n",
        &Options::default(),
    );
    // The third differs only in case; the converter does not lower-case patterns (the
    // engine matches case-insensitively), so its text, and its rule, differ.
    assert_eq!(out.stats.duplicates, 1);
    assert_eq!(out.stats.blocks, 2);
}

#[test]
fn what_cannot_be_expressed_is_dropped_and_counted() {
    let source = "\
! a comment\n\
[Adblock Plus 2.0]\n\
\n\
example.com##.banner\n\
example.com#@#.banner\n\
||a.example^$redirect=noopjs\n\
||b.example^$csp=script-src 'none'\n\
/(ad|banner)/\n\
||bücher.example^\n\
||c.example^$domain=a.example|~b.example\n\
@@||d.example^$elemhide\n\
||ok.example^\n";
    let out = convert(source, &Options::default());
    let s = &out.stats;
    assert_eq!(s.lines, 12);
    assert_eq!(s.comments, 3);
    assert_eq!(s.blocks, 1);
    assert_eq!(s.dropped(DropReason::Cosmetic), 3);
    assert_eq!(s.dropped(DropReason::UnsupportedOption), 2);
    assert_eq!(s.dropped(DropReason::UnsupportedPattern), 1);
    assert_eq!(s.dropped(DropReason::NonAscii), 1);
    assert_eq!(s.dropped(DropReason::MixedDomains), 1);
    assert_eq!(s.dropped_total(), 8);
}

#[test]
fn nothing_convertible_is_no_list() {
    let out = convert(
        "! only comments\nexample.com##.ad\n@@||x.example^\n",
        &Options::default(),
    );
    assert!(out.lists.is_empty());
}

#[test]
fn long_lists_split_at_the_limit() {
    let mut source = String::new();
    for i in 0..250 {
        source.push_str(&format!("||ad{i}.example^\n"));
    }
    source.push_str("@@||ad1.example/ok\n@@||ad2.example/ok\n");
    let out = convert(
        &source,
        &Options {
            max_rules_per_list: 100,
        },
    );
    assert_eq!(out.stats.blocks, 250);
    // 100 - 2 exceptions = 98 blocks per list: 3 lists.
    assert_eq!(out.lists.len(), 3);
    for list in &out.lists {
        let r = check_list(&list.json, 100);
        assert_eq!(r.len(), list.rules);
        let last = r.last().unwrap();
        assert_eq!(last["action"]["type"], json!("ignore-previous-rules"));
    }
}

#[test]
fn output_is_deterministic() {
    let source = "||a.example^\n@@||b.example^\n/x/$script\n||c.example^$important\n";
    assert_eq!(
        convert(source, &Options::default()),
        convert(source, &Options::default())
    );
}

#[test]
fn crlf_and_padding_are_tolerated() {
    let r = rules("  ||a.example^  \r\n! c\r\n");
    assert_eq!(r.len(), 1);
}

#[test]
fn a_filter_that_would_block_everything_is_not_a_rule() {
    let out = convert("*\n|\n||\n^\n$script\n||ok.example^\n", &Options::default());
    // `$script` alone is a type, so it is a rule (blocks every script); the bare anchors are not.
    assert_eq!(out.stats.blocks, 2);
    assert_eq!(out.stats.dropped(DropReason::UnsupportedPattern), 4);
}

#[test]
fn plugin_requests_have_no_webkit_type_and_are_dropped() {
    let out = convert(
        "||a.example^$object\n||b.example^$object-subrequest\n",
        &Options::default(),
    );
    assert_eq!(out.stats.blocks, 0);
    assert_eq!(out.stats.dropped(DropReason::UnsupportedOption), 2);
}

#[test]
fn a_dropped_exception_is_counted_apart() {
    let out = convert(
        "||a.example^\n@@/(x|y)js/\n@@||b.example^$csp=x\n",
        &Options::default(),
    );
    assert_eq!(out.stats.dropped_exceptions, 2);
    assert_eq!(out.stats.dropped_total(), 2);
}

#[test]
fn important_is_not_a_duplicate_of_the_plain_filter() {
    let out = convert(
        "||a.example^\n||a.example^$important\n@@||a.example/ok\n",
        &Options::default(),
    );
    assert_eq!(
        (out.stats.blocks, out.stats.important, out.stats.duplicates),
        (1, 1, 0)
    );
}

#[test]
fn a_pattern_that_is_all_wildcards_or_enormous_is_refused() {
    let many = format!("||a.example/{}\n", "x*".repeat(40));
    let long = format!("||a.example/{}\n", "y".repeat(3000));
    let out = convert(&format!("{many}{long}||ok.example^\n"), &Options::default());
    assert_eq!(out.stats.blocks, 1);
    assert_eq!(out.stats.dropped(DropReason::UnsupportedPattern), 2);
}
