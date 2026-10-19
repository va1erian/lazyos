//! A conversation as transcript rows. Pure, host-tested.
//!
//! Row ids are positions: item `i` of the conversation is row `i * 4` (its
//! text) and `i * 4 + 1` (its reasoning, collapsed unless opened), so an
//! append never renumbers what is already on screen. The reply being streamed
//! and an error have ids of their own.

use std::collections::HashSet;
use std::sync::Arc;

use lazyagent_core::store::Item;
use lazyagent_core::Conversation;

use crate::model::{RowId, RowKind, RowSpec, LIVE};

/// The row of the reasoning being streamed.
pub const LIVE_THINKING: RowId = LIVE - 1;
/// The row of the last error.
pub const FAILURE: RowId = LIVE - 2;

/// What is arriving now.
#[derive(Default, Clone)]
pub struct Live {
    pub text: String,
    pub reasoning: String,
    /// A turn is running: an empty live reply shows as `...`.
    pub active: bool,
}

fn row(id: RowId, kind: RowKind, text: &str) -> (RowId, Arc<RowSpec>) {
    (
        id,
        Arc::new(RowSpec {
            kind,
            text: text.to_string(),
        }),
    )
}

/// The rows for `conversation`, the live reply and `failure`.
pub fn specs(
    conversation: Option<&Conversation>,
    live: &Live,
    pending_user: Option<&str>,
    failure: Option<&str>,
    opened: &HashSet<RowId>,
) -> Vec<(RowId, Arc<RowSpec>)> {
    let mut out = Vec::new();
    if let Some(conversation) = conversation {
        for (i, item) in conversation.items.iter().enumerate() {
            let id = i as u64 * 4;
            match item {
                Item::User { text } => out.push(row(id, RowKind::User, text)),
                Item::Assistant {
                    text,
                    reasoning,
                    interrupted,
                    ..
                } => {
                    if !reasoning.is_empty() {
                        out.push(row(
                            id + 1,
                            RowKind::Thinking {
                                open: opened.contains(&(id + 1)),
                            },
                            reasoning,
                        ));
                    }
                    if !text.is_empty() || *interrupted {
                        let shown = if *interrupted {
                            format!("{text}\n\n_The answer was interrupted._")
                        } else {
                            text.clone()
                        };
                        out.push(row(id, RowKind::Assistant, &shown));
                    }
                }
                Item::Summary { .. } => out.push(row(
                    id,
                    RowKind::Notice,
                    "Earlier turns were summarised to save room.",
                )),
                // Tool results are not shown in a plain chat (P5 adds cards).
                Item::ToolResult { .. } => {}
            }
        }
    }
    // The message just sent shows at once, under the id it will have.
    if let Some(text) = pending_user {
        let next = conversation.map_or(0, |c| c.items.len()) as u64 * 4;
        out.push(row(next, RowKind::User, text));
    }
    if !live.reasoning.is_empty() {
        out.push(row(
            LIVE_THINKING,
            RowKind::Thinking {
                open: opened.contains(&LIVE_THINKING),
            },
            &live.reasoning,
        ));
    }
    if !live.text.is_empty() || (live.active && live.reasoning.is_empty()) {
        out.push(row(LIVE, RowKind::Assistant, &live.text));
    }
    if let Some(failure) = failure {
        out.push(row(FAILURE, RowKind::Failure, failure));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use lazyagent_core::store::Event;
    use lazyagent_core::Persona;

    fn conversation(events: Vec<Event>) -> Conversation {
        let meta = Event::Meta {
            id: "c".into(),
            created: 1,
            model: "m".into(),
            persona: Persona::Chat,
            diagnostics: false,
            system: None,
        };
        Conversation::replay(std::iter::once(meta).chain(events)).unwrap()
    }

    #[test]
    fn items_become_rows_with_stable_ids() {
        let c = conversation(vec![
            Event::User { text: "q".into() },
            Event::Reasoning { text: "hmm".into() },
            Event::Assistant {
                text: "a".into(),
                model: "m".into(),
                interrupted: false,
            },
        ]);
        let rows = specs(Some(&c), &Live::default(), None, None, &HashSet::new());
        let ids: Vec<RowId> = rows.iter().map(|r| r.0).collect();
        assert_eq!(ids, [0, 5, 4]);
        assert_eq!(rows[1].1.kind, RowKind::Thinking { open: false });
        let open: HashSet<RowId> = [5].into();
        assert_eq!(
            specs(Some(&c), &Live::default(), None, None, &open)[1]
                .1
                .kind,
            RowKind::Thinking { open: true }
        );
        // An append leaves the earlier ids alone.
        let mut longer = c.clone();
        longer.apply(Event::User { text: "q2".into() });
        let again: Vec<RowId> = specs(Some(&longer), &Live::default(), None, None, &HashSet::new())
            .iter()
            .map(|r| r.0)
            .collect();
        assert_eq!(&again[..3], &ids[..]);
    }

    #[test]
    fn an_interrupted_answer_says_so_and_an_empty_one_still_shows() {
        let c = conversation(vec![
            Event::User { text: "q".into() },
            Event::Assistant {
                text: "half".into(),
                model: "m".into(),
                interrupted: true,
            },
        ]);
        let rows = specs(Some(&c), &Live::default(), None, None, &HashSet::new());
        assert!(rows[1].1.text.contains("interrupted"));
        let empty = conversation(vec![
            Event::User { text: "q".into() },
            Event::Assistant {
                text: String::new(),
                model: "m".into(),
                interrupted: true,
            },
        ]);
        assert_eq!(
            specs(Some(&empty), &Live::default(), None, None, &HashSet::new()).len(),
            2
        );
    }

    #[test]
    fn the_live_reply_and_the_failure_come_last() {
        let c = conversation(vec![Event::User { text: "q".into() }]);
        let live = Live {
            text: "so far".into(),
            reasoning: "thinking".into(),
            active: true,
        };
        let rows = specs(Some(&c), &live, None, Some("it broke"), &HashSet::new());
        let ids: Vec<RowId> = rows.iter().map(|r| r.0).collect();
        assert_eq!(ids, [0, LIVE_THINKING, LIVE, FAILURE]);
        let waiting = specs(
            Some(&c),
            &Live {
                active: true,
                ..Live::default()
            },
            None,
            None,
            &HashSet::new(),
        );
        assert_eq!(
            waiting.last().unwrap().0,
            LIVE,
            "a pending reply shows its placeholder"
        );
        assert_eq!(
            specs(None, &Live::default(), None, None, &HashSet::new()).len(),
            0
        );
    }

    #[test]
    fn a_message_just_sent_shows_under_the_id_it_will_have() {
        let c = conversation(vec![
            Event::User { text: "q".into() },
            Event::Assistant {
                text: "a".into(),
                model: "m".into(),
                interrupted: false,
            },
        ]);
        let rows = specs(
            Some(&c),
            &Live::default(),
            Some("next"),
            None,
            &HashSet::new(),
        );
        assert_eq!(rows.last().unwrap().0, 8);
        assert_eq!(rows.last().unwrap().1.kind, RowKind::User);
        let first = specs(None, &Live::default(), Some("hello"), None, &HashSet::new());
        assert_eq!(first[0].0, 0);
    }
}
