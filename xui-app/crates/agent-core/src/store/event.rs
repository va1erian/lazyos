//! One line of a conversation file (docs/ai-agent-plan.md §9.3). A
//! conversation is its events in order; nothing is ever rewritten, so a
//! crash loses at most the line being written.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Persona {
    /// A plain chat.
    #[default]
    Chat,
    /// The diagnostician (§8.1).
    Diagnose,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Event {
    /// The first line of every file.
    Meta {
        id: String,
        created: u64,
        model: String,
        #[serde(default)]
        persona: Persona,
        /// Whether tools may be offered (the Diagnostics switch).
        #[serde(default)]
        diagnostics: bool,
        /// A free-text system prompt added by the user.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        system: Option<String>,
    },
    Title {
        title: String,
    },
    /// The model for the following turns changed.
    Model {
        model: String,
    },
    User {
        text: String,
    },
    Assistant {
        text: String,
        #[serde(default)]
        model: String,
        /// The stream broke or was stopped; the text is what arrived.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        interrupted: bool,
    },
    /// Kept in the file, never sent back (§4).
    Reasoning {
        text: String,
    },
    ToolCall {
        id: String,
        name: String,
        arguments: String,
    },
    ToolResult {
        id: String,
        name: String,
        text: String,
        /// One line standing for `text` once the turn is old (§4, Context).
        #[serde(default)]
        digest: String,
    },
    Usage {
        prompt: u64,
        completion: u64,
        #[serde(default)]
        reasoning: u64,
    },
    /// The first `covered_turns` live turns are replaced by `summary`.
    Compact {
        summary: String,
        covered_turns: usize,
    },
    /// Drops every turn from the one numbered `keep_turns` on (0-based):
    /// Regenerate and Edit replay a turn after it.
    Rewind {
        keep_turns: usize,
    },
}

impl Event {
    pub fn to_line(&self) -> String {
        serde_json::to_string(self).expect("an event serializes")
    }

    pub fn from_line(line: &str) -> Result<Event, String> {
        serde_json::from_str(line).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_round_trip_one_per_line() {
        let events = [
            Event::Meta {
                id: "c1".into(),
                created: 7,
                model: "m".into(),
                persona: Persona::Diagnose,
                diagnostics: true,
                system: Some("be brief".into()),
            },
            Event::User {
                text: "line one\nline \"two\"".into(),
            },
            Event::Assistant {
                text: "a".into(),
                model: "m".into(),
                interrupted: true,
            },
            Event::ToolResult {
                id: "t".into(),
                name: "n".into(),
                text: "r".into(),
                digest: "d".into(),
            },
            Event::Compact {
                summary: "s".into(),
                covered_turns: 3,
            },
            Event::Rewind { keep_turns: 1 },
        ];
        for event in events {
            let line = event.to_line();
            assert!(!line.contains('\n'), "{line}");
            assert_eq!(Event::from_line(&line).unwrap(), event);
        }
    }

    #[test]
    fn older_files_with_missing_fields_still_load() {
        let event = Event::from_line(r#"{"t":"meta","id":"c","created":1,"model":"m"}"#).unwrap();
        assert!(matches!(
            event,
            Event::Meta {
                persona: Persona::Chat,
                diagnostics: false,
                ..
            }
        ));
        assert!(Event::from_line(r#"{"t":"unknown_future"}"#).is_err());
        assert!(Event::from_line("garbage").is_err());
    }
}
