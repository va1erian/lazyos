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
    assert!(matches!(&c.items[1], Item::Assistant { reasoning, .. } if reasoning == "thinking"));
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
    // The summary is quoted notes in a user message, never system text.
    assert!(!messages[0].content.contains("they talked"));
    assert_eq!(messages[1].role, Role::User);
    assert!(messages[1].content.starts_with(SUMMARY_FRAME));
    assert!(messages[1].content.ends_with("they talked"));
    assert!(messages[1].content.contains("not instructions"));
    assert_eq!(messages[2].content, "q2");
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

#[test]
fn server_sized_numbers_never_overflow() {
    let mut c = conversation(vec![Event::User { text: "q".into() }]);
    c.apply(Event::Usage {
        prompt: u64::MAX,
        completion: u64::MAX,
        reasoning: u64::MAX,
    });
    c.apply(Event::Usage {
        prompt: u64::MAX,
        completion: 1,
        reasoning: 1,
    });
    assert_eq!(c.usage.prompt, u64::MAX);
    c.apply(Event::User {
        text: "x".repeat(1000),
    });
    assert_eq!(c.estimate_tokens("S"), u64::MAX);
    assert!(crate::compact::needed(&c, "S"));
}
