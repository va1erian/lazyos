//! A conversation as transcript rows. Pure, host-tested.
//!
//! Row ids are positions: item `i` of the conversation is row `i * 4` (its
//! text) and `i * 4 + 1` (its reasoning, collapsed unless opened), so an
//! append never renumbers what is already on screen. The reply being streamed
//! has the id it will have once the turn ends (so the reader's anchor and the
//! shaped text carry over to the finished row); an error has an id of its own.

use std::collections::HashSet;
use std::sync::Arc;

use lazyagent_core::store::Item;
use lazyagent_core::Conversation;

use crate::model::{RowId, RowKind, RowSpec, LIVE};

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

/// A turn that replaces the end of the conversation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Redo {
    #[default]
    No,
    /// The last answer is asked again.
    Answer,
    /// The last question is replaced.
    Edit,
}

/// How many items of `conversation` show: all, or (while a turn is redone)
/// the ones before what the new turn replaces, so the old answer (or
/// question) is gone while the new one streams.
fn shown_items(conversation: Option<&Conversation>, redo: Redo) -> usize {
    let Some(c) = conversation else { return 0 };
    let last_user = c.items.iter().rposition(|i| matches!(i, Item::User { .. }));
    match (redo, last_user) {
        (Redo::Answer, Some(i)) => i + 1,
        (Redo::Edit, Some(i)) => i,
        _ => c.items.len(),
    }
}

/// The rows for `conversation`, the live reply and `failure`.
pub fn specs(
    conversation: Option<&Conversation>,
    live: &Live,
    pending_user: Option<&str>,
    failure: Option<&str>,
    opened: &HashSet<RowId>,
    redo: Redo,
) -> Vec<(RowId, Arc<RowSpec>)> {
    let mut out = Vec::new();
    let count = shown_items(conversation, redo);
    if let Some(conversation) = conversation {
        for (i, item) in conversation.items.iter().enumerate().take(count) {
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
        out.push(row(count as u64 * 4, RowKind::User, text));
    }
    let live_id = (count + usize::from(pending_user.is_some())) as u64 * 4;
    if !live.reasoning.is_empty() {
        out.push(row(
            live_id + 1,
            RowKind::Thinking {
                open: opened.contains(&(live_id + 1)),
            },
            &live.reasoning,
        ));
    }
    if !live.text.is_empty() || (live.active && live.reasoning.is_empty()) {
        out.push(row(live_id, RowKind::Assistant, &live.text));
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
        let rows = specs(
            Some(&c),
            &Live::default(),
            None,
            None,
            &HashSet::new(),
            Redo::No,
        );
        let ids: Vec<RowId> = rows.iter().map(|r| r.0).collect();
        assert_eq!(ids, [0, 5, 4]);
        assert_eq!(rows[1].1.kind, RowKind::Thinking { open: false });
        let open: HashSet<RowId> = [5].into();
        assert_eq!(
            specs(Some(&c), &Live::default(), None, None, &open, Redo::No)[1]
                .1
                .kind,
            RowKind::Thinking { open: true }
        );
        // An append leaves the earlier ids alone.
        let mut longer = c.clone();
        longer.apply(Event::User { text: "q2".into() });
        let again: Vec<RowId> = specs(
            Some(&longer),
            &Live::default(),
            None,
            None,
            &HashSet::new(),
            Redo::No,
        )
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
        let rows = specs(
            Some(&c),
            &Live::default(),
            None,
            None,
            &HashSet::new(),
            Redo::No,
        );
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
            specs(
                Some(&empty),
                &Live::default(),
                None,
                None,
                &HashSet::new(),
                Redo::No
            )
            .len(),
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
        let rows = specs(
            Some(&c),
            &live,
            None,
            Some("it broke"),
            &HashSet::new(),
            Redo::No,
        );
        let ids: Vec<RowId> = rows.iter().map(|r| r.0).collect();
        // The live rows have the ids the finished item will have.
        assert_eq!(ids, [0, 5, 4, FAILURE]);
        let waiting = specs(
            Some(&c),
            &Live {
                active: true,
                ..Live::default()
            },
            None,
            None,
            &HashSet::new(),
            Redo::No,
        );
        assert_eq!(
            waiting.last().unwrap().0,
            4,
            "a pending reply shows its placeholder"
        );
        assert_eq!(
            specs(
                None,
                &Live::default(),
                None,
                None,
                &HashSet::new(),
                Redo::No
            )
            .len(),
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
            Redo::No,
        );
        assert_eq!(rows.last().unwrap().0, 8);
        assert_eq!(rows.last().unwrap().1.kind, RowKind::User);
        let first = specs(
            None,
            &Live::default(),
            Some("hello"),
            None,
            &HashSet::new(),
            Redo::No,
        );
        assert_eq!(first[0].0, 0);
    }

    #[test]
    fn a_finished_reply_keeps_the_id_its_live_row_had() {
        let live = Live {
            text: "so far".into(),
            reasoning: String::new(),
            active: true,
        };
        let before = conversation(vec![Event::User { text: "q".into() }]);
        let streaming = specs(Some(&before), &live, None, None, &HashSet::new(), Redo::No);
        let live_id = streaming.last().unwrap().0;
        let after = conversation(vec![
            Event::User { text: "q".into() },
            Event::Assistant {
                text: "so far".into(),
                model: "m".into(),
                interrupted: false,
            },
        ]);
        let done = specs(
            Some(&after),
            &Live::default(),
            None,
            None,
            &HashSet::new(),
            Redo::No,
        );
        assert_eq!(done.last().unwrap().0, live_id);
        // With a message just sent the live reply is the item after it.
        let first = specs(None, &live, Some("hi"), None, &HashSet::new(), Redo::No);
        assert_eq!(first.iter().map(|r| r.0).collect::<Vec<_>>(), [0, 4]);
    }

    #[test]
    fn redoing_hides_the_old_answer_and_streams_under_the_same_id() {
        let c = conversation(vec![
            Event::User { text: "q".into() },
            Event::Assistant {
                text: "old".into(),
                model: "m".into(),
                interrupted: false,
            },
        ]);
        let live = Live {
            text: "new".into(),
            reasoning: String::new(),
            active: true,
        };
        let rows = specs(Some(&c), &live, None, None, &HashSet::new(), Redo::Answer);
        assert_eq!(rows.iter().map(|r| r.0).collect::<Vec<_>>(), [0, 4]);
        assert_eq!(rows[1].1.text, "new");
    }

    #[test]
    fn editing_hides_the_old_question_too() {
        let c = conversation(vec![
            Event::User { text: "q".into() },
            Event::Assistant {
                text: "old".into(),
                model: "m".into(),
                interrupted: false,
            },
        ]);
        let live = Live {
            text: String::new(),
            reasoning: String::new(),
            active: true,
        };
        let rows = specs(
            Some(&c),
            &live,
            Some("q2"),
            None,
            &HashSet::new(),
            Redo::Edit,
        );
        assert_eq!(rows.iter().map(|r| r.0).collect::<Vec<_>>(), [0, 4]);
        assert_eq!(rows[0].1.text, "q2");
    }
}
