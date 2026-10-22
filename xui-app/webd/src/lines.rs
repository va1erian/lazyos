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
    /// A `NOTE` a client can make fire in a loop: printed under a budget.
    Flood(String),
    /// An engine line that can fire in a loop (a page's timing, the paint meter, a hung page's
    /// pings): printed under a budget of its own, so a client's refusals cannot silence the paint
    /// meter a harness reads, nor the other way round.
    EngineFlood(String),
}

/// The engine's markers (`webd_ui_say`, as the C++ half prints them without the `WEBD:NOTE:` prefix)
/// that a page or a client can make fire in a loop: a link click, a refused frame or history entry,
/// a refused base URL, an ignored configuration field, a duplicate view id, the ping of a hung page
/// and its recovery, and the per-navigation timing. A prefix, so `CONFIG:IGNORED:*` and the
/// `INFO:*` lines are covered whatever follows.
pub const FLOODING_ENGINE_LINES: &[&str] = &[
    "LIGHT:LINK",
    "LIGHT:REFUSED:",
    "LIGHT:BASE:REFUSED",
    "CONFIG:IGNORED:",
    "VIEW:DUPLICATE",
    "WEBPROCESS:RESPONSIVE:",
    "WEBPROCESS:UNRESPONSIVE:",
    "WEBPROCESS:RECOVER",
    "INFO:",
];

/// Whether an engine line is one of [`FLOODING_ENGINE_LINES`].
pub fn engine_line_floods(line: &str) -> bool {
    FLOODING_ENGINE_LINES
        .iter()
        .any(|prefix| line.starts_with(prefix))
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
        Note::Engine(line) if engine_line_floods(&line) => vec![Line::EngineFlood(line)],
        Note::Engine(line) => vec![Line::Note(line)],
        // A page can make these in a loop (dialogs, held navigations): rate limited.
        Note::Full { view, what } => vec![Line::Flood(format!("full view={view} {what}"))],
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
    }

    #[test]
    fn the_engines_per_page_markers_are_floods_and_its_lifecycle_markers_never_are() {
        let say = |text: &str| lines_of(Note::Engine(text.into()), &mut name);
        for text in [
            "LIGHT:LINK",
            "LIGHT:REFUSED:frame",
            "LIGHT:REFUSED:history",
            "LIGHT:BASE:REFUSED",
            "CONFIG:IGNORED:color_scheme",
            "CONFIG:IGNORED:user_style",
            "VIEW:DUPLICATE",
            "WEBPROCESS:UNRESPONSIVE:42",
            "WEBPROCESS:RESPONSIVE:42",
            "WEBPROCESS:RECOVER:SHARED:42",
            "INFO:TIME:nav=3:start=1:commit=2:finish=3:load=4",
        ] {
            assert_eq!(say(text), vec![Line::EngineFlood(text.into())], "{text}");
        }
        // What must not be lost stays a plain note.
        for text in [
            "PROCESS:LAUNCH:web:7",
            "PROCESS:LAUNCH:web:FAIL:errno=12",
            "WEBPROCESS:UP:7",
            "NETWORKPROCESS:UP:8",
            "FAIL:view:WKViewCreate returned null",
            "LIGHT:LOCK:READY",
            "PROCESS:EXIT:web:7:signal=9",
            "ENGINE:UP",
        ] {
            assert_eq!(say(text), vec![Line::Note(text.into())], "{text}");
        }
    }
}
