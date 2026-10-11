//! Posts and the pages of them the feed endpoints return.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use serde_json::Value;

use super::actors::Actor;
use super::embed::{self, Embed, MAX_TEXT_BYTES};
use super::facets::{self, Facet};
use super::{count, text};

/// `{uri, cid}`: a record pinned to a version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrongRef {
    pub uri: String,
    pub cid: String,
}

impl StrongRef {
    pub fn from_json(v: &Value) -> Option<StrongRef> {
        Some(StrongRef {
            uri: text(v, "uri").filter(|s| s.starts_with("at://"))?,
            cid: text(v, "cid").filter(|s| !s.is_empty())?,
        })
    }

    pub fn to_json(&self) -> Value {
        serde_json::json!({"uri": self.uri, "cid": self.cid})
    }
}

/// What the signed-in user has done to a post: the URIs of their like and
/// repost records (needed to undo them).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ViewerState {
    pub like: Option<String>,
    pub repost: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Post {
    pub uri: String,
    pub cid: String,
    pub author: Actor,
    pub text: String,
    pub facets: Vec<Facet>,
    pub created_at: String,
    pub indexed_at: String,
    pub embed: Embed,
    pub reply_count: u64,
    pub repost_count: u64,
    pub like_count: u64,
    pub quote_count: u64,
    pub viewer: ViewerState,
    /// The thread this post replies into, from its record.
    pub reply: Option<(StrongRef, StrongRef)>,
    /// The post as the server sent it, for the Raw view.
    pub raw: Arc<Value>,
}

impl Post {
    /// Reads a `postView`. `None` when it lacks what a row needs.
    pub fn from_view(v: &Value) -> Option<Post> {
        let author = Actor::from_json(v.get("author")?)?;
        let record = v.get("record")?;
        let mut body = text(record, "text").unwrap_or_default();
        embed::truncate(&mut body, MAX_TEXT_BYTES);
        let viewer = v.get("viewer");
        let viewer_uri = |key: &str| {
            viewer
                .and_then(|state| text(state, key))
                .filter(|s| s.starts_with("at://"))
        };
        let indexed_at = text(v, "indexedAt").unwrap_or_default();
        let reply = record.get("reply").and_then(|r| {
            Some((
                StrongRef::from_json(r.get("root")?)?,
                StrongRef::from_json(r.get("parent")?)?,
            ))
        });
        Some(Post {
            uri: text(v, "uri").filter(|s| s.starts_with("at://"))?,
            cid: text(v, "cid").unwrap_or_default(),
            author,
            facets: facets::parse(&body, record),
            created_at: text(record, "createdAt").unwrap_or_else(|| indexed_at.clone()),
            text: body,
            indexed_at,
            embed: v.get("embed").map_or(Embed::None, Embed::parse),
            reply_count: count(v, "replyCount"),
            repost_count: count(v, "repostCount"),
            like_count: count(v, "likeCount"),
            quote_count: count(v, "quoteCount"),
            viewer: ViewerState {
                like: viewer_uri("like"),
                repost: viewer_uri("repost"),
            },
            reply,
            raw: Arc::new(v.clone()),
        })
    }

    pub fn strong_ref(&self) -> StrongRef {
        StrongRef {
            uri: self.uri.clone(),
            cid: self.cid.clone(),
        }
    }

    /// The thread root a reply to this post must name: this post's own root,
    /// or this post when it starts a thread.
    pub fn thread_root(&self) -> StrongRef {
        self.reply
            .as_ref()
            .map_or_else(|| self.strong_ref(), |(root, _)| root.clone())
    }

    /// The rkey of the post (the last URI segment).
    pub fn rkey(&self) -> &str {
        self.uri.rsplit('/').next().unwrap_or("")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    /// "reposted by": the account that reposted it.
    Repost { by: Actor },
}

/// One entry of a feed: a post and why it is there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedItem {
    pub post: Post,
    pub reason: Option<Reason>,
}

impl FeedItem {
    /// A stable id for the row: a hash of the post URI and the reposter, so
    /// the same post reposted twice is two rows and a refresh keeps ids.
    pub fn row_id(&self) -> u64 {
        let mut h = DefaultHasher::new();
        self.post.uri.hash(&mut h);
        if let Some(Reason::Repost { by }) = &self.reason {
            by.did.hash(&mut h);
        }
        h.finish()
    }
}

/// A page of a feed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Page {
    pub items: Vec<FeedItem>,
    pub cursor: Option<String>,
    /// Entries that could not be read (blocked, deleted, malformed).
    pub skipped: usize,
}

impl Page {
    /// `getTimeline`, `getAuthorFeed`, `getActorLikes`: `{feed: [{post,
    /// reason?}], cursor?}`.
    pub fn from_feed(v: &Value) -> Page {
        page(v, "feed", |entry| {
            let post = Post::from_view(entry.get("post")?)?;
            let reason = entry.get("reason").and_then(reason_of);
            Some(FeedItem { post, reason })
        })
    }

    /// `searchPosts`: `{posts: [postView], cursor?}`.
    pub fn from_posts(v: &Value) -> Page {
        page(v, "posts", |entry| {
            Some(FeedItem {
                post: Post::from_view(entry)?,
                reason: None,
            })
        })
    }

    /// `getBookmarks`: `{bookmarks: [{item: postView}], cursor?}`.
    pub fn from_bookmarks(v: &Value) -> Page {
        page(v, "bookmarks", |entry| {
            Some(FeedItem {
                post: Post::from_view(entry.get("item")?)?,
                reason: None,
            })
        })
    }
}

fn page(v: &Value, key: &str, item: impl Fn(&Value) -> Option<FeedItem>) -> Page {
    let mut out = Page {
        cursor: text(v, "cursor").filter(|c| !c.is_empty()),
        ..Page::default()
    };
    for entry in v.get(key).and_then(Value::as_array).into_iter().flatten() {
        match item(entry) {
            Some(parsed) => out.items.push(parsed),
            None => out.skipped += 1,
        }
    }
    out
}

fn reason_of(v: &Value) -> Option<Reason> {
    match super::type_of(v) {
        "app.bsky.feed.defs#reasonRepost" => Some(Reason::Repost {
            by: Actor::from_json(v.get("by")?)?,
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn view(n: u32) -> Value {
        json!({
            "uri": format!("at://did:plc:a/app.bsky.feed.post/{n}"), "cid": "bafyrei",
            "author": {"did": "did:plc:a", "handle": "a.test", "displayName": "Alice"},
            "record": {"text": "hello", "createdAt": "2026-01-01T10:00:00Z"},
            "indexedAt": "2026-01-01T10:00:01Z", "likeCount": 4, "repostCount": 2,
            "viewer": {"like": "at://did:plc:me/app.bsky.feed.like/9"}
        })
    }

    #[test]
    fn a_post_reads_its_counts_and_viewer_state() {
        let p = Post::from_view(&view(1)).unwrap();
        assert_eq!((p.like_count, p.repost_count, p.reply_count), (4, 2, 0));
        assert!(p.viewer.like.is_some() && p.viewer.repost.is_none());
        assert_eq!(p.rkey(), "1");
        assert_eq!(
            p.thread_root().uri,
            p.uri,
            "a thread starter is its own root"
        );
    }

    #[test]
    fn replies_name_their_root() {
        let mut v = view(2);
        v["record"]["reply"] = json!({
            "root": {"uri": "at://did:plc:r/app.bsky.feed.post/1", "cid": "bafyroot"},
            "parent": {"uri": "at://did:plc:p/app.bsky.feed.post/5", "cid": "bafyparent"}});
        let p = Post::from_view(&v).unwrap();
        assert_eq!(p.thread_root().cid, "bafyroot");
    }

    #[test]
    fn a_feed_page_skips_what_it_cannot_read() {
        let v = json!({"cursor": "next", "feed": [
            {"post": view(1)},
            {"post": {"uri": "at://x/y/z", "cid": "c"}},          // no author or record
            {"post": {"$type": "app.bsky.feed.defs#blockedPost"}},
            {"post": view(2), "reason": {"$type": "app.bsky.feed.defs#reasonRepost",
                "by": {"did": "did:plc:b", "handle": "b.test"}, "indexedAt": "2026-01-01T11:00:00Z"}},
            {"post": view(3), "reason": {"$type": "app.bsky.feed.defs#reasonPin"}},
        ]});
        let page = Page::from_feed(&v);
        assert_eq!(page.items.len(), 3);
        assert_eq!(page.skipped, 2);
        assert_eq!(page.cursor.as_deref(), Some("next"));
        assert!(
            matches!(&page.items[1].reason, Some(Reason::Repost { by }) if by.handle == "b.test")
        );
        assert!(
            page.items[2].reason.is_none(),
            "an unknown reason is no reason"
        );
    }

    #[test]
    fn row_ids_tell_a_repost_from_the_original() {
        let post = Post::from_view(&view(1)).unwrap();
        let plain = FeedItem {
            post: post.clone(),
            reason: None,
        };
        let by = Actor::from_json(&json!({"did": "did:plc:b", "handle": "b.test"})).unwrap();
        let reposted = FeedItem {
            post,
            reason: Some(Reason::Repost { by }),
        };
        assert_ne!(plain.row_id(), reposted.row_id());
        assert_eq!(plain.row_id(), plain.clone().row_id());
    }

    #[test]
    fn huge_text_is_cut_and_facets_beyond_it_dropped() {
        let mut v = view(1);
        v["record"]["text"] = json!("x".repeat(MAX_TEXT_BYTES + 500));
        v["record"]["facets"] = json!([{"index": {"byteStart": MAX_TEXT_BYTES + 10, "byteEnd": MAX_TEXT_BYTES + 20},
            "features": [{"$type": "app.bsky.richtext.facet#link", "uri": "https://a.test"}]}]);
        let p = Post::from_view(&v).unwrap();
        assert_eq!(p.text.len(), MAX_TEXT_BYTES);
        assert!(p.facets.is_empty());
    }

    #[test]
    fn bookmarks_and_search_pages() {
        let b = json!({"bookmarks": [{"createdAt": "x", "item": view(1)}, {"item": {"$type": "x#notFound"}}]});
        let page = Page::from_bookmarks(&b);
        assert_eq!((page.items.len(), page.skipped), (1, 1));
        let s = json!({"posts": [view(1), view(2)]});
        assert_eq!(Page::from_posts(&s).items.len(), 2);
    }
}
