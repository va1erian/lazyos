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
mod tests;
