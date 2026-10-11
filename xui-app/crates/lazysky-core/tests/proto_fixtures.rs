//! The protocol layer against realistic server answers: a timeline with
//! reposts, quotes, one to four images, an external card, a video, an unknown
//! embed, a missing `aspectRatio`, a blocked post and bad facets; a thread;
//! notifications; a `createSession` with a DID document.

use lazysky_core::proto::embed::DEFAULT_ASPECT;
use lazysky_core::proto::{
    notifications, parse_json, Embed, Feature, NotificationKind, Page, Quoted, Reason, Session,
    Thread,
};

fn fixture(name: &str) -> serde_json::Value {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    parse_json(&bytes).unwrap()
}

#[test]
fn the_timeline_reads_every_embed_and_skips_the_blocked_post() {
    let page = Page::from_feed(&fixture("timeline.json"));
    assert_eq!(page.cursor.as_deref(), Some("cursor-page-2"));
    assert_eq!(page.skipped, 1, "only the blocked post cannot be shown");
    assert_eq!(page.items.len(), 14);

    let by_text = |needle: &str| {
        page.items
            .iter()
            .find(|i| i.post.text.starts_with(needle))
            .unwrap_or_else(|| panic!("no post starting {needle:?}"))
    };

    // Facets: a valid link, and the post whose only facet falls inside "é".
    let linked = by_text("Read:");
    assert!(matches!(
        linked.post.facets.as_slice(),
        [f] if f.feature == Feature::Link("https://example.test/article".into())
            && &linked.post.text[f.start..f.end] == "https://example.test/article"
    ));
    assert!(by_text("héllo").post.facets.is_empty());

    // One to four images; image 1 of each has no aspectRatio.
    for k in 1..=4 {
        let post = &by_text(&format!("{k} image")).post;
        assert_eq!(post.embed.images().len(), k);
    }
    let two = by_text("2 image").post.embed.images();
    assert_eq!(two[0].aspect, Some((4000, 3000)));
    assert_eq!(two[1].aspect_or_default(), DEFAULT_ASPECT);

    assert!(matches!(&by_text("the dog").post.embed, Embed::External(c) if c.title == "A dog"));
    assert!(matches!(&by_text("a video").post.embed, Embed::Video(_)));
    assert_eq!(by_text("from the future").post.embed, Embed::Unknown);

    match &by_text("quoting").post.embed {
        Embed::Record(Quoted::Post(q)) => {
            assert_eq!(q.text, "original thought");
            assert_eq!(q.author.handle, "carol.bsky.test");
        }
        other => panic!("{other:?}"),
    }
    let with_media = &by_text("quote with a picture").post.embed;
    assert!(matches!(
        with_media,
        Embed::RecordWithMedia(Quoted::Post(_), _)
    ));
    assert_eq!(with_media.images().len(), 1);

    // Repost reason and viewer state.
    let reposted = by_text("reposted by");
    assert!(
        matches!(&reposted.reason, Some(Reason::Repost { by }) if by.handle == "carol.bsky.test")
    );
    let done = by_text("liked and");
    assert!(done.post.viewer.like.is_some() && done.post.viewer.repost.is_some());
    assert_eq!(by_text("the dog").post.author.name(), "Bob 🦀");
}

#[test]
fn row_ids_are_unique_and_stable_across_a_reparse() {
    let a = Page::from_feed(&fixture("timeline.json"));
    let b = Page::from_feed(&fixture("timeline.json"));
    let ids = |p: &Page| p.items.iter().map(|i| i.row_id()).collect::<Vec<_>>();
    assert_eq!(ids(&a), ids(&b));
    let mut sorted = ids(&a);
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), a.items.len());
}

#[test]
fn the_thread_flattens_in_reading_order() {
    let thread = Thread::from_json(&fixture("thread.json")).unwrap();
    let rows: Vec<_> = thread
        .rows()
        .into_iter()
        .map(|(p, level)| (p.text.clone(), level))
        .collect();
    let expect = [
        ("thread post 1", 0),
        ("thread post 2", 0),
        ("thread post 3", 0),
        ("thread post 4", 1),
        ("thread post 5", 2),
        ("thread post 6", 1),
    ];
    assert_eq!(rows, expect.map(|(t, l)| (t.to_string(), l)));
}

#[test]
fn notifications_summarise() {
    let (items, cursor) = notifications::parse(&fixture("notifications.json"));
    assert_eq!(cursor.as_deref(), Some("n2"));
    let lines: Vec<_> = items.iter().map(|n| n.summary()).collect();
    assert_eq!(
        lines,
        [
            "Alice liked your post",
            "Bob 🦀 replied to you",
            "carol.bsky.test followed you"
        ]
    );
    assert_eq!(items[1].kind, NotificationKind::Reply);
    assert_eq!(items[1].text, "great post!");
    assert_eq!(items.iter().filter(|n| !n.is_read).count(), 2);
}

#[test]
fn a_session_finds_its_pds_and_hides_its_tokens() {
    let session = Session::from_json(&fixture("session.json"), "https://bsky.social").unwrap();
    assert_eq!(session.pds, "https://pds.bsky.test");
    assert_eq!(session.handle, "me.bsky.test");
    assert!(!format!("{session:?}").contains("SECRET"));
}

#[test]
fn garbage_pages_are_empty_not_errors() {
    for body in [
        &b"{}"[..],
        b"[]",
        b"{\"feed\": 7}",
        b"{\"feed\": [null, 1, \"x\"]}",
    ] {
        let page = Page::from_feed(&parse_json(body).unwrap());
        assert!(page.items.is_empty());
    }
    assert!(parse_json(b"<html>").unwrap_err().contains("not JSON"));
}
