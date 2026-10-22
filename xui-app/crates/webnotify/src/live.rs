//! The notifications on screen, and the limits that keep one site from using up the browser's
//! allowance of toasts.
//!
//! The shell lets an app keep 16 toasts and post a burst of 6; LazyWeb+ shares that allowance
//! between the sites it shows and its own downloads. So a site gets at most [`LIVE_PER_ORIGIN`]
//! live toasts, and posts (a new one or an update) at the pace of [`BURST`] at once and one more
//! each second; all sites together keep at most [`LIVE_TOTAL`]. What a site posts beyond that is
//! dropped, never queued: a page can produce notifications in a loop, and a queue of them is
//! the denial of service.
//!
//! A notification with a `tag` replaces the live one of the same site and tag: the toast is
//! updated in place. The model is pure; the clock is whatever milliseconds the caller passes.

use std::collections::HashMap;

/// Live toasts one site may have.
pub const LIVE_PER_ORIGIN: usize = 3;
/// Live web toasts in all, leaving the rest of the shell's allowance to downloads.
pub const LIVE_TOTAL: usize = 10;
/// Posts a site may make at once; one more is earned each second.
pub const BURST: u64 = 3;
/// A toast the shell never reported closed is forgotten after this long (its longest life is 60 s).
pub const LIFETIME_MS: u64 = 70_000;
/// The most sites whose pace is remembered.
const MAX_PACES: usize = 64;

const SECOND: u64 = 1000;

/// A notification that is on screen, or being put there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Live {
    /// WebKit's id for it: what the engine is told about.
    pub id: u64,
    /// The site, as [`Origin::key`](crate::origin::Origin::key).
    pub origin: String,
    /// The page that asked.
    pub page_url: String,
    pub tag: String,
    /// The shell's id of its toast, once the shell answered.
    pub toast: Option<u64>,
    /// When it was admitted.
    pub since_ms: u64,
    /// The page's object is gone (it navigated away): the toast is the person's and a click
    /// cannot reach a script.
    pub orphaned: bool,
}

/// Why a notification was not shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dropped {
    /// The site posted faster than the pace allows.
    Rate,
    /// The site already has [`LIVE_PER_ORIGIN`] toasts.
    Site,
    /// The browser already shows [`LIVE_TOTAL`] web toasts.
    Busy,
}

impl Dropped {
    /// The word markers use.
    pub fn word(self) -> &'static str {
        match self {
            Dropped::Rate => "rate",
            Dropped::Site => "site",
            Dropped::Busy => "busy",
        }
    }
}

/// What to do with a notification a page asked for.
#[derive(Debug, PartialEq, Eq)]
pub enum Admit {
    /// Show it; `replaces` is the toast of the live notification with its tag, updated in place,
    /// and `displaced` that notification's WebKit id (its page object is told it closed).
    Post {
        replaces: Option<u64>,
        displaced: Option<u64>,
    },
    Drop(Dropped),
}

/// One site's pace: how much of a post it has earned.
struct Pace {
    /// Thousandths of a post.
    earned: u64,
    at_ms: u64,
}

/// Every live web notification and every site's pace.
pub struct Registry {
    live: Vec<Live>,
    paces: HashMap<String, Pace>,
    /// Live web toasts in all ([`LIVE_TOTAL`] unless [`Registry::with_total`] says otherwise).
    total: usize,
}

impl Default for Registry {
    fn default() -> Self {
        Registry::with_total(LIVE_TOTAL)
    }
}

/// A token bucket: `burst` posts at once, one more earned every `refill_ms`. A site's pace is one
/// (inside [`Registry`]); webd keeps one per client as well, so that one client's pages cannot use
/// up the sender-wide allowance the shell grants webd for every client.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bucket {
    burst: u64,
    refill_ms: u64,
    /// Milliseconds earned toward the next token, plus whole tokens times `refill_ms`.
    earned: u64,
    at_ms: u64,
}

impl Bucket {
    /// A full bucket at `now_ms`. `refill_ms` is at least 1.
    pub fn new(now_ms: u64, burst: u64, refill_ms: u64) -> Bucket {
        let refill_ms = refill_ms.max(1);
        Bucket {
            burst,
            refill_ms,
            earned: burst.saturating_mul(refill_ms),
            at_ms: now_ms,
        }
    }

    /// Takes one post out of the bucket; false when it has not earned one.
    pub fn take(&mut self, now_ms: u64) -> bool {
        let cap = self.burst.saturating_mul(self.refill_ms);
        self.earned = self
            .earned
            .saturating_add(now_ms.saturating_sub(self.at_ms))
            .min(cap);
        self.at_ms = self.at_ms.max(now_ms);
        if self.earned < self.refill_ms {
            return false;
        }
        self.earned -= self.refill_ms;
        true
    }

    /// Whether it is full again (nothing worth remembering).
    pub fn is_full(&self, now_ms: u64) -> bool {
        self.earned
            .saturating_add(now_ms.saturating_sub(self.at_ms))
            >= self.burst.saturating_mul(self.refill_ms)
    }
}

impl Registry {
    /// A registry that keeps `total` live toasts in all (at least one) instead of [`LIVE_TOTAL`]: webd
    /// keeps one per client, within the shell's allowance for all of webd's clients together.
    pub fn with_total(total: usize) -> Registry {
        Registry {
            live: Vec::new(),
            paces: HashMap::new(),
            total: total.max(1),
        }
    }

    /// Whether anything is live.
    pub fn is_empty(&self) -> bool {
        self.live.is_empty()
    }

    /// Decides on notification `id` of `origin`, asked for by `page_url`; a post is kept as live
    /// (without a toast yet: [`Registry::posted`]).
    pub fn admit(
        &mut self,
        now_ms: u64,
        id: u64,
        origin: &str,
        page_url: &str,
        tag: &str,
    ) -> Admit {
        let same_tag = (!tag.is_empty())
            .then(|| {
                self.live
                    .iter()
                    .position(|n| n.origin == origin && n.tag == tag)
            })
            .flatten();
        let site_live = self.live.iter().filter(|n| n.origin == origin).count();
        if same_tag.is_none() && site_live >= LIVE_PER_ORIGIN {
            return Admit::Drop(Dropped::Site);
        }
        if same_tag.is_none() && self.live.len() >= self.total {
            return Admit::Drop(Dropped::Busy);
        }
        if !self.pay(now_ms, origin) {
            return Admit::Drop(Dropped::Rate);
        }
        let (replaces, displaced) = match same_tag {
            Some(at) => {
                let old = self.live.remove(at);
                (old.toast, Some(old.id))
            }
            None => (None, None),
        };
        self.live.push(Live {
            id,
            origin: origin.to_owned(),
            page_url: page_url.to_owned(),
            tag: tag.to_owned(),
            toast: None,
            since_ms: now_ms,
            orphaned: false,
        });
        Admit::Post {
            replaces,
            displaced,
        }
    }

    /// Takes one post out of `origin`'s pace; false when it has not earned one.
    fn pay(&mut self, now_ms: u64, origin: &str) -> bool {
        if self.paces.len() >= MAX_PACES && !self.paces.contains_key(origin) {
            // Sites that are back to a full burst carry nothing worth remembering.
            self.paces
                .retain(|_, pace| pace.earned + now_ms.saturating_sub(pace.at_ms) < BURST * SECOND);
            if self.paces.len() >= MAX_PACES {
                return false;
            }
        }
        let pace = self.paces.entry(origin.to_owned()).or_insert(Pace {
            earned: BURST * SECOND,
            at_ms: now_ms,
        });
        // A thousandth of a post per millisecond: one a second.
        let gained = now_ms.saturating_sub(pace.at_ms);
        pace.earned = (pace.earned + gained).min(BURST * SECOND);
        pace.at_ms = now_ms;
        if pace.earned < SECOND {
            return false;
        }
        pace.earned -= SECOND;
        true
    }

    /// The shell showed notification `id` as `toast`. Returns a toast to close when the
    /// notification went away while the shell was making it (the page took it down, a newer one
    /// with its tag replaced it).
    pub fn posted(&mut self, id: u64, toast: u64) -> Option<u64> {
        match self.live.iter_mut().find(|n| n.id == id) {
            Some(n) => {
                n.toast = Some(toast);
                None
            }
            None => Some(toast),
        }
    }

    /// The shell could not show notification `id`: it is forgotten.
    pub fn failed(&mut self, id: u64) -> Option<Live> {
        self.take(|n| n.id == id)
    }

    /// The page took notification `id` down: it is forgotten, and its toast (if the shell made
    /// one) is returned to be closed.
    pub fn cancel(&mut self, id: u64) -> Option<Live> {
        self.take(|n| n.id == id)
    }

    /// The page's object for `id` is gone; the toast stays.
    pub fn gone(&mut self, id: u64) {
        if let Some(n) = self.live.iter_mut().find(|n| n.id == id) {
            n.orphaned = true;
        }
    }

    /// Every toast is gone (the shell lost its line): nothing is live, the paces stay.
    pub fn forget_all(&mut self) {
        self.live.clear();
    }

    /// Every page object is gone (the page's process crashed): the toasts stay and none can
    /// reach a script.
    pub fn orphan_all(&mut self) {
        for n in &mut self.live {
            n.orphaned = true;
        }
    }

    /// The notification whose toast is `toast`.
    pub fn by_toast(&self, toast: u64) -> Option<&Live> {
        self.live.iter().find(|n| n.toast == Some(toast))
    }

    /// The shell took `toast` down: its notification is forgotten.
    pub fn toast_closed(&mut self, toast: u64) -> Option<Live> {
        self.take(|n| n.toast == Some(toast))
    }

    /// Forgets what outlived [`LIFETIME_MS`] (a close the shell never reported), returning it.
    pub fn expire(&mut self, now_ms: u64) -> Vec<Live> {
        let mut gone = Vec::new();
        while let Some(old) = self.take(|n| now_ms.saturating_sub(n.since_ms) > LIFETIME_MS) {
            gone.push(old);
        }
        gone
    }

    /// Every live notification (the window is closing: their toasts go with it).
    pub fn all(&self) -> &[Live] {
        &self.live
    }

    /// How many notifications `origin` has live.
    pub fn live_of(&self, origin: &str) -> usize {
        self.live.iter().filter(|n| n.origin == origin).count()
    }

    fn take(&mut self, wanted: impl Fn(&Live) -> bool) -> Option<Live> {
        let at = self.live.iter().position(wanted)?;
        Some(self.live.remove(at))
    }
}

#[cfg(test)]
mod tests {
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
}
