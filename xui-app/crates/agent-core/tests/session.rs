//! A conversation end to end with a scripted provider: what is stored, what
//! is sent, interruptions, regenerate and edit, compaction and the title.

use std::collections::VecDeque;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use lazyagent_core::message::Role;
use lazyagent_core::provider::Discard;
use lazyagent_core::store::Item;
use lazyagent_core::{
    ChatError, ChatRequest, Completion, Persona, Provider, Session, Sink, Store, TurnError, Usage,
};

struct Fake {
    replies: Mutex<VecDeque<Result<Completion, ChatError>>>,
    sent: Mutex<Vec<ChatRequest>>,
}

impl Fake {
    fn new(replies: Vec<Result<Completion, ChatError>>) -> Fake {
        Fake {
            replies: Mutex::new(replies.into()),
            sent: Mutex::default(),
        }
    }
}

impl Provider for Fake {
    fn stream(
        &self,
        request: &ChatRequest,
        _stop: &Arc<AtomicBool>,
        sink: &mut dyn Sink,
    ) -> Result<Completion, ChatError> {
        self.sent.lock().unwrap().push(request.clone());
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("a scripted reply");
        if let Ok(done) = &reply {
            sink.text(&done.content);
        }
        reply
    }
}

fn says(text: &str) -> Result<Completion, ChatError> {
    Ok(Completion {
        content: text.into(),
        finish_reason: Some("stop".into()),
        usage: Some(Usage {
            prompt: 20,
            completion: 5,
            reasoning: 2,
        }),
        ..Completion::default()
    })
}

fn stop() -> Arc<AtomicBool> {
    Arc::default()
}

fn temp_store(name: &str) -> (Store, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("lazyagent-session-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    (Store::new(&dir), dir)
}

fn session<'a>(store: &'a Store, fake: &'a Fake, model: &str) -> Session<'a> {
    let conversation = store.create(1, model, Persona::Chat, false, None).unwrap();
    Session::new(conversation, store, fake, "SYSTEM PROMPT")
}

#[test]
fn a_turn_is_stored_and_reloads_the_same() {
    let (store, dir) = temp_store("turn");
    let fake = Fake::new(vec![Ok(Completion {
        reasoning: "thinking".into(),
        ..says("Hello!").unwrap()
    })]);
    let mut s = session(
        &store,
        &fake,
        "accounts/fireworks/models/deepseek-v4p1-flash",
    );
    let done = s.send("hi", &stop(), &mut Discard).unwrap();
    assert_eq!(done.content, "Hello!");
    let sent = fake.sent.lock().unwrap()[0].clone();
    assert_eq!(sent.messages[0].role, Role::System);
    assert!(sent.messages[0].content.starts_with("SYSTEM PROMPT"));
    assert_eq!(sent.messages[1].content, "hi");
    assert_eq!(
        s.conversation.usage,
        Usage {
            prompt: 20,
            completion: 5,
            reasoning: 2
        }
    );
    let reloaded = store.load(&s.conversation.id).unwrap();
    assert_eq!(reloaded, s.conversation);
    assert!(
        matches!(&reloaded.items[1], Item::Assistant { reasoning, .. } if reasoning == "thinking")
    );
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn an_interrupted_answer_is_kept_and_regenerate_replaces_the_turn() {
    let (store, dir) = temp_store("interrupted");
    let fake = Fake::new(vec![
        Err(ChatError::Interrupted {
            partial: Box::new(Completion {
                content: "Half".into(),
                ..Completion::default()
            }),
            why: "no data".into(),
        }),
        says("The whole answer"),
    ]);
    let mut s = session(&store, &fake, "m");
    let err = s.send("question", &stop(), &mut Discard).unwrap_err();
    assert!(matches!(
        err,
        TurnError::Chat(ChatError::Interrupted { .. })
    ));
    assert!(matches!(
        &s.conversation.items[1],
        Item::Assistant { text, interrupted: true, .. } if text == "Half"
    ));
    s.regenerate(&stop(), &mut Discard).unwrap();
    assert_eq!(s.conversation.turns(), 1);
    assert_eq!(s.conversation.items.len(), 2);
    assert!(
        matches!(&s.conversation.items[1], Item::Assistant { text, interrupted: false, .. } if text == "The whole answer")
    );
    // The retry asked for the same question without the broken answer.
    let second = fake.sent.lock().unwrap()[1].clone();
    assert_eq!(second.messages.len(), 2);
    assert_eq!(store.load(&s.conversation.id).unwrap(), s.conversation);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn a_failure_with_no_answer_keeps_only_the_question_and_can_be_resumed() {
    let (store, dir) = temp_store("401");
    let fake = Fake::new(vec![
        Err(ChatError::Unauthorized("bad key".into())),
        says("now it works"),
    ]);
    let mut s = session(&store, &fake, "m");
    assert!(s.send("hello", &stop(), &mut Discard).is_err());
    assert_eq!(s.conversation.items.len(), 1);
    s.respond(&stop(), &mut Discard).unwrap();
    assert_eq!(s.conversation.items.len(), 2);
    assert_eq!(fake.sent.lock().unwrap()[1].messages.len(), 2);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn a_stop_before_any_text_stores_no_empty_answer() {
    let (store, dir) = temp_store("stop");
    let fake = Fake::new(vec![Err(ChatError::Stopped {
        partial: Box::default(),
    })]);
    let mut s = session(&store, &fake, "m");
    assert!(s.send("hello", &stop(), &mut Discard).is_err());
    assert_eq!(s.conversation.items.len(), 1);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn edit_rewrites_the_last_message_and_keeps_earlier_turns() {
    let (store, dir) = temp_store("edit");
    let fake = Fake::new(vec![says("a1"), says("a2"), says("a2 again")]);
    let mut s = session(&store, &fake, "m");
    s.send("q1", &stop(), &mut Discard).unwrap();
    s.send("q2 with a typo", &stop(), &mut Discard).unwrap();
    s.edit_last("q2 fixed", &stop(), &mut Discard).unwrap();
    assert_eq!(s.conversation.turns(), 2);
    let last = fake.sent.lock().unwrap()[2].clone();
    let texts: Vec<&str> = last.messages.iter().map(|m| m.content.as_str()).collect();
    assert_eq!(&texts[1..], ["q1", "a1", "q2 fixed"]);
    assert!(!texts.iter().any(|t| t.contains("typo")));
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn a_conversation_too_big_for_the_model_is_compacted_first() {
    let (store, dir) = temp_store("compact");
    // An unknown model has a 32k-token context: 80 kB of text is over 75% of it.
    let fake = Fake::new(vec![
        says("a1"),
        says("a2"),
        says("a3"),
        says("They discussed the logs."),
        says("a4"),
    ]);
    let mut s = session(&store, &fake, "someone/else/model");
    let padding = "padding ".repeat(5_000);
    s.send("first question", &stop(), &mut Discard).unwrap();
    s.send("second question", &stop(), &mut Discard).unwrap();
    s.send(&padding, &stop(), &mut Discard).unwrap();
    assert_eq!(fake.sent.lock().unwrap().len(), 3, "nothing to compact yet");
    let mut notes = Notes::default();
    s.send(&padding, &stop(), &mut notes).unwrap();
    assert_eq!(notes.compacted, Some(2), "the oldest two turns were folded");
    let sent = fake.sent.lock().unwrap();
    // The summarizer ran before the real request, with no tools.
    assert_eq!(sent.len(), 5);
    let summarizer = &sent[3];
    assert!(summarizer.tools.is_empty());
    assert!(summarizer.messages[0].content.contains("condense"));
    assert!(summarizer.messages[1].content.contains("first question"));
    assert!(summarizer.messages[1].content.contains("second question"));
    assert!(
        !summarizer.messages[1].content.contains("padding"),
        "only the old turns"
    );
    let main = &sent[4];
    assert!(main.messages[0]
        .content
        .contains("They discussed the logs."));
    let texts: Vec<&str> = main.messages.iter().map(|m| m.content.as_str()).collect();
    assert!(!texts.iter().any(|t| t.contains("first question")));
    assert_eq!(s.conversation.turns(), 2);
    assert_eq!(store.load(&s.conversation.id).unwrap(), s.conversation);
    std::fs::remove_dir_all(dir).ok();
}

#[derive(Default)]
struct Notes {
    compacted: Option<usize>,
}

impl Sink for Notes {
    fn compacted(&mut self, turns: usize) {
        self.compacted = Some(turns);
    }
}

#[test]
fn a_huge_single_turn_cannot_be_compacted_and_says_so() {
    let (store, dir) = temp_store("toobig");
    let fake = Fake::new(vec![]);
    let mut s = session(&store, &fake, "someone/else/model");
    let err = s
        .send(&"x".repeat(200_000), &stop(), &mut Discard)
        .unwrap_err();
    assert!(
        matches!(&err, TurnError::Chat(ChatError::Refused(m)) if m.contains("too few turns")),
        "{err}"
    );
    assert!(fake.sent.lock().unwrap().is_empty());
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn the_model_names_the_conversation_once() {
    let (store, dir) = temp_store("title");
    let fake = Fake::new(vec![says("an answer"), says("\"Why netd restarts.\"\n")]);
    let mut s = session(&store, &fake, "m");
    s.send("why does netd restart?", &stop(), &mut Discard)
        .unwrap();
    assert_eq!(s.name(&stop()).as_deref(), Some("Why netd restarts"));
    assert_eq!(s.name(&stop()), None, "named once");
    assert_eq!(store.list()[0].title, "Why netd restarts");
    let titling = fake.sent.lock().unwrap()[1].clone();
    assert!(titling.tools.is_empty());
    assert!(titling.messages[1]
        .content
        .contains("why does netd restart?"));
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn a_failing_title_request_is_not_an_error() {
    let (store, dir) = temp_store("title-fail");
    let fake = Fake::new(vec![
        says("an answer"),
        Err(ChatError::Transport("offline".into())),
    ]);
    let mut s = session(&store, &fake, "m");
    s.send("hello", &stop(), &mut Discard).unwrap();
    assert_eq!(s.name(&stop()), None);
    assert!(s.conversation.title.is_none());
    std::fs::remove_dir_all(dir).ok();
}
