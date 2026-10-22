//! What the engine's thread must decide on the spot about a full view (`webd_ui_navigate`,
//! `webd_ui_blocking`, `webd_ui_download_start`, `webd_paint_plan`): a table of the views' rules the
//! service keeps up to date and the engine's callbacks read, under one lock that is never held while
//! anything waits. Nothing here talks to a client: what a decision means for one (a held navigation,
//! a hand-off, a download) is queued as an event like the rest.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use lazydamage::{IRect, Pending, Plan};
use lazydownloads::{Destination, Saver};

use crate::fullconfig::Profile;
use crate::policy::{host, recent, Policy, Verdict};

/// A full view's rules, as the engine's thread needs them.
#[derive(Clone)]
pub struct ViewRules {
    pub policy: Policy,
    pub profile: Profile,
    /// Content blocking was asked for (the profile's switch and exceptions decide per page).
    pub blocking: bool,
    /// Where downloads go, when the view may download.
    pub downloads: Option<Arc<Saver>>,
    /// The client wants `NewWindowRequested`; without, a page's `window.open` is not even queued.
    pub popups: bool,
}

/// A profile's content-blocking switch and the hosts it is off for (`SetBlocking`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockSettings {
    pub enabled: bool,
    pub exceptions: Vec<String>,
}

impl Default for BlockSettings {
    fn default() -> Self {
        BlockSettings {
            enabled: true,
            exceptions: Vec::new(),
        }
    }
}

struct Entry {
    rules: ViewRules,
    /// When real input last reached the view (the service's clock, ms); 0: never.
    last_input_ms: u64,
    /// The next main-frame navigation to this address goes without the blocker
    /// (`Reload(bypass_blocking)` of the page on show); any other navigation forgets it.
    bypass_blocking: Option<String>,
    /// What changed since the last paint, for a view with deltas (`webd_paint_*`).
    paint: Option<Pending>,
    /// The view's downloads under way (the engine's ids): their `.part` files go with the view.
    downloads: BTreeSet<u64>,
}

/// The paint plan as the C ABI hands it out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaintPlan {
    Nothing,
    Full,
    Delta {
        shift: Option<[i32; 6]>,
        rects: Vec<[i32; 4]>,
    },
}

/// A download's id in the shared [`Saver`]: the engine's ids are per view.
pub fn download_key(view: u64, download: u64) -> u64 {
    (view << 32) | (download & 0xFFFF_FFFF)
}

#[derive(Default)]
pub struct Table {
    views: BTreeMap<u64, Entry>,
    blockers: BTreeMap<Profile, BlockSettings>,
    /// The notification decisions WebKit starts a page process with (`webd_ui_notify_permission_at`):
    /// origin and allowed, sorted by origin.
    notify: Vec<(String, bool)>,
}

impl Table {
    /// The decisions a page process learns at its start (WebKit keeps one record for the context).
    pub fn set_notify_decisions(&mut self, decisions: Vec<(String, bool)>) {
        self.notify = decisions;
    }

    /// Decision `index` of [`Table::set_notify_decisions`], if there is one.
    pub fn notify_decision_at(&self, index: usize) -> Option<(String, bool)> {
        self.notify.get(index).cloned()
    }

    pub fn register(&mut self, view: u64, rules: ViewRules) {
        self.views.insert(
            view,
            Entry {
                rules,
                last_input_ms: 0,
                bypass_blocking: None,
                downloads: BTreeSet::new(),
                paint: None,
            },
        );
    }

    /// Whether `view` wants new-window requests.
    pub fn popups(&self, view: u64) -> bool {
        self.views
            .get(&view)
            .is_some_and(|entry| entry.rules.popups)
    }

    /// The view is gone: its rules go, and the `.part` file of every download it had under way is
    /// removed (the engine cancels them, and tells nobody once the view is forgotten).
    pub fn unregister(&mut self, view: u64) {
        let Some(entry) = self.views.remove(&view) else {
            return;
        };
        if let Some(saver) = entry.rules.downloads {
            for download in entry.downloads {
                let _ = saver.finish(download_key(view, download), false);
            }
        }
    }

    pub fn input(&mut self, view: u64, now_ms: u64) {
        if let Some(entry) = self.views.get_mut(&view) {
            entry.last_input_ms = now_ms.max(1);
        }
    }

    /// The reload of `url` (the page on show) goes once without the blocker.
    pub fn bypass_blocking(&mut self, view: u64, url: &str) {
        if let Some(entry) = self.views.get_mut(&view) {
            entry.bypass_blocking = Some(url.to_owned());
        }
    }

    pub fn set_blocking(&mut self, profile: Profile, settings: BlockSettings) {
        self.blockers.insert(profile, settings);
    }

    pub fn blocking_settings(&self, profile: &Profile) -> BlockSettings {
        self.blockers.get(profile).cloned().unwrap_or_default()
    }

    pub fn navigate(
        &self,
        view: u64,
        url: &str,
        gesture: bool,
        main_frame: bool,
        now_ms: u64,
    ) -> Verdict {
        let Some(entry) = self.views.get(&view) else {
            return Verdict::Deny;
        };
        entry.rules.policy.decide(
            url,
            gesture,
            main_frame,
            recent(entry.last_input_ms, now_ms),
        )
    }

    /// Whether the rule lists apply to a page at `url` (the one-shot bypass is used up).
    pub fn blocking(&mut self, view: u64, url: &str) -> bool {
        let Some(entry) = self.views.get_mut(&view) else {
            return true;
        };
        if !entry.rules.blocking {
            return false;
        }
        // Used up by the next main-frame decision, which is the reload's unless the page navigated
        // first: then it is forgotten, never applied to another address.
        if entry
            .bypass_blocking
            .take()
            .is_some_and(|wanted| wanted == url)
        {
            return false;
        }
        let settings = self
            .blockers
            .get(&entry.rules.profile)
            .cloned()
            .unwrap_or_default();
        if !settings.enabled {
            return false;
        }
        let Some(host) = host(url) else {
            return true;
        };
        !settings
            .exceptions
            .iter()
            .any(|ex| host == *ex || host.ends_with(&format!(".{ex}")))
    }

    /// A destination for a download the engine starts, or `None` to refuse it.
    pub fn download(&mut self, view: u64, download: u64, suggested: &str) -> Option<Destination> {
        let entry = self.views.get_mut(&view)?;
        let saver = entry.rules.downloads.clone()?;
        let destination = saver
            .reserve(download_key(view, download), suggested)
            .ok()?;
        entry.downloads.insert(download);
        Some(destination)
    }

    /// A download ended (its file finished or removed by the caller).
    pub fn download_ended(&mut self, view: u64, download: u64) {
        if let Some(entry) = self.views.get_mut(&view) {
            entry.downloads.remove(&download);
        }
    }

    /// The saver of a view's downloads (to finish one).
    pub fn saver(&self, view: u64) -> Option<Arc<Saver>> {
        self.views.get(&view)?.rules.downloads.clone()
    }

    fn pending(&mut self, view: u64) -> Option<&mut Pending> {
        self.views.get_mut(&view)?.paint.as_mut()
    }

    pub fn paint_resize(&mut self, view: u64, width: i32, height: i32) {
        if let Some(entry) = self.views.get_mut(&view) {
            match &mut entry.paint {
                Some(pending) => pending.resize(width, height),
                None => entry.paint = Some(Pending::new(width, height)),
            }
        }
    }

    pub fn paint_full(&mut self, view: u64) {
        if let Some(pending) = self.pending(view) {
            pending.note_full();
        }
    }

    pub fn paint_rect(&mut self, view: u64, rect: [i32; 4]) {
        if let Some(pending) = self.pending(view) {
            pending.note_rect(IRect::new(rect[0], rect[1], rect[2], rect[3]));
        }
    }

    pub fn paint_update(
        &mut self,
        view: u64,
        area: [i32; 4],
        dx: i32,
        dy: i32,
        rects: &[[i32; 4]],
    ) {
        if let Some(pending) = self.pending(view) {
            let rects: Vec<IRect> = rects
                .iter()
                .map(|r| IRect::new(r[0], r[1], r[2], r[3]))
                .collect();
            pending.note_update(
                IRect::new(area[0], area[1], area[2], area[3]),
                dx,
                dy,
                &rects,
            );
        }
    }

    pub fn paint_plan(&mut self, view: u64) -> PaintPlan {
        // A view whose size the engine never said gets whole frames.
        let Some(pending) = self.pending(view) else {
            return PaintPlan::Full;
        };
        match pending.take_plan() {
            None => PaintPlan::Nothing,
            Some(Plan::Full) => PaintPlan::Full,
            Some(Plan::Delta { scroll, rects }) => PaintPlan::Delta {
                shift: scroll.map(|s| {
                    [
                        s.area.x0,
                        s.area.y0,
                        s.area.width(),
                        s.area.height(),
                        s.dx,
                        s.dy,
                    ]
                }),
                rects: rects
                    .iter()
                    .map(|r| [r.x0, r.y0, r.width(), r.height()])
                    .collect(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{Handoff, Schemes};

    fn rules(blocking: bool) -> ViewRules {
        ViewRules {
            policy: Policy {
                schemes: Schemes::Web,
                origins: Vec::new(),
                handoff: Handoff::Browser,
            },
            profile: Profile::Browser { ephemeral: false },
            blocking,
            downloads: None,
            popups: false,
        }
    }

    #[test]
    fn an_unknown_view_is_refused_everything() {
        let mut table = Table::default();
        assert_eq!(table.navigate(9, "http://a/", true, true, 5), Verdict::Deny);
        assert!(table.download(9, 1, "x").is_none());
        assert_eq!(table.paint_plan(9), PaintPlan::Full);
    }

    #[test]
    fn a_hand_off_needs_input_the_service_saw() {
        let mut table = Table::default();
        table.register(1, rules(false));
        assert_eq!(
            table.navigate(1, "mailto:a@b", true, true, 10_000),
            Verdict::Deny
        );
        table.input(1, 9_000);
        assert_eq!(
            table.navigate(1, "mailto:a@b", true, true, 10_000),
            Verdict::HandOff
        );
        table.unregister(1);
        assert_eq!(
            table.navigate(1, "http://a/", true, true, 10_000),
            Verdict::Deny
        );
    }

    #[test]
    fn blocking_follows_the_profile_switch_the_exceptions_and_the_one_shot_bypass() {
        let mut table = Table::default();
        table.register(1, rules(true));
        table.register(2, rules(false));
        assert!(table.blocking(1, "http://ads.example/"));
        assert!(
            !table.blocking(2, "http://ads.example/"),
            "the view did not ask for it"
        );
        table.bypass_blocking(1, "http://ads.example/");
        assert!(!table.blocking(1, "http://ads.example/"));
        assert!(table.blocking(1, "http://ads.example/"), "once");
        // A navigation elsewhere before the reload uses it up without applying it.
        table.bypass_blocking(1, "http://ads.example/");
        assert!(table.blocking(1, "http://other.example/"));
        assert!(table.blocking(1, "http://ads.example/"));
        let profile = Profile::Browser { ephemeral: false };
        table.set_blocking(
            profile.clone(),
            BlockSettings {
                enabled: true,
                exceptions: vec!["example".into()],
            },
        );
        assert!(!table.blocking(1, "http://ads.example/"));
        assert!(!table.blocking(1, "https://example:8443/"));
        assert!(table.blocking(1, "http://example.org/"));
        table.set_blocking(
            profile,
            BlockSettings {
                enabled: false,
                exceptions: Vec::new(),
            },
        );
        assert!(!table.blocking(1, "http://example.org/"));
    }

    #[test]
    fn the_paint_plan_is_the_windows_rules_per_view() {
        let mut table = Table::default();
        table.register(1, rules(false));
        table.paint_resize(1, 100, 100);
        // The engine paints its first picture whole whatever the plan says; a size change is "all".
        assert_eq!(table.paint_plan(1), PaintPlan::Nothing);
        table.paint_resize(1, 100, 100);
        assert_eq!(table.paint_plan(1), PaintPlan::Full);
        assert_eq!(table.paint_plan(1), PaintPlan::Nothing);
        table.paint_update(1, [0, 0, 100, 100], 0, -10, &[[0, 90, 100, 10]]);
        assert_eq!(
            table.paint_plan(1),
            PaintPlan::Delta {
                shift: Some([0, 0, 100, 100, 0, -10]),
                rects: vec![[0, 90, 100, 10]]
            }
        );
        table.paint_full(1);
        assert_eq!(table.paint_plan(1), PaintPlan::Full);
    }
}
