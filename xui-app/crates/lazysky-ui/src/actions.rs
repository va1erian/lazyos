//! Optimistic edits to the posts a feed shows (redsky's
//! `update_post_optimistically`): a like or repost shows at once and is
//! rolled back if the server refuses it.

use std::sync::Arc;

use lazysky_core::feed::RowKind;
use lazysky_core::proto::feed::ViewerState;
use lazysky_core::proto::{FeedItem, Post};

use crate::feedview::ViewState;

/// The stand-in for a like or repost record the server has not confirmed.
/// A post carrying it ignores further clicks on that button.
pub const PENDING: &str = "at://pending";

/// What a post looked like before an optimistic edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Before {
    pub viewer: ViewerState,
    pub like_count: u64,
    pub repost_count: u64,
}

impl Before {
    pub fn of(post: &Post) -> Before {
        Before {
            viewer: post.viewer.clone(),
            like_count: post.like_count,
            repost_count: post.repost_count,
        }
    }

    pub fn restore(&self, post: &mut Post) {
        post.viewer = self.viewer.clone();
        post.like_count = self.like_count;
        post.repost_count = self.repost_count;
    }
}

/// The two buttons that toggle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toggle {
    Like,
    Repost,
}

impl Toggle {
    /// The record URI of the viewer's own like or repost.
    pub fn record(self, post: &Post) -> Option<&String> {
        match self {
            Toggle::Like => post.viewer.like.as_ref(),
            Toggle::Repost => post.viewer.repost.as_ref(),
        }
    }

    /// Shows the toggle switched (`on`) with `record` as its record.
    pub fn show(self, post: &mut Post, on: bool, record: Option<String>) {
        let (slot, count) = match self {
            Toggle::Like => (&mut post.viewer.like, &mut post.like_count),
            Toggle::Repost => (&mut post.viewer.repost, &mut post.repost_count),
        };
        if on && slot.is_none() {
            *count += 1;
        } else if !on && slot.is_some() {
            *count = count.saturating_sub(1);
        }
        *slot = if on { record } else { None };
    }

    /// Replaces the stand-in record with the real one.
    pub fn confirm(self, post: &mut Post, record: String) {
        match self {
            Toggle::Like => post.viewer.like = Some(record),
            Toggle::Repost => post.viewer.repost = Some(record),
        }
    }
}

/// Applies `edit` to every row of the feed that shows the post `uri`; the
/// rows are re-prepared (their old layout is drawn until the new one
/// arrives). Returns whether any row changed.
pub fn patch(state: &mut ViewState, uri: &str, edit: &dyn Fn(&mut Post)) -> bool {
    let mut changed = false;
    for i in 0..state.feed.len() {
        let Some(row) = state.feed.row_mut(i) else {
            continue;
        };
        let RowKind::Post(item) = &row.kind else {
            continue;
        };
        if item.post.uri != uri {
            continue;
        }
        let mut next = FeedItem::clone(item);
        edit(&mut next.post);
        row.kind = RowKind::Post(Arc::new(next));
        row.dirty = true;
        changed = true;
    }
    if changed {
        state.moved = true;
    }
    changed
}

/// The post `uri` as the feed shows it.
pub fn find(state: &ViewState, uri: &str) -> Option<Post> {
    state.feed.rows().iter().find_map(|row| match &row.kind {
        RowKind::Post(item) if item.post.uri == uri => Some(item.post.clone()),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prepare::Prepared;
    use lazysky_core::feed::FeedRow;
    use serde_json::json;

    fn post(n: u32) -> Post {
        Post::from_view(&json!({
            "uri": format!("at://did:plc:a/app.bsky.feed.post/{n}"), "cid": "c",
            "author": {"did": "did:plc:a", "handle": "a.test"},
            "record": {"text": "x"}, "likeCount": 3
        }))
        .unwrap()
    }

    fn state_with(posts: &[Post]) -> ViewState {
        let mut state = ViewState::new();
        let rows = posts
            .iter()
            .map(|p| {
                let item = Arc::new(FeedItem {
                    post: p.clone(),
                    reason: None,
                });
                let prepared = Prepared {
                    width: 100,
                    height: 50,
                    pieces: Vec::new(),
                    hits: Vec::new(),
                };
                FeedRow::post(item, 50, Some(prepared))
            })
            .collect();
        state.feed.replace_all(rows);
        state
    }

    #[test]
    fn a_like_shows_at_once_and_rolls_back() {
        let p = post(1);
        let before = Before::of(&p);
        let mut state = state_with(&[p.clone(), post(2)]);
        assert!(patch(&mut state, &p.uri, &|post| {
            Toggle::Like.show(post, true, Some(PENDING.into()));
        }));
        let shown = find(&state, &p.uri).unwrap();
        assert_eq!(shown.like_count, 4);
        assert_eq!(
            Toggle::Like.record(&shown).map(String::as_str),
            Some(PENDING)
        );
        assert!(state.feed.rows()[0].dirty && !state.feed.rows()[1].dirty);
        assert!(state.moved, "maintenance is due");

        patch(&mut state, &p.uri, &|post| before.restore(post));
        assert_eq!(Before::of(&find(&state, &p.uri).unwrap()), before);
    }

    #[test]
    fn confirming_keeps_the_count_and_swaps_the_record() {
        let mut p = post(1);
        Toggle::Repost.show(&mut p, true, Some(PENDING.into()));
        Toggle::Repost.confirm(&mut p, "at://did:plc:me/app.bsky.feed.repost/3k".into());
        assert_eq!(p.repost_count, 1);
        assert!(p.viewer.repost.as_deref().unwrap().ends_with("3k"));
        Toggle::Repost.show(&mut p, false, None);
        assert_eq!((p.repost_count, p.viewer.repost.clone()), (0, None));
        // Switching off again never underflows.
        Toggle::Repost.show(&mut p, false, None);
        assert_eq!(p.repost_count, 0);
    }

    #[test]
    fn every_row_of_the_same_post_changes() {
        let p = post(1);
        let mut state = state_with(&[p.clone(), post(2)]);
        // The same post appears twice (a repost): two rows carry it.
        let item = Arc::new(FeedItem {
            post: p.clone(),
            reason: None,
        });
        state.feed.append(vec![FeedRow::post(item, 50, None)]);
        // Dedupe keeps ids unique, so only the first stays; patch still
        // reaches every row that shows the URI.
        assert!(patch(&mut state, &p.uri, &|post| post.like_count = 9));
        assert!(!patch(
            &mut state,
            "at://nobody/app.bsky.feed.post/0",
            &|_| {}
        ));
    }
}
