//! A conversation as the replay of its events, and the messages it sends.

use super::event::{Event, Persona};
use crate::message::{Message, ToolCall, Usage};

/// What a conversation holds, in order. A turn starts at each `User`.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    User {
        text: String,
    },
    Assistant {
        text: String,
        reasoning: String,
        model: String,
        interrupted: bool,
        tool_calls: Vec<ToolCall>,
    },
    ToolResult {
        id: String,
        name: String,
        text: String,
        digest: String,
    },
    /// Stands for the turns a compaction replaced; only ever first.
    Summary {
        text: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Conversation {
    pub id: String,
    pub created: u64,
    pub model: String,
    pub persona: Persona,
    pub diagnostics: bool,
    pub system: Option<String>,
    pub title: Option<String>,
    pub items: Vec<Item>,
    /// Everything the conversation has cost, summed over its requests.
    pub usage: Usage,
    /// The last request's prompt size, and how many items it covered.
    last_prompt: u64,
    items_at_last_prompt: usize,
    pending_reasoning: String,
}

/// Tool results of older turns than this many are sent as digests (§4).
const FULL_TURNS: usize = 2;

impl Conversation {
    /// Replays `events`; `None` when the first one is not a `Meta`. Events
    /// that make no sense in place (a result with no call) are kept as data.
    pub fn replay(events: impl IntoIterator<Item = Event>) -> Option<Conversation> {
        let mut events = events.into_iter();
        let Some(Event::Meta {
            id,
            created,
            model,
            persona,
            diagnostics,
            system,
        }) = events.next()
        else {
            return None;
        };
        let mut conversation = Conversation {
            id,
            created,
            model,
            persona,
            diagnostics,
            system,
            title: None,
            items: Vec::new(),
            usage: Usage::default(),
            last_prompt: 0,
            items_at_last_prompt: 0,
            pending_reasoning: String::new(),
        };
        for event in events {
            conversation.apply(event);
        }
        Some(conversation)
    }

    pub fn apply(&mut self, event: Event) {
        match event {
            Event::Meta { .. } => {}
            Event::Title { title } => self.title = Some(title),
            Event::Model { model } => self.model = model,
            Event::User { text } => {
                self.pending_reasoning.clear();
                self.items.push(Item::User { text });
            }
            Event::Reasoning { text } => self.pending_reasoning.push_str(&text),
            Event::Assistant {
                text,
                model,
                interrupted,
            } => {
                let reasoning = std::mem::take(&mut self.pending_reasoning);
                self.items.push(Item::Assistant {
                    text,
                    reasoning,
                    model,
                    interrupted,
                    tool_calls: Vec::new(),
                });
            }
            Event::ToolCall {
                id,
                name,
                arguments,
            } => {
                if !matches!(self.items.last(), Some(Item::Assistant { .. })) {
                    self.items.push(Item::Assistant {
                        text: String::new(),
                        reasoning: std::mem::take(&mut self.pending_reasoning),
                        model: self.model.clone(),
                        interrupted: false,
                        tool_calls: Vec::new(),
                    });
                }
                if let Some(Item::Assistant { tool_calls, .. }) = self.items.last_mut() {
                    tool_calls.push(ToolCall {
                        id,
                        name,
                        arguments,
                    });
                }
            }
            Event::ToolResult {
                id,
                name,
                text,
                digest,
            } => self.items.push(Item::ToolResult {
                id,
                name,
                text,
                digest,
            }),
            Event::Usage {
                prompt,
                completion,
                reasoning,
            } => {
                self.usage.prompt += prompt;
                self.usage.completion += completion;
                self.usage.reasoning += reasoning;
                self.last_prompt = prompt;
                self.items_at_last_prompt = self.items.len();
            }
            Event::Compact {
                summary,
                covered_turns,
            } => {
                let covered = covered_turns.min(self.turns());
                if covered == 0 {
                    return;
                }
                let cut = self.turn_start(covered);
                self.items.drain(..cut);
                self.items.insert(0, Item::Summary { text: summary });
                self.items_at_last_prompt = 0;
                self.last_prompt = 0;
            }
            Event::Rewind { keep_turns } => {
                let cut = self.turn_start(keep_turns);
                self.items.truncate(cut);
                self.items_at_last_prompt = self.items_at_last_prompt.min(cut);
                self.pending_reasoning.clear();
            }
        }
    }

    /// The number of turns (user messages) the conversation holds.
    pub fn turns(&self) -> usize {
        self.items
            .iter()
            .filter(|i| matches!(i, Item::User { .. }))
            .count()
    }

    /// The index of the first item of turn `n` (0-based); `items.len()` for
    /// `n` past the last turn.
    pub fn turn_start(&self, n: usize) -> usize {
        self.items
            .iter()
            .enumerate()
            .filter(|(_, i)| matches!(i, Item::User { .. }))
            .nth(n)
            .map_or(self.items.len(), |(index, _)| index)
    }

    /// The wire messages for the next request: the system prompt (with the
    /// summary of compacted turns in it), then every turn. Reasoning is
    /// never sent. Tool results of turns older than the last two go as their
    /// digest. A tool call with no recorded result (a crash, a stop) is left
    /// out with nothing dangling, since the provider refuses an unanswered
    /// call.
    pub fn messages(&self, system_prompt: &str) -> Vec<Message> {
        let mut system = system_prompt.to_string();
        if let Some(custom) = self.system.as_deref().filter(|s| !s.trim().is_empty()) {
            system.push_str("\n\n");
            system.push_str(custom);
        }
        let first = match self.items.first() {
            Some(Item::Summary { text }) => {
                system.push_str(
                    "\n\nThe earlier part of this conversation was replaced by this summary:\n",
                );
                system.push_str(text);
                1
            }
            _ => 0,
        };
        let mut out = vec![Message::system(system)];
        let full_from = self.turns().saturating_sub(FULL_TURNS);
        let mut turn = 0usize;
        let mut index = first;
        while index < self.items.len() {
            match &self.items[index] {
                Item::User { text } => {
                    turn += 1;
                    out.push(Message::user(text.clone()));
                }
                Item::Assistant {
                    text, tool_calls, ..
                } => {
                    let end = self.items[index + 1..]
                        .iter()
                        .position(|i| !matches!(i, Item::ToolResult { .. }))
                        .map_or(self.items.len(), |p| index + 1 + p);
                    let results = &self.items[index + 1..end];
                    let answered: Vec<ToolCall> = tool_calls
                        .iter()
                        .filter(|c| {
                            results
                                .iter()
                                .any(|r| matches!(r, Item::ToolResult { id, .. } if *id == c.id))
                        })
                        .cloned()
                        .collect();
                    if !text.is_empty() || !answered.is_empty() {
                        out.push(Message::assistant(text.clone(), answered.clone()));
                    }
                    let old = turn <= full_from;
                    for call in &answered {
                        if let Some(Item::ToolResult { text, digest, .. }) = results
                            .iter()
                            .find(|r| matches!(r, Item::ToolResult { id, .. } if *id == call.id))
                        {
                            out.push(Message::tool(
                                call.id.clone(),
                                if old {
                                    shorten(text, digest)
                                } else {
                                    text.clone()
                                },
                            ));
                        }
                    }
                    index = end;
                    continue;
                }
                Item::ToolResult { .. } | Item::Summary { .. } => {}
            }
            index += 1;
        }
        out
    }

    /// A rough size of `messages`, in tokens. Real counts only arrive with a
    /// response, so the bytes are divided by three (a safe over-estimate for
    /// English and JSON) and the last real prompt size is used as a floor.
    pub fn estimate_tokens(&self, system_prompt: &str) -> u64 {
        let guess = |messages: &[Message]| -> u64 {
            messages
                .iter()
                .map(|m| {
                    (m.content.len()
                        + m.tool_calls
                            .iter()
                            .map(|c| c.name.len() + c.arguments.len())
                            .sum::<usize>()) as u64
                        / 3
                        + 4
                })
                .sum()
        };
        let all = guess(&self.messages(system_prompt));
        if self.last_prompt == 0 {
            return all;
        }
        let since: u64 = self.items[self.items_at_last_prompt.min(self.items.len())..]
            .iter()
            .map(|item| match item {
                Item::User { text } | Item::Summary { text } => text.len() as u64 / 3,
                Item::Assistant {
                    text, tool_calls, ..
                } => {
                    (text.len() + tool_calls.iter().map(|c| c.arguments.len()).sum::<usize>())
                        as u64
                        / 3
                }
                Item::ToolResult { text, .. } => text.len() as u64 / 3,
            })
            .sum();
        all.max(self.last_prompt + since)
    }

    /// The conversation as Markdown for export.
    pub fn to_markdown(&self) -> String {
        let mut out = format!("# {}\n\n", self.title.as_deref().unwrap_or("Conversation"));
        out.push_str(&format!("Model: `{}`\n\n", self.model));
        for item in &self.items {
            match item {
                Item::Summary { text } => {
                    out.push_str(&format!(
                        "> Earlier turns, summarised: {}\n\n",
                        text.replace('\n', "\n> ")
                    ));
                }
                Item::User { text } => out.push_str(&format!("## You\n\n{text}\n\n")),
                Item::Assistant {
                    text,
                    interrupted,
                    tool_calls,
                    ..
                } => {
                    out.push_str("## Assistant\n\n");
                    if !text.is_empty() {
                        out.push_str(text);
                        out.push_str("\n\n");
                    }
                    for call in tool_calls {
                        out.push_str(&format!("*Called `{}`*\n\n", call.name));
                    }
                    if *interrupted {
                        out.push_str("*(interrupted)*\n\n");
                    }
                }
                Item::ToolResult { name, text, .. } => {
                    out.push_str(&format!("Result of `{name}`:\n\n~~~\n{text}\n~~~\n\n"));
                }
            }
        }
        out
    }
}

/// What an old tool result is sent as: its digest, or its first line.
fn shorten(text: &str, digest: &str) -> String {
    if !digest.is_empty() {
        return digest.to_string();
    }
    let first = text.lines().next().unwrap_or("");
    let mut end = first.len().min(160);
    while !first.is_char_boundary(end) {
        end -= 1;
    }
    format!("{} [{} bytes, shortened]", &first[..end], text.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Role;

    fn meta() -> Event {
        Event::Meta {
            id: "c".into(),
            created: 1,
            model: "m".into(),
            persona: Persona::Chat,
            diagnostics: false,
            system: None,
        }
    }

    fn turn(user: &str, answer: &str) -> Vec<Event> {
        vec![
            Event::User { text: user.into() },
            Event::Assistant {
                text: answer.into(),
                model: "m".into(),
                interrupted: false,
            },
        ]
    }

    fn conversation(events: Vec<Event>) -> Conversation {
        Conversation::replay(std::iter::once(meta()).chain(events)).unwrap()
    }

    #[test]
    fn a_file_must_start_with_meta() {
        assert!(Conversation::replay([Event::User { text: "x".into() }]).is_none());
        assert!(Conversation::replay(std::iter::empty::<Event>()).is_none());
    }

    #[test]
    fn messages_follow_the_turns_and_leave_reasoning_out() {
        let mut events = vec![Event::User { text: "hi".into() }];
        events.push(Event::Reasoning {
            text: "thinking".into(),
        });
        events.push(Event::Assistant {
            text: "hello".into(),
            model: "m".into(),
            interrupted: false,
        });
        let c = conversation(events);
        let messages = c.messages("SYS");
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].role, Role::System);
        assert_eq!(messages[2].content, "hello");
        assert!(messages.iter().all(|m| !m.content.contains("thinking")));
        assert!(
            matches!(&c.items[1], Item::Assistant { reasoning, .. } if reasoning == "thinking")
        );
    }

    #[test]
    fn tool_calls_pair_with_their_results_and_dangling_ones_are_dropped() {
        let c = conversation(vec![
            Event::User { text: "q".into() },
            Event::Assistant {
                text: String::new(),
                model: "m".into(),
                interrupted: false,
            },
            Event::ToolCall {
                id: "a".into(),
                name: "n".into(),
                arguments: "{}".into(),
            },
            Event::ToolCall {
                id: "b".into(),
                name: "n".into(),
                arguments: "{}".into(),
            },
            Event::ToolResult {
                id: "a".into(),
                name: "n".into(),
                text: "ra".into(),
                digest: String::new(),
            },
        ]);
        let messages = c.messages("S");
        let assistant = &messages[2];
        assert_eq!(assistant.tool_calls.len(), 1);
        assert_eq!(assistant.tool_calls[0].id, "a");
        assert_eq!(messages[3].tool_call_id.as_deref(), Some("a"));
        assert_eq!(messages.len(), 4);
    }

    #[test]
    fn a_call_without_a_leading_assistant_event_still_replays() {
        let c = conversation(vec![
            Event::User { text: "q".into() },
            Event::ToolCall {
                id: "a".into(),
                name: "n".into(),
                arguments: "{}".into(),
            },
            Event::ToolResult {
                id: "a".into(),
                name: "n".into(),
                text: "r".into(),
                digest: String::new(),
            },
        ]);
        assert_eq!(c.messages("S").len(), 4);
    }

    #[test]
    fn old_tool_results_go_as_digests() {
        let mut events = Vec::new();
        for n in 0..4 {
            events.push(Event::User {
                text: format!("q{n}"),
            });
            events.push(Event::Assistant {
                text: String::new(),
                model: "m".into(),
                interrupted: false,
            });
            events.push(Event::ToolCall {
                id: format!("c{n}"),
                name: "n".into(),
                arguments: "{}".into(),
            });
            events.push(Event::ToolResult {
                id: format!("c{n}"),
                name: "n".into(),
                text: format!("BIG{n}\n{}", "x".repeat(500)),
                digest: if n == 0 {
                    "digest zero".into()
                } else {
                    String::new()
                },
            });
            events.push(Event::Assistant {
                text: format!("a{n}"),
                model: "m".into(),
                interrupted: false,
            });
        }
        let messages = conversation(events).messages("S");
        let tool_texts: Vec<&str> = messages
            .iter()
            .filter(|m| m.role == Role::Tool)
            .map(|m| m.content.as_str())
            .collect();
        assert_eq!(tool_texts[0], "digest zero");
        assert!(tool_texts[1].starts_with("BIG1 [") && tool_texts[1].ends_with("shortened]"));
        assert!(
            tool_texts[2].len() > 500 && tool_texts[3].len() > 500,
            "{tool_texts:?}"
        );
    }

    #[test]
    fn compaction_replaces_the_oldest_turns_with_a_summary() {
        let mut events = Vec::new();
        for n in 0..4 {
            events.extend(turn(&format!("q{n}"), &format!("a{n}")));
        }
        events.push(Event::Compact {
            summary: "they talked".into(),
            covered_turns: 2,
        });
        let c = conversation(events);
        assert_eq!(c.turns(), 2);
        assert!(matches!(&c.items[0], Item::Summary { text } if text == "they talked"));
        let messages = c.messages("SYS");
        assert!(messages[0].content.contains("they talked"));
        assert_eq!(messages[1].content, "q2");
        // A second compaction folds the first summary away.
        let mut c = c;
        c.apply(Event::Compact {
            summary: "later".into(),
            covered_turns: 1,
        });
        assert_eq!(c.turns(), 1);
        assert!(matches!(&c.items[0], Item::Summary { text } if text == "later"));
        c.apply(Event::Compact {
            summary: "none".into(),
            covered_turns: 0,
        });
        assert!(matches!(&c.items[0], Item::Summary { text } if text == "later"));
    }

    #[test]
    fn rewind_drops_turns_from_the_given_one() {
        let mut events = turn("q0", "a0");
        events.extend(turn("q1", "a1"));
        events.push(Event::Rewind { keep_turns: 1 });
        events.extend(turn("q1b", "a1b"));
        let c = conversation(events);
        assert_eq!(c.turns(), 2);
        assert_eq!(c.messages("S")[3].content, "q1b");
    }

    #[test]
    fn usage_sums_and_the_estimate_never_undercuts_the_last_prompt() {
        let mut events = turn("q", "a");
        events.push(Event::Usage {
            prompt: 1000,
            completion: 10,
            reasoning: 4,
        });
        events.push(Event::Usage {
            prompt: 1100,
            completion: 20,
            reasoning: 6,
        });
        events.push(Event::User {
            text: "x".repeat(300),
        });
        let c = conversation(events);
        assert_eq!(
            c.usage,
            Usage {
                prompt: 2100,
                completion: 30,
                reasoning: 10
            }
        );
        assert!(c.estimate_tokens("S") >= 1100 + 100);
    }

    #[test]
    fn markdown_export_shows_both_sides() {
        let mut c = conversation(turn("hello?", "hi!"));
        c.apply(Event::Title {
            title: "Greeting".into(),
        });
        let md = c.to_markdown();
        assert!(md.starts_with("# Greeting"));
        assert!(md.contains("## You\n\nhello?") && md.contains("## Assistant\n\nhi!"));
    }
}
