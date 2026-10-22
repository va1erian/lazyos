//! The ordered queue of things that happened once, bounded without losing what someone waits for.
//!
//! The queue is one for every view of every client, so a page that makes news in a loop (link clicks,
//! copies, `window.open`, notifications) must not push another view's question out of it: a dropped
//! `Dialog` would leave that page frozen, a dropped `Held` its navigation held, a dropped `Lost` its client
//! waiting for a picture. So past [`QUEUE_KEPT`] the oldest event that nobody waits for goes first; what
//! someone waits for is kept, and is bounded by the engine itself (a page waits on one dialog at a time, a
//! view is lost once, a newer main-frame navigation drops an older held one, which [`Inner::push`]
//! coalesces, downloads are capped per profile, permission requests need a gesture and are capped per view).
//! [`HARD_KEPT`] is the backstop if that reasoning is ever wrong: then the oldest goes, whatever it is.

use super::{Inner, Once};
use crate::limits::QUEUE_KEPT;

/// The most events kept at all.
pub const HARD_KEPT: usize = 4 * QUEUE_KEPT;

impl Once {
    /// Someone waits for this one: the engine (an answer), or the client (the end of something it
    /// started). Everything else is news a later event supersedes or nobody misses.
    pub fn awaited(&self) -> bool {
        matches!(
            self,
            Once::Lost { .. }
                | Once::Held { .. }
                | Once::HandOff { .. }
                | Once::Dialog { .. }
                | Once::DownloadStarted { .. }
                | Once::DownloadDone { .. }
                | Once::DownloadFailed { .. }
                | Once::NotifyAsk { .. }
                | Once::BlockingList { .. }
        )
    }
}

impl Inner {
    /// Queue `event`, keeping the queue bounded (see the module).
    pub(super) fn push(&mut self, event: Once) {
        // The engine refused a view's older held navigation when it held a newer one: only the newest
        // is worth asking about (an answer to the older one is ignored by the engine anyway).
        if let Once::Held { view, .. } = &event {
            let view = *view;
            self.once
                .retain(|old| !matches!(old, Once::Held { view: v, .. } if *v == view));
        }
        if self.once.len() >= QUEUE_KEPT {
            match self.once.iter().position(|old| !old.awaited()) {
                Some(at) => {
                    self.once.remove(at);
                    self.dropped += 1;
                }
                None if !event.awaited() => {
                    self.dropped += 1;
                    return;
                }
                None if self.once.len() >= HARD_KEPT => {
                    self.once.pop_front();
                    self.dropped += 1;
                }
                None => {}
            }
        }
        self.once.push_back(event);
    }
}

#[cfg(test)]
mod tests {
    use super::super::EventQueue;
    use super::*;
    use crate::engine::EngineEvents;

    #[test]
    fn a_flood_of_news_does_not_push_another_views_question_out() {
        let queue = EventQueue::new();
        queue.dialog(1, 7, 0, "hello", "");
        queue.view_lost(2, 2);
        for n in 0..QUEUE_KEPT * 3 {
            queue.link_clicked(3, &format!("https://x/{n}"));
        }
        let once = queue.drain().once;
        assert_eq!(once.len(), QUEUE_KEPT);
        assert!(once.contains(&Once::Dialog {
            view: 1,
            dialog: 7,
            kind: 0,
            message: "hello".into(),
            default: String::new()
        }));
        assert!(once.contains(&Once::Lost { view: 2, reason: 2 }));
    }

    #[test]
    fn only_the_newest_held_navigation_of_a_view_is_kept() {
        let mut inner = Inner::default();
        for request in 1..=5 {
            inner.push(Once::Held {
                view: 1,
                request,
                url: "mailto:a@b".into(),
                kind: 5,
                gesture: false,
            });
        }
        inner.push(Once::Held {
            view: 2,
            request: 9,
            url: "mailto:a@b".into(),
            kind: 5,
            gesture: false,
        });
        let held: Vec<(u64, u64)> = inner
            .once
            .iter()
            .filter_map(|e| match e {
                Once::Held { view, request, .. } => Some((*view, *request)),
                _ => None,
            })
            .collect();
        assert_eq!(held, vec![(1, 5), (2, 9)]);
    }

    #[test]
    fn awaited_events_have_a_hard_bound() {
        let mut inner = Inner::default();
        for view in 0..(HARD_KEPT as u64 + 10) {
            inner.push(Once::Lost { view, reason: 1 });
        }
        assert_eq!(inner.once.len(), HARD_KEPT);
        assert_eq!(inner.dropped, 10);
    }
}
