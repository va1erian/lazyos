//! `app.bsky.notification.listNotifications` and `getUnreadCount`.

use serde_json::Value;

use super::actors::Actor;
use super::{count, text};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotificationKind {
    Like,
    Repost,
    Follow,
    Mention,
    Reply,
    Quote,
    /// A reason this client does not know, as the server spelled it.
    Other(String),
}

impl NotificationKind {
    fn from_reason(reason: &str) -> NotificationKind {
        match reason {
            "like" => NotificationKind::Like,
            "repost" => NotificationKind::Repost,
            "follow" => NotificationKind::Follow,
            "mention" => NotificationKind::Mention,
            "reply" => NotificationKind::Reply,
            "quote" => NotificationKind::Quote,
            other => NotificationKind::Other(other.chars().take(40).collect()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub uri: String,
    pub author: Actor,
    pub kind: NotificationKind,
    /// The post a like, repost or reply is about.
    pub subject: Option<String>,
    pub is_read: bool,
    pub indexed_at: String,
    /// The text of a reply, mention or quote.
    pub text: String,
}

impl Notification {
    fn from_json(v: &Value) -> Option<Notification> {
        Some(Notification {
            uri: text(v, "uri")?,
            author: Actor::from_json(v.get("author")?)?,
            kind: NotificationKind::from_reason(&text(v, "reason")?),
            subject: text(v, "reasonSubject"),
            is_read: v.get("isRead").and_then(Value::as_bool).unwrap_or(false),
            indexed_at: text(v, "indexedAt").unwrap_or_default(),
            text: v
                .get("record")
                .and_then(|r| text(r, "text"))
                .map(|mut t| {
                    super::embed::truncate(&mut t, 1000);
                    t
                })
                .unwrap_or_default(),
        })
    }

    /// One line, as the list shows it ("Alice liked your post").
    pub fn summary(&self) -> String {
        let who = self.author.name();
        match &self.kind {
            NotificationKind::Like => format!("{who} liked your post"),
            NotificationKind::Repost => format!("{who} reposted your post"),
            NotificationKind::Follow => format!("{who} followed you"),
            NotificationKind::Mention => format!("{who} mentioned you"),
            NotificationKind::Reply => format!("{who} replied to you"),
            NotificationKind::Quote => format!("{who} quoted your post"),
            NotificationKind::Other(reason) => format!("{who}: {reason}"),
        }
    }
}

/// A page of notifications and the cursor to the next one.
pub fn parse(v: &Value) -> (Vec<Notification>, Option<String>) {
    let items = v
        .get("notifications")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Notification::from_json).collect())
        .unwrap_or_default();
    (items, text(v, "cursor").filter(|c| !c.is_empty()))
}

/// `getUnreadCount`: `{count}`.
pub fn unread_count(v: &Value) -> u64 {
    count(v, "count")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn notifications_read_kinds_and_text() {
        let v = json!({"cursor": "c1", "notifications": [
            {"uri": "at://did:plc:b/app.bsky.feed.like/1", "cid": "c",
             "author": {"did": "did:plc:b", "handle": "b.test", "displayName": "Bob"},
             "reason": "like", "reasonSubject": "at://did:plc:me/app.bsky.feed.post/9",
             "record": {}, "isRead": false, "indexedAt": "2026-01-01T00:00:00Z"},
            {"uri": "at://did:plc:c/app.bsky.feed.post/2", "cid": "c",
             "author": {"did": "did:plc:c", "handle": "c.test"},
             "reason": "reply", "record": {"text": "nice one"}, "isRead": true},
            {"uri": "at://x/y/z", "author": {"did": "d", "handle": "h"}, "reason": "starterpack-joined"},
            {"nonsense": true},
        ]});
        let (items, cursor) = parse(&v);
        assert_eq!(cursor.as_deref(), Some("c1"));
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].summary(), "Bob liked your post");
        assert!(!items[0].is_read && items[1].is_read);
        assert_eq!(items[1].text, "nice one");
        assert_eq!(
            items[2].kind,
            NotificationKind::Other("starterpack-joined".into())
        );
    }

    #[test]
    fn the_unread_count() {
        assert_eq!(unread_count(&json!({"count": 7})), 7);
        assert_eq!(unread_count(&json!({})), 0);
    }
}
