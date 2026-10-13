//! Seeded fuzzing of `convert`: whatever the text, it must not panic, must stay
//! deterministic and must emit only lists WebKit's parser would take. Replay a failure
//! with `FUZZ_SEED=0x...`; run longer with `FUZZ_CASES=20000 cargo test --release`.

mod common;

use common::{check_list, fuzz_cases, fuzz_seed, Rng};
use lazyadblock::{convert, Options};

const FRAGMENTS: [&str; 40] = [
    "||",
    "|",
    "@@",
    "*",
    "^",
    "$",
    "$script",
    "$image,third-party",
    "$~third-party",
    "$domain=a.com|~b.com",
    "$domain=a.com|b.org",
    "$important",
    "$match-case",
    "$badfilter",
    "$redirect=x",
    "$document",
    "$popup",
    "##.ad",
    "#@#",
    "example.com",
    "ads.example.com",
    "/ads/",
    "/banner",
    ".js",
    "?id=",
    "&x=",
    "/",
    "-",
    "_",
    "%2F",
    "\\",
    "(",
    ")",
    "[",
    "]",
    "{2,3}",
    "+",
    "?",
    "bücher",
    " ",
];

fn random_line(rng: &mut Rng) -> String {
    let mut line = String::new();
    match rng.below(10) {
        // Printable ASCII noise.
        0 => {
            for _ in 0..rng.below(40) {
                line.push((0x20 + rng.below(0x5f)) as u8 as char);
            }
        }
        // A regex filter.
        1 => {
            line.push('/');
            for _ in 0..1 + rng.below(8) {
                line.push_str(rng.pick(&FRAGMENTS));
            }
            line.push('/');
            if rng.chance(30) {
                line.push_str("$script");
            }
        }
        // Raw bytes, as the lossy decoder of a downloaded file would see them.
        2 => {
            let bytes: Vec<u8> = (0..rng.below(24)).map(|_| rng.next() as u8).collect();
            line = String::from_utf8_lossy(&bytes).into_owned();
        }
        _ => {
            for _ in 0..1 + rng.below(6) {
                line.push_str(rng.pick(&FRAGMENTS));
            }
        }
    }
    line
}

fn random_source(rng: &mut Rng) -> String {
    let mut s = String::new();
    for _ in 0..1 + rng.below(40) {
        s.push_str(&random_line(rng));
        s.push_str(if rng.chance(10) { "\r\n" } else { "\n" });
    }
    s
}

#[test]
fn any_text_converts_to_lists_webkit_accepts() {
    let seed = fuzz_seed(0xad_b10c_f022);
    let mut rng = Rng(seed);
    let options = Options {
        max_rules_per_list: 16,
    };
    let (mut lists, mut rules) = (0usize, 0usize);
    for case in 0..fuzz_cases(1500) {
        let source = random_source(&mut rng);
        let out = convert(&source, &options);
        assert_eq!(
            out,
            convert(&source, &options),
            "seed {seed:#x} case {case}: not deterministic"
        );
        for list in &out.lists {
            let parsed = check_list(&list.json, options.max_rules_per_list);
            assert_eq!(parsed.len(), list.rules, "seed {seed:#x} case {case}");
            lists += 1;
            rules += parsed.len();
        }
        let s = &out.stats;
        assert!(
            s.comments + s.dropped_total() <= s.lines,
            "seed {seed:#x} case {case}: {s}"
        );
        assert_eq!(
            s.lines,
            source.lines().count(),
            "seed {seed:#x} case {case}"
        );
    }
    assert!(
        lists > 100 && rules > 500,
        "the generator reached {lists} lists and {rules} rules"
    );
}

#[test]
fn an_exception_only_ever_trails_the_blocks_it_may_cancel() {
    let mut rng = Rng(fuzz_seed(0xe_c_e_9));
    for _ in 0..fuzz_cases(300) {
        let source = random_source(&mut rng);
        let out = convert(
            &source,
            &Options {
                max_rules_per_list: 12,
            },
        );
        for list in &out.lists {
            let rules = check_list(&list.json, 12);
            // Block rules, then ignore-previous-rules, then (important) blocks again:
            // never a block, an exception, a block, an exception.
            let kinds: Vec<bool> = rules
                .iter()
                .map(|r| r["action"]["type"] == "block")
                .collect();
            let flips = kinds.windows(2).filter(|w| w[0] != w[1]).count();
            assert!(flips <= 2, "{kinds:?}");
        }
    }
}

#[test]
fn a_large_list_converts_quickly_and_whole() {
    // 200,000 filters of mixed kinds: the converter is linear, so this stays well under a
    // second in release and a few seconds in debug.
    let mut rng = Rng(fuzz_seed(0x1a69e));
    let mut source = String::new();
    for i in 0..200_000 {
        match i % 5 {
            0 => source.push_str(&format!("||ad{i}.example^\n")),
            1 => source.push_str(&format!("/ads{i}/*$script,third-party\n")),
            2 => source.push_str(&format!("@@||ad{}.example/ok\n", rng.below(i + 1))),
            3 => source.push_str(&format!("example.com##.ad{i}\n")),
            _ => source.push_str(&format!("||t{i}.example^$domain=a{i}.com\n")),
        }
    }
    let out = convert(&source, &Options::default());
    assert!(out.lists.iter().all(|l| l.rules <= 150_000));
    assert_eq!(out.stats.dropped_total(), 40_000);
    assert!(out.stats.blocks > 100_000);
    // The one list parses (and every rule in it is accepted by the subset parser).
    let first = &out.lists[0];
    let parsed = check_list(&first.json, 150_000);
    assert_eq!(parsed.len(), first.rules);
}
