use super::*;

const NOW: u64 = 1_800_000_000;

fn visit(url: &str, title: &str, ago_days: u64) -> Visit {
    Visit {
        time: NOW - ago_days * DAY,
        url: url.into(),
        title: title.into(),
    }
}

fn urls(matches: &[Match<'_>]) -> Vec<String> {
    matches.iter().map(|m| m.url.clone()).collect()
}

#[test]
fn visits_of_one_page_are_counted_once_with_the_newest_title() {
    let index = Index::from_visits(&[
        visit("https://a.example/", "Old", 9),
        visit("https://b.example/", "B", 1),
        visit("https://a.example/", "New", 2),
        visit("https://a.example/", "", 3),
    ]);
    assert_eq!(index.len(), 2);
    let a = index.rank("a.example", NOW, 8);
    assert_eq!(a[0].visits, 3);
    assert_eq!(a[0].title, "New");
    assert_eq!(a[0].last, NOW - 2 * DAY);
}

#[test]
fn words_match_titles_hosts_and_the_start_of_the_address() {
    let index = Index::from_visits(&[
        visit("https://www.example.com/docs/intro", "Getting Started", 1),
        visit("https://gist.github.com/x", "A snippet", 1),
        visit("http://10.0.2.2:8080/", "Test server", 1),
    ]);
    let hit = |q: &str| urls(&index.rank(q, NOW, 8));
    assert_eq!(
        hit("exa"),
        ["https://www.example.com/docs/intro"],
        "host, www skipped"
    );
    assert_eq!(
        hit("example.com/do"),
        ["https://www.example.com/docs/intro"]
    );
    assert_eq!(
        hit("start"),
        ["https://www.example.com/docs/intro"],
        "title word"
    );
    assert_eq!(hit("github"), ["https://gist.github.com/x"], "host label");
    assert_eq!(
        hit("GIST snip"),
        ["https://gist.github.com/x"],
        "every word, any case"
    );
    assert_eq!(hit("10.0.2"), ["http://10.0.2.2:8080/"]);
    assert!(hit("tarted").is_empty(), "words match at their start");
    assert!(hit("start nothing").is_empty(), "every word must match");
    assert!(hit("   ").is_empty());
    assert!(hit("").is_empty());
}

#[test]
fn visits_times_recency_decide_and_an_address_opening_beats_a_title() {
    let mut visits = Vec::new();
    // Ten visits long ago against two visits today.
    for _ in 0..10 {
        visits.push(visit("https://old.example/", "Shared name", 200));
    }
    for _ in 0..2 {
        visits.push(visit("https://new.example/", "Shared name", 0));
    }
    // A page whose title only says "ex", against a host that starts with it.
    visits.push(visit("https://other.test/", "ex libris", 0));
    visits.push(visit("https://ex.example/", "Home", 0));
    visits.push(visit("https://ex.example/", "Home", 0));
    let index = Index::from_visits(&visits);
    // 10 * 10% = 100 against 2 * 100% = 200.
    assert_eq!(
        urls(&index.rank("shared", NOW, 8)),
        ["https://new.example/", "https://old.example/"]
    );
    // The address that opens with the text (2 visits, doubled) beats the title that has it.
    let ex = urls(&index.rank("ex", NOW, 8));
    assert_eq!(ex[0], "https://ex.example/");
    assert!(ex.contains(&"https://other.test/".to_owned()));
}

#[test]
fn only_the_limit_is_returned_best_first() {
    let visits: Vec<Visit> = (0..30)
        .map(|n| visit(&format!("https://site{n}.example/"), "Site", n))
        .collect();
    let index = Index::from_visits(&visits);
    let top = index.rank("site", NOW, MAX_ROWS);
    assert_eq!(top.len(), MAX_ROWS);
    // Equal visit counts: the more recent first.
    assert_eq!(top[0].url, "https://site0.example/");
    assert_eq!(top[7].url, "https://site7.example/");
    assert!(index.rank("site", NOW, 0).is_empty());
}

#[test]
fn removing_a_page_forgets_all_of_its_visits() {
    let mut index = Index::from_visits(&[
        visit("https://a.example/", "A", 1),
        visit("https://a.example/", "A", 2),
        visit("https://b.example/", "B", 1),
    ]);
    index.remove("https://a.example/");
    assert!(index.rank("a.example", NOW, 8).is_empty());
    assert_eq!(index.len(), 1);
}

#[test]
fn the_best_host_completes_what_was_typed_and_only_one_word_of_it() {
    let index = Index::from_visits(&[visit("https://www.example.com/docs", "Docs", 0)]);
    let best = &index.rank("exa", NOW, 1)[0];
    assert_eq!(completion("exa", best).as_deref(), Some("example.com"));
    assert_eq!(completion("  EXA", best).as_deref(), Some("EXAmple.com"));
    assert_eq!(completion("example.com", best), None, "nothing left to add");
    assert_eq!(completion("exa mple", best), None);
    assert_eq!(
        completion("example.com/d", best),
        None,
        "a path is not completed"
    );
    assert_eq!(completion("", best), None);
    // A title match that does not open the host adds nothing.
    let titled = Index::from_visits(&[visit("https://z.example/", "Docs", 0)]);
    assert_eq!(completion("docs", titled.rank("docs", NOW, 1)[0]), None);
}

#[test]
fn odd_text_is_ranked_without_panicking() {
    let index = Index::from_visits(&[
        visit("https://caf\u{e9}.example/", "Caf\u{c9} \u{1f600} menu", 1),
        visit("https://x.example/\u{0}", "\u{202e}rtl", 1),
    ]);
    assert_eq!(index.rank("caf\u{e9}", NOW, 8).len(), 1);
    assert_eq!(index.rank("CAF\u{c9}", NOW, 8).len(), 1);
    assert_eq!(index.rank(&"a".repeat(100_000), NOW, 8).len(), 0);
    assert_eq!(index.rank("\u{1f600}", NOW, 8).len(), 0);
}

/// The ranking has to keep up with typing: 50,000 pages in a frame. Release only (`cargo test
/// --release -p xui-lazyweb bench -- --ignored --nocapture`).
#[test]
#[ignore = "timing: run in release"]
fn bench_ranking_fifty_thousand_entries_takes_under_a_frame() {
    let words = [
        "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india", "juliet",
    ];
    let items = (0..50_000u64).map(|n| {
        let (a, b) = (words[(n % 10) as usize], words[((n / 10) % 10) as usize]);
        (
            format!("https://www.{a}{n}.example/{b}/page{n}"),
            format!("{a} {b} notes number {n}"),
            1 + (n % 7) as u32,
            NOW - (n % 400) * DAY,
        )
    });
    let index = Index::from_items(items);
    assert_eq!(index.len(), 50_000);
    let mut worst = std::time::Duration::ZERO;
    for query in ["a", "alpha", "alpha br", "ech 12", "zzz", "notes", "www.al"] {
        let started = std::time::Instant::now();
        let top = index.rank(query, NOW, MAX_ROWS);
        let took = started.elapsed();
        println!("rank {query:?}: {} rows in {took:?}", top.len());
        worst = worst.max(took);
    }
    assert!(
        worst.as_millis() < 16,
        "the slowest query took {worst:?}, over a 60 Hz frame"
    );
}
