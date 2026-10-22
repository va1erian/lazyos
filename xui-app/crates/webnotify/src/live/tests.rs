use super::*;

const A: &str = "https://a.test";
const B: &str = "https://b.test";

fn post(registry: &mut Registry, now: u64, id: u64, origin: &str, tag: &str) -> Admit {
    registry.admit(now, id, origin, &format!("{origin}/"), tag)
}

fn posted(registry: &mut Registry, id: u64) {
    assert_eq!(registry.posted(id, 1000 + id), None);
}

#[test]
fn a_burst_of_six_shows_three_and_drops_three() {
    let mut registry = Registry::default();
    let outcomes: Vec<Admit> = (1..=6)
        .map(|id| post(&mut registry, 0, id, A, &format!("t{id}")))
        .collect();
    let shown = outcomes
        .iter()
        .filter(|o| matches!(o, Admit::Post { .. }))
        .count();
    assert_eq!(shown, 3);
    assert_eq!(registry.live_of(A), 3);
    // The fourth is over the site's live limit; the pace never gets asked.
    assert_eq!(outcomes[3], Admit::Drop(Dropped::Site));
}

#[test]
fn a_site_earns_one_post_a_second() {
    let mut registry = Registry::default();
    for id in 1..=3 {
        assert!(matches!(
            post(&mut registry, 0, id, A, &format!("t{id}")),
            Admit::Post { .. }
        ));
        registry.cancel(id);
    }
    // Nothing is live, and the burst is spent: the pace alone says no.
    assert_eq!(
        post(&mut registry, 500, 4, A, "n"),
        Admit::Drop(Dropped::Rate)
    );
    assert!(matches!(
        post(&mut registry, 1000, 5, A, "n"),
        Admit::Post { .. }
    ));
    assert_eq!(
        post(&mut registry, 1500, 6, A, "m"),
        Admit::Drop(Dropped::Rate)
    );
    assert!(matches!(
        post(&mut registry, 2000, 7, A, "m"),
        Admit::Post { .. }
    ));
}

#[test]
fn a_site_cannot_use_up_anothers_allowance() {
    let mut registry = Registry::default();
    for id in 1..=6 {
        let _ = post(&mut registry, 0, id, A, &format!("t{id}"));
    }
    assert!(matches!(
        post(&mut registry, 0, 10, B, "x"),
        Admit::Post { .. }
    ));
}

#[test]
fn the_browser_keeps_room_for_its_downloads() {
    let mut registry = Registry::default();
    let mut id = 0;
    let mut shown = 0;
    // Four sites with three each would be twelve.
    for origin in [
        "https://1.test",
        "https://2.test",
        "https://3.test",
        "https://4.test",
    ] {
        for _ in 0..3 {
            id += 1;
            if matches!(
                post(&mut registry, 0, id, origin, &format!("t{id}")),
                Admit::Post { .. }
            ) {
                shown += 1;
            }
        }
    }
    assert_eq!(shown, LIVE_TOTAL);
    id += 1;
    assert_eq!(
        post(&mut registry, 0, id, "https://5.test", "x"),
        Admit::Drop(Dropped::Busy)
    );
}

#[test]
fn a_tag_replaces_the_live_toast_of_its_site_in_place() {
    let mut registry = Registry::default();
    assert!(matches!(
        post(&mut registry, 0, 1, A, "chat"),
        Admit::Post { replaces: None, .. }
    ));
    posted(&mut registry, 1);
    assert_eq!(
        post(&mut registry, 1000, 2, A, "chat"),
        Admit::Post {
            replaces: Some(1001),
            displaced: Some(1)
        }
    );
    assert_eq!(registry.live_of(A), 1, "one toast, not two");
    // The same tag of another site is another notification.
    assert!(matches!(
        post(&mut registry, 1000, 3, B, "chat"),
        Admit::Post { replaces: None, .. }
    ));
    // An update keeps its place when the site is at its limit.
    for id in 10..12 {
        let _ = post(&mut registry, 5000, id, A, &format!("o{id}"));
    }
    assert_eq!(registry.live_of(A), 3);
    assert!(matches!(
        post(&mut registry, 9000, 4, A, "chat"),
        Admit::Post { .. }
    ));
}

#[test]
fn an_update_that_arrives_before_its_toast_is_closed_when_the_shell_answers() {
    let mut registry = Registry::default();
    let _ = post(&mut registry, 0, 1, A, "t");
    // Not answered yet: the update cannot name a toast.
    assert_eq!(
        post(&mut registry, 1000, 2, A, "t"),
        Admit::Post {
            replaces: None,
            displaced: Some(1)
        }
    );
    // The first one's toast arrives for a notification that is gone: it is to be closed.
    assert_eq!(registry.posted(1, 77), Some(77));
    assert_eq!(registry.posted(2, 78), None);
    assert_eq!(registry.by_toast(78).map(|n| n.id), Some(2));
}

#[test]
fn what_the_page_closes_and_the_shell_closes_is_forgotten() {
    let mut registry = Registry::default();
    let _ = post(&mut registry, 0, 1, A, "");
    let _ = post(&mut registry, 0, 2, A, "");
    posted(&mut registry, 1);
    posted(&mut registry, 2);
    assert_eq!(registry.cancel(1).and_then(|n| n.toast), Some(1001));
    assert_eq!(registry.toast_closed(1002).map(|n| n.id), Some(2));
    assert!(registry.all().is_empty());
    assert!(registry.cancel(1).is_none());
}

#[test]
fn a_page_that_is_gone_leaves_its_toast_clickable() {
    let mut registry = Registry::default();
    let _ = registry.admit(0, 1, A, "https://a.test/inbox", "");
    posted(&mut registry, 1);
    registry.gone(1);
    let n = registry.by_toast(1001).unwrap();
    assert!(n.orphaned);
    assert_eq!(n.page_url, "https://a.test/inbox");
}

#[test]
fn a_close_the_shell_never_reported_is_forgotten_in_time() {
    let mut registry = Registry::default();
    let _ = post(&mut registry, 0, 1, A, "");
    assert!(registry.expire(LIFETIME_MS).is_empty());
    let gone = registry.expire(LIFETIME_MS + 1);
    assert_eq!(gone.len(), 1);
    assert_eq!(registry.live_of(A), 0);
}

#[test]
fn a_bucket_allows_its_burst_then_one_per_refill() {
    let mut bucket = Bucket::new(1000, 3, 2000);
    assert!(bucket.take(1000) && bucket.take(1000) && bucket.take(1000));
    assert!(!bucket.take(1000), "the burst is spent");
    assert!(!bucket.take(2999), "not yet one refill");
    assert!(bucket.take(3000), "one refill earned");
    assert!(!bucket.take(3000));
    // A long quiet time earns the burst and no more.
    let mut taken = 0;
    while bucket.take(1_000_000) {
        taken += 1;
    }
    assert_eq!(taken, 3);
    assert!(bucket.is_full(1_000_000 + 6000));
    assert!(!bucket.is_full(1_000_000));
    // A clock that goes backwards earns nothing and breaks nothing.
    assert!(!bucket.take(5));
}

#[test]
fn a_registry_can_keep_fewer_live_toasts_in_all() {
    let mut registry = Registry::with_total(2);
    assert!(matches!(
        post(&mut registry, 0, 1, A, "a"),
        Admit::Post { .. }
    ));
    assert!(matches!(
        post(&mut registry, 0, 2, B, "b"),
        Admit::Post { .. }
    ));
    assert_eq!(
        post(&mut registry, 0, 3, "https://c.test", "c"),
        Admit::Drop(Dropped::Busy)
    );
    assert!(!registry.is_empty());
}

#[test]
fn many_sites_cannot_make_the_pace_table_grow_without_bound() {
    let mut registry = Registry::default();
    for n in 0..(MAX_PACES * 3) {
        let origin = format!("https://s{n}.test");
        let _ = registry.admit(0, n as u64, &origin, "", "");
        registry.cancel(n as u64);
    }
    assert!(registry.paces.len() <= MAX_PACES);
}
