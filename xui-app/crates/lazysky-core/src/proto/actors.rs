//! Accounts: the short view every post carries and the full profile.

use serde_json::Value;

use super::{count, text};

/// `app.bsky.actor.defs#profileViewBasic` (and `profileView`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Actor {
    pub did: String,
    pub handle: String,
    pub display_name: String,
    /// An `http(s)` avatar URL (the thumbnail the AppView serves).
    pub avatar: Option<String>,
    /// The URI of the viewer's follow record, when they follow this account.
    pub following: Option<String>,
}

impl Actor {
    /// `None` without a DID and a handle: such an account cannot be shown.
    pub fn from_json(v: &Value) -> Option<Actor> {
        let did = text(v, "did").filter(|s| !s.is_empty())?;
        let handle = text(v, "handle").filter(|s| !s.is_empty())?;
        Some(Actor {
            did,
            handle,
            display_name: text(v, "displayName")
                .unwrap_or_default()
                .trim()
                .to_string(),
            avatar: text(v, "avatar").filter(|u| web_url(u)),
            following: v.get("viewer").and_then(|viewer| text(viewer, "following")),
        })
    }

    /// What to call the account: the display name, else the handle.
    pub fn name(&self) -> &str {
        if self.display_name.is_empty() {
            &self.handle
        } else {
            &self.display_name
        }
    }
}

/// `app.bsky.actor.getProfile`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub actor: Actor,
    pub description: String,
    pub banner: Option<String>,
    pub followers: u64,
    pub follows: u64,
    pub posts: u64,
}

impl Profile {
    pub fn from_json(v: &Value) -> Option<Profile> {
        Some(Profile {
            actor: Actor::from_json(v)?,
            description: text(v, "description").unwrap_or_default(),
            banner: text(v, "banner").filter(|u| web_url(u)),
            followers: count(v, "followersCount"),
            follows: count(v, "followsCount"),
            posts: count(v, "postsCount"),
        })
    }
}

/// Actors of `v[key]`: `searchActors` (`actors`), `getRepostedBy`
/// (`repostedBy`). A malformed entry is skipped.
pub fn actors_of(v: &Value, key: &str) -> Vec<Actor> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Actor::from_json).collect())
        .unwrap_or_default()
}

/// The likers of `getLikes`: each entry wraps an `actor`.
pub fn likers(v: &Value) -> Vec<Actor> {
    v.get("likes")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|like| Actor::from_json(like.get("actor")?))
                .collect()
        })
        .unwrap_or_default()
}

/// Whether `url` is one the client may fetch or open.
pub(crate) fn web_url(url: &str) -> bool {
    let lower = url.get(..8).unwrap_or(url).to_ascii_lowercase();
    lower.starts_with("https://") || lower.starts_with("http://")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_actor_needs_a_did_and_a_handle() {
        assert!(Actor::from_json(&json!({"handle": "a.test"})).is_none());
        assert!(Actor::from_json(&json!({"did": "did:plc:a"})).is_none());
        let a = Actor::from_json(&json!({"did": "did:plc:a", "handle": "a.test"})).unwrap();
        assert_eq!(a.name(), "a.test");
    }

    #[test]
    fn display_names_win_and_hostile_avatars_are_dropped() {
        let a = Actor::from_json(&json!({
            "did": "did:plc:a", "handle": "a.test", "displayName": " Alice ",
            "avatar": "javascript:1", "viewer": {"following": "at://did:plc:me/app.bsky.graph.follow/3k"}
        }))
        .unwrap();
        assert_eq!(a.name(), "Alice");
        assert_eq!(a.avatar, None);
        assert!(a.following.is_some());
    }

    #[test]
    fn profiles_carry_counts() {
        let p = Profile::from_json(&json!({
            "did": "did:plc:a", "handle": "a.test", "followersCount": 12,
            "followsCount": 3, "postsCount": 40, "avatar": "https://cdn.test/a.jpg"
        }))
        .unwrap();
        assert_eq!((p.followers, p.follows, p.posts), (12, 3, 40));
    }

    #[test]
    fn lists_skip_bad_entries() {
        let v = json!({"likes": [{"actor": {"did": "d", "handle": "h"}}, {"actor": 3}, 7]});
        assert_eq!(likers(&v).len(), 1);
    }
}
