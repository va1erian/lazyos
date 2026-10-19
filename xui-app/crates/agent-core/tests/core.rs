//! The core a window drives, against a scripted TLS server: keys, turns,
//! Stop, conversations and settings (docs/ai-agent-plan.md P2).

mod common;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use lazyagent_core::{
    Core, Endpoint, Event, Events, Platform, Request, Roots, Secret, Settings, ThemeChoice,
};

struct Test {
    dir: PathBuf,
    roots: Roots,
    endpoint: Endpoint,
}

impl Platform for Test {
    fn ca_roots(&self) -> Roots {
        self.roots.clone()
    }
    fn app_dir(&self) -> PathBuf {
        self.dir.clone()
    }
    fn endpoint(&self) -> Endpoint {
        self.endpoint.clone()
    }
    fn log(&self, _line: &str) {}
}

fn temp(name: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "lazyagent-core-{}-{name}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn start(pki: &Pki, server: &Server, dir: &std::path::Path) -> (Core, Events) {
    Core::start(Arc::new(Test {
        dir: dir.to_path_buf(),
        roots: pki.roots(),
        endpoint: server.endpoint(),
    }))
}

/// Every event up to and including the next `Busy(false)`.
fn turn(events: &Events) -> Vec<Event> {
    let mut seen = Vec::new();
    let end = Instant::now() + Duration::from_secs(20);
    while Instant::now() < end {
        if let Some(event) = events.wait(Duration::from_millis(200)) {
            let done = matches!(event, Event::Busy(false));
            seen.push(event);
            if done {
                return seen;
            }
        }
    }
    panic!("no end of turn; saw {seen:?}");
}

fn text_of(events: &[Event]) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn the_first_events_are_the_settings_and_an_empty_list() {
    let pki = Pki::new();
    let server = serve(&pki, |_, _| answer_of("x"));
    let dir = temp("first");
    let (_core, events) = start(&pki, &server, &dir);
    let Some(Event::Settings(s)) = events.wait(Duration::from_secs(5)) else {
        panic!()
    };
    assert!(!s.consented);
    assert!(
        matches!(events.wait(Duration::from_secs(5)), Some(Event::Conversations(list)) if list.is_empty())
    );
}

#[test]
fn without_a_key_a_send_asks_for_it_and_stores_nothing() {
    let pki = Pki::new();
    let server = serve(&pki, |_, _| answer_of("x"));
    let dir = temp("nokey");
    let (core, events) = start(&pki, &server, &dir);
    core.send(Request::Send("hello".into()));
    loop {
        match events.wait(Duration::from_secs(5)).expect("an answer") {
            Event::Failed(f) => {
                assert!(f.key_needed);
                assert_eq!(f.status, "key");
                break;
            }
            _ => continue,
        }
    }
    assert_eq!(server.requests(), 0);
    assert!(!dir.join("conversations").exists());
}

#[test]
fn a_turn_streams_stores_titles_and_never_shows_the_key() {
    let pki = Pki::new();
    let server = serve(&pki, |n, _| {
        if n == 0 {
            answer_of("Hello there")
        } else {
            answer_of("\"A greeting\"")
        }
    });
    let dir = temp("turn");
    let (core, events) = start(&pki, &server, &dir);
    core.send(Request::SetKey(Secret::new(KEY)));
    core.send(Request::Send("hi".into()));
    let seen = turn(&events);
    assert_eq!(text_of(&seen), "Hello there");
    let opened = seen
        .iter()
        .find_map(|e| match e {
            Event::Opened(c) => Some(c.clone()),
            _ => None,
        })
        .expect("the conversation");
    assert_eq!(opened.items.len(), 2);
    assert!(seen
        .iter()
        .any(|e| matches!(e, Event::Titled { title, .. } if title == "A greeting")));
    let stats = seen.iter().find_map(|e| match e {
        Event::Done(s) => Some(s.clone()),
        _ => None,
    });
    assert!(stats.is_some_and(|s| s.usage.is_some() && s.finish.as_deref() == Some("stop")));
    let Some(Event::Conversations(list)) = seen
        .iter()
        .rev()
        .find(|e| matches!(e, Event::Conversations(_)))
    else {
        panic!()
    };
    assert_eq!(list[0].title, "A greeting");
    let first = server.seen.lock().unwrap()[0].clone();
    assert_eq!(
        first.header("authorization"),
        Some(format!("Bearer {KEY}").as_str())
    );
    let everything = format!("{seen:?}");
    assert!(!everything.contains(KEY), "the key leaked into an event");
    for entry in std::fs::read_dir(dir.join("conversations"))
        .unwrap()
        .flatten()
    {
        assert!(!std::fs::read_to_string(entry.path()).unwrap().contains(KEY));
    }
    assert!(!std::fs::read_to_string(dir.join("settings.toml"))
        .unwrap_or_default()
        .contains(KEY));
}

#[test]
fn a_refused_key_is_forgotten_and_asked_for_again() {
    let pki = Pki::new();
    let server = serve(&pki, |_, _| {
        status(
            401,
            &[],
            r#"{"error":{"message":"The API key you provided is invalid."}}"#,
        )
    });
    let dir = temp("401");
    let (core, events) = start(&pki, &server, &dir);
    core.send(Request::SetKey(Secret::new("wrong")));
    core.send(Request::Send("hi".into()));
    let seen = turn(&events);
    assert!(seen
        .iter()
        .any(|e| matches!(e, Event::Failed(f) if f.key_needed && f.status == "401")));
    core.send(Request::Send("again".into()));
    let again = events.wait(Duration::from_secs(5));
    let mut found = false;
    let mut next = again;
    while let Some(e) = next {
        if matches!(&e, Event::Failed(f) if f.status == "key") {
            found = true;
            break;
        }
        next = events.wait(Duration::from_secs(1));
    }
    assert!(found, "the refused key must not be tried twice");
    assert_eq!(server.requests(), 1);
}

#[test]
fn stop_ends_a_stalled_reply_and_keeps_the_partial() {
    let pki = Pki::new();
    let server = serve(&pki, |_, _| {
        vec![
            sse_head(),
            event(&text_delta("Half an")),
            Step::Sleep(Duration::from_secs(30)),
        ]
    });
    let dir = temp("stop");
    let (core, events) = start(&pki, &server, &dir);
    core.send(Request::SetKey(Secret::new(KEY)));
    core.send(Request::Send("hi".into()));
    // Wait for the first text, then stop.
    let mut seen = Vec::new();
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end {
        if let Some(e) = events.wait(Duration::from_millis(100)) {
            let text = matches!(e, Event::Text(_));
            seen.push(e);
            if text {
                break;
            }
        }
    }
    let stopped_at = Instant::now();
    core.stop();
    seen.extend(turn(&events));
    assert!(
        stopped_at.elapsed() < Duration::from_millis(1500),
        "{:?}",
        stopped_at.elapsed()
    );
    assert!(seen
        .iter()
        .any(|e| matches!(e, Event::Failed(f) if f.status == "stopped" && f.kept_partial)));
    let opened = seen.iter().find_map(|e| match e {
        Event::Opened(c) => Some(c.clone()),
        _ => None,
    });
    let items = opened.unwrap().items.clone();
    assert!(
        matches!(&items[1], lazyagent_core::store::Item::Assistant { text, interrupted: true, .. } if text == "Half an")
    );
}

#[test]
fn conversations_open_rename_search_export_and_delete() {
    let pki = Pki::new();
    let server = serve(&pki, |n, _| {
        if n % 2 == 0 {
            answer_of("The DHCP lease renews often.")
        } else {
            answer_of("Renewals")
        }
    });
    let dir = temp("manage");
    let (core, events) = start(&pki, &server, &dir);
    core.send(Request::SetKey(Secret::new(KEY)));
    core.send(Request::Send("why does DHCP renew?".into()));
    let seen = turn(&events);
    let id = seen
        .iter()
        .find_map(|e| match e {
            Event::Opened(c) => Some(c.id.clone()),
            _ => None,
        })
        .unwrap();
    core.send(Request::New);
    assert!(matches!(
        next_matching(&events, |e| matches!(e, Event::Cleared)),
        Event::Cleared
    ));
    core.send(Request::Open(id.clone()));
    let Event::Opened(c) = next_matching(&events, |e| matches!(e, Event::Opened(_))) else {
        panic!()
    };
    assert_eq!(c.items.len(), 2);
    core.send(Request::Rename {
        id: id.clone(),
        title: "  DHCP notes  ".into(),
    });
    let Event::Titled { title, .. } = next_matching(&events, |e| matches!(e, Event::Titled { .. }))
    else {
        panic!()
    };
    assert_eq!(title, "DHCP notes");
    core.send(Request::Search("lease".into()));
    let Event::Hits(hits) = next_matching(&events, |e| matches!(e, Event::Hits(_))) else {
        panic!()
    };
    assert_eq!(hits.len(), 1);
    core.send(Request::Export(id.clone()));
    let Event::Exported { markdown, .. } =
        next_matching(&events, |e| matches!(e, Event::Exported { .. }))
    else {
        panic!()
    };
    assert!(markdown.starts_with("# DHCP notes") && markdown.contains("## You"));
    core.send(Request::Delete(id));
    assert!(matches!(
        next_matching(&events, |e| matches!(e, Event::Cleared)),
        Event::Cleared
    ));
    let Event::Conversations(list) =
        next_matching(&events, |e| matches!(e, Event::Conversations(_)))
    else {
        panic!()
    };
    assert!(list.is_empty());
}

fn next_matching(events: &Events, wanted: impl Fn(&Event) -> bool) -> Event {
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end {
        if let Some(e) = events.wait(Duration::from_millis(200)) {
            if wanted(&e) {
                return e;
            }
        }
    }
    panic!("the wanted event never came");
}

#[test]
fn settings_are_saved_clamped_and_used() {
    let pki = Pki::new();
    let server = serve(&pki, |_, _| answer_of("ok"));
    let dir = temp("settings");
    let (core, events) = start(&pki, &server, &dir);
    let settings = Settings {
        model: "accounts/fireworks/models/glm-5p3-flash".into(),
        effort: "low".into(),
        temperature: Some(7.0),
        theme: ThemeChoice::Dark,
        consented: true,
        ..Settings::default()
    };
    core.send(Request::SaveSettings(settings));
    let Event::Settings(back) =
        next_matching(&events, |e| matches!(e, Event::Settings(s) if s.consented))
    else {
        panic!()
    };
    assert_eq!(back.temperature, Some(2.0));
    assert_eq!(Settings::load(&dir), back);
    core.send(Request::SetKey(Secret::new(KEY)));
    core.send(Request::Send("hi".into()));
    turn(&events);
    let body = server.seen.lock().unwrap()[0].json();
    assert_eq!(body["model"], "accounts/fireworks/models/glm-5p3-flash");
    assert_eq!(body["reasoning_effort"], "low");
    assert_eq!(body["temperature"], 2.0);
    // A new run reads them back.
    drop(core);
    let (_core2, events2) = start(&pki, &server, &dir);
    let Some(Event::Settings(s)) = events2.wait(Duration::from_secs(5)) else {
        panic!()
    };
    assert!(s.consented && s.theme == ThemeChoice::Dark);
}

#[test]
fn regenerate_and_edit_replay_the_turn() {
    let pki = Pki::new();
    let server = serve(&pki, |n, _| answer_of(&format!("reply {n}")));
    let dir = temp("regen");
    let (core, events) = start(&pki, &server, &dir);
    core.send(Request::SetKey(Secret::new(KEY)));
    core.send(Request::Send("question".into()));
    turn(&events);
    core.send(Request::Regenerate);
    let seen = turn(&events);
    let c = seen
        .iter()
        .find_map(|e| match e {
            Event::Opened(c) => Some(c.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(c.turns(), 1);
    core.send(Request::EditLast("question, better".into()));
    let seen = turn(&events);
    let c = seen
        .iter()
        .find_map(|e| match e {
            Event::Opened(c) => Some(c.clone()),
            _ => None,
        })
        .unwrap();
    assert!(
        matches!(&c.items[0], lazyagent_core::store::Item::User { text } if text == "question, better")
    );
}
