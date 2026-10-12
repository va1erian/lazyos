//! Reading: feeds, threads and people lists.

use crate::net::api::{Api, ApiError, Notes};
use crate::proto::{actors, notifications, Actor, Notification, Page, Profile, Thread};
use crate::{ActorList, AuthorFilter, FeedSource};

/// A page is this many posts.
const PAGE_SIZE: &str = "30";
/// A people list page.
const PEOPLE_PAGE: &str = "50";
/// How many levels of a thread the server is asked for.
const THREAD_DEPTH: &str = "6";

pub(super) fn feed(
    api: &mut Api,
    source: &FeedSource,
    cursor: Option<&str>,
    notes: &dyn Notes,
) -> Result<Page, ApiError> {
    let mut query: Vec<(&str, &str)> = vec![("limit", PAGE_SIZE)];
    if let Some(cursor) = cursor {
        query.push(("cursor", cursor));
    }
    match source {
        FeedSource::Timeline => {
            let v = api.get("app.bsky.feed.getTimeline", &query, notes)?;
            Ok(Page::from_feed(&v))
        }
        FeedSource::Author { actor, filter } => {
            query.push(("actor", actor));
            query.push((
                "filter",
                match filter {
                    AuthorFilter::Posts => "posts_no_replies",
                    AuthorFilter::Media => "posts_with_media",
                },
            ));
            let v = api.get("app.bsky.feed.getAuthorFeed", &query, notes)?;
            Ok(Page::from_feed(&v))
        }
        FeedSource::ActorLikes { actor } => {
            query.push(("actor", actor));
            let v = api.get("app.bsky.feed.getActorLikes", &query, notes)?;
            Ok(Page::from_feed(&v))
        }
        FeedSource::Bookmarks => {
            let v = api.get("app.bsky.bookmark.getBookmarks", &query, notes)?;
            Ok(Page::from_bookmarks(&v))
        }
        FeedSource::SearchPosts { query: text } => {
            query.push(("q", text));
            let v = api.get("app.bsky.feed.searchPosts", &query, notes)?;
            Ok(Page::from_posts(&v))
        }
    }
}

pub(super) fn thread(api: &mut Api, uri: &str, notes: &dyn Notes) -> Result<Thread, ApiError> {
    let v = api.get(
        "app.bsky.feed.getPostThread",
        &[("uri", uri), ("depth", THREAD_DEPTH)],
        notes,
    )?;
    Thread::from_json(&v)
        .ok_or_else(|| ApiError::Transport("that post is gone or cannot be shown".into()))
}

pub(super) fn actors(
    api: &mut Api,
    list: &ActorList,
    cursor: Option<&str>,
    notes: &dyn Notes,
) -> Result<(Vec<Actor>, Option<String>), ApiError> {
    let mut query: Vec<(&str, &str)> = vec![("limit", PEOPLE_PAGE)];
    if let Some(cursor) = cursor {
        query.push(("cursor", cursor));
    }
    let (nsid, found): (&str, fn(&serde_json::Value) -> Vec<Actor>) = match list {
        ActorList::LikedBy(uri) => {
            query.push(("uri", uri));
            ("app.bsky.feed.getLikes", actors::likers)
        }
        ActorList::RepostedBy(uri) => {
            query.push(("uri", uri));
            ("app.bsky.feed.getRepostedBy", |v| {
                actors::actors_of(v, "repostedBy")
            })
        }
        ActorList::Search(text) => {
            query.push(("q", text));
            ("app.bsky.actor.searchActors", |v| {
                actors::actors_of(v, "actors")
            })
        }
    };
    let v = api.get(nsid, &query, notes)?;
    let next = v
        .get("cursor")
        .and_then(|c| c.as_str())
        .filter(|c| !c.is_empty())
        .map(str::to_owned);
    Ok((found(&v), next))
}

pub(super) fn profile(api: &mut Api, actor: &str, notes: &dyn Notes) -> Result<Profile, ApiError> {
    let v = api.get("app.bsky.actor.getProfile", &[("actor", actor)], notes)?;
    Profile::from_json(&v).ok_or_else(|| ApiError::Transport("that account cannot be shown".into()))
}

pub(super) fn notifications(
    api: &mut Api,
    cursor: Option<&str>,
    notes: &dyn Notes,
) -> Result<(Vec<Notification>, Option<String>), ApiError> {
    let mut query: Vec<(&str, &str)> = vec![("limit", PAGE_SIZE)];
    if let Some(cursor) = cursor {
        query.push(("cursor", cursor));
    }
    let v = api.get("app.bsky.notification.listNotifications", &query, notes)?;
    Ok(notifications::parse(&v))
}

pub(super) fn unread(api: &mut Api, notes: &dyn Notes) -> Result<u64, ApiError> {
    let v = api.get("app.bsky.notification.getUnreadCount", &[], notes)?;
    Ok(notifications::unread_count(&v))
}
