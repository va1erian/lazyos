//! The service's notes as markers. `xui-webd` cannot print (it knows no trace channel), so it
//! reports what happened as [`Note`]s and the host turns them into the `WEBD:*` lines a harness
//! reads. This module is the pure half of that: what to say, not how to write it.
//!
//! ```text
//! WEBD:VIEW:CREATE client=<label> view=<id>
//! WEBD:VIEW:DESTROY view=<id>
//! WEBD:LOST view=<id> reason=<refused|crashed|memory|other|unknown>
//! WEBD:NOTE:<text>
//! ```
//!
//! `<label>` is the caller's package label (`app:os.lazy.webddemo`), `none` for a caller without one
//! (a script run from the Terminal). Notes that can repeat in a loop (a client that does not read its
//! frames, a client that sends bad requests) are [`Line::Flood`]: the host prints them under a rate
//! budget so a misbehaving client cannot fill the console.

use xui_webd::ident::ClientKey;
use xui_webd::Note;

/// One marker to print.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Line {
    /// `WEBD:VIEW:CREATE client=<label> view=<id>`.
    Create { client: String, view: u64 },
    /// `WEBD:VIEW:DESTROY view=<id>`.
    Destroy { view: u64 },
    /// `WEBD:LOST view=<id> reason=<name>`.
    Lost { view: u64, reason: &'static str },
    /// `WEBD:NOTE:<text>`.
    Note(String),
    /// A `NOTE` that can fire in a loop: printed under a budget.
    Flood(String),
}

/// The word for a `LostReason` wire value.
pub fn reason_name(reason: u32) -> &'static str {
    match reason {
        1 => "refused",
        2 => "crashed",
        3 => "memory",
        4 => "other",
        _ => "unknown",
    }
}

/// The markers for one note. `client_name` names a caller (the label's text); it is asked only for
/// a creation.
pub fn lines_of(note: Note, client_name: &mut dyn FnMut(ClientKey) -> String) -> Vec<Line> {
    match note {
        Note::ViewCreated { view, client, .. } => vec![Line::Create {
            client: client_name(client),
            view,
        }],
        Note::ViewEnded { view, why } => {
            let mut lines = vec![Line::Destroy { view }];
            // The client asking is the ordinary end; the others are worth a word.
            if why != "destroyed" {
                lines.push(Line::Note(format!("view {view} ended: {why}")));
            }
            lines
        }
        Note::ViewLost { view, reason } => vec![Line::Lost {
            view,
            reason: reason_name(reason),
        }],
        Note::Refused { method, code, text } => vec![Line::Flood(format!(
            "refused method={method} errno={} {text}",
            code.unsigned_abs()
        ))],
        Note::FrameDropped { view, why } => {
            vec![Line::Flood(format!("frame dropped view={view}: {why}"))]
        }
        Note::Engine(line) => vec![Line::Note(line)],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(label: u32) -> ClientKey {
        ClientKey {
            uid: 1000,
            label,
            task: 0,
        }
    }

    fn name(key: ClientKey) -> String {
        if key.label == 0 {
            "none".into()
        } else {
            format!("app:label{}", key.label)
        }
    }

    #[test]
    fn a_creation_names_the_client_and_the_view() {
        let note = Note::ViewCreated {
            view: 7,
            client: client(3),
            width: 100,
            height: 50,
            group: 1,
        };
        assert_eq!(
            lines_of(note, &mut name),
            vec![Line::Create {
                client: "app:label3".into(),
                view: 7
            }]
        );
    }

    #[test]
    fn a_destroy_the_client_asked_for_is_one_line_and_any_other_end_adds_a_note() {
        let asked = Note::ViewEnded {
            view: 2,
            why: "destroyed",
        };
        assert_eq!(lines_of(asked, &mut name), vec![Line::Destroy { view: 2 }]);
        let gone = Note::ViewEnded {
            view: 2,
            why: "client gone",
        };
        assert_eq!(
            lines_of(gone, &mut name),
            vec![
                Line::Destroy { view: 2 },
                Line::Note("view 2 ended: client gone".into())
            ]
        );
    }

    #[test]
    fn a_lost_view_says_why_in_a_word() {
        for (wire, word) in [
            (1, "refused"),
            (2, "crashed"),
            (3, "memory"),
            (4, "other"),
            (0, "unknown"),
            (99, "unknown"),
        ] {
            let note = Note::ViewLost {
                view: 5,
                reason: wire,
            };
            assert_eq!(
                lines_of(note, &mut name),
                vec![Line::Lost {
                    view: 5,
                    reason: word
                }]
            );
        }
    }

    #[test]
    fn what_can_repeat_is_a_flood_and_the_engine_speaks_in_notes() {
        let refused = Note::Refused {
            method: 3,
            code: -16,
            text: "slot is busy",
        };
        assert_eq!(
            lines_of(refused, &mut name),
            vec![Line::Flood("refused method=3 errno=16 slot is busy".into())]
        );
        let dropped = Note::FrameDropped {
            view: 1,
            why: "no free slot",
        };
        assert!(matches!(
            lines_of(dropped, &mut name)[0],
            Line::Flood(ref text) if text.contains("no free slot")
        ));
        assert_eq!(
            lines_of(
                Note::Engine("CONFIG:IGNORED:color_scheme".into()),
                &mut name
            ),
            vec![Line::Note("CONFIG:IGNORED:color_scheme".into())]
        );
    }
}
