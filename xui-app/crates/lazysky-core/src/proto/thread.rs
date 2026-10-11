//! `app.bsky.feed.getPostThread`: the focused post, its ancestors and its
//! replies, flattened for a list.

use serde_json::Value;

use super::feed::Post;
use super::{text, type_of};

/// Threads nest as deep as people reply; hostile input must not recurse
/// without bound.
pub const MAX_DEPTH: usize = 40;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThreadNode {
    Post {
        post: Box<Post>,
        parent: Option<Box<ThreadNode>>,
        replies: Vec<ThreadNode>,
    },
    NotFound(String),
    Blocked(String),
}

/// A thread, with the post that was asked for at its root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thread {
    pub root: ThreadNode,
}

impl Thread {
    pub fn from_json(v: &Value) -> Option<Thread> {
        Some(Thread {
            root: node(v.get("thread")?, 0)?,
        })
    }

    /// The thread as list rows: ancestors from the oldest, the focused post,
    /// then replies depth first. Each row has its indent level (ancestors 0,
    /// the focus 0, replies from 1).
    pub fn rows(&self) -> Vec<(&Post, usize)> {
        let mut out = Vec::new();
        if let ThreadNode::Post {
            post,
            parent,
            replies,
        } = &self.root
        {
            let mut ancestors = Vec::new();
            let mut at = parent.as_deref();
            while let Some(ThreadNode::Post { post, parent, .. }) = at {
                ancestors.push(post.as_ref());
                at = parent.as_deref();
            }
            out.extend(ancestors.into_iter().rev().map(|p| (p, 0)));
            out.push((post.as_ref(), 0));
            for reply in replies {
                push_replies(reply, 1, &mut out);
            }
        }
        out
    }
}

fn push_replies<'a>(n: &'a ThreadNode, level: usize, out: &mut Vec<(&'a Post, usize)>) {
    if let ThreadNode::Post { post, replies, .. } = n {
        out.push((post.as_ref(), level));
        for reply in replies {
            push_replies(reply, level + 1, out);
        }
    }
}

fn node(v: &Value, depth: usize) -> Option<ThreadNode> {
    if depth > MAX_DEPTH {
        return None;
    }
    let uri = text(v, "uri").unwrap_or_default();
    match type_of(v) {
        "app.bsky.feed.defs#threadViewPost" => {
            let post = Box::new(Post::from_view(v.get("post")?)?);
            let parent = v
                .get("parent")
                .and_then(|p| node(p, depth + 1))
                .map(Box::new);
            let replies = v
                .get("replies")
                .and_then(Value::as_array)
                .map(|r| r.iter().filter_map(|n| node(n, depth + 1)).collect())
                .unwrap_or_default();
            Some(ThreadNode::Post {
                post,
                parent,
                replies,
            })
        }
        "app.bsky.feed.defs#notFoundPost" => Some(ThreadNode::NotFound(uri)),
        "app.bsky.feed.defs#blockedPost" => Some(ThreadNode::Blocked(uri)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn post(n: u32) -> Value {
        json!({"uri": format!("at://did:plc:a/app.bsky.feed.post/{n}"), "cid": "c",
               "author": {"did": "did:plc:a", "handle": "a.test"},
               "record": {"text": format!("post {n}")}})
    }

    fn tvp(n: u32, parent: Option<Value>, replies: Vec<Value>) -> Value {
        let mut v = json!({"$type": "app.bsky.feed.defs#threadViewPost", "post": post(n),
                           "replies": replies});
        if let Some(p) = parent {
            v["parent"] = p;
        }
        v
    }

    #[test]
    fn rows_run_ancestors_focus_then_replies() {
        let v = json!({"thread": tvp(
            3,
            Some(tvp(2, Some(tvp(1, None, vec![])), vec![])),
            vec![tvp(4, None, vec![tvp(5, None, vec![])]), tvp(6, None, vec![])],
        )});
        let thread = Thread::from_json(&v).unwrap();
        let rows: Vec<(String, usize)> = thread
            .rows()
            .into_iter()
            .map(|(p, level)| (p.text.clone(), level))
            .collect();
        let expect = [
            ("post 1", 0),
            ("post 2", 0),
            ("post 3", 0),
            ("post 4", 1),
            ("post 5", 2),
            ("post 6", 1),
        ];
        assert_eq!(rows, expect.map(|(t, l)| (t.to_string(), l)));
    }

    #[test]
    fn blocked_and_missing_replies_are_kept_out_of_the_rows() {
        let v = json!({"thread": tvp(1, None, vec![
            json!({"$type": "app.bsky.feed.defs#blockedPost", "uri": "at://x/y/z"}),
            json!({"$type": "app.bsky.feed.defs#notFoundPost", "uri": "at://x/y/w"}),
            tvp(2, None, vec![]),
        ])});
        let thread = Thread::from_json(&v).unwrap();
        assert_eq!(thread.rows().len(), 2);
    }

    #[test]
    fn a_missing_thread_is_none() {
        assert!(Thread::from_json(&json!({})).is_none());
        let nf =
            json!({"thread": {"$type": "app.bsky.feed.defs#notFoundPost", "uri": "at://x/y/z"}});
        assert!(Thread::from_json(&nf).unwrap().rows().is_empty());
    }

    #[test]
    fn absurd_depth_is_cut_not_followed() {
        let mut v = tvp(0, None, vec![]);
        for n in 1..200 {
            v = tvp(n, None, vec![v]);
        }
        let thread = Thread::from_json(&json!({"thread": v})).unwrap();
        assert!(thread.rows().len() <= MAX_DEPTH + 1);
    }
}
