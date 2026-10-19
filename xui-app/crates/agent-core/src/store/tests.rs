use std::fs;
use std::sync::atomic::{AtomicU32, Ordering};

use super::*;

fn temp_dir(name: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "lazyagent-store-{}-{name}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn user(store: &Store, id: &str, text: &str) {
    store
        .append(id, &Event::User { text: text.into() })
        .unwrap();
}

#[test]
fn a_conversation_survives_a_reload() {
    let dir = temp_dir("reload");
    let store = Store::new(&dir);
    let c = store
        .create(100, "m", Persona::Diagnose, true, Some("be brief".into()))
        .unwrap();
    user(&store, &c.id, "why did netd restart?");
    store
        .append(
            &c.id,
            &Event::Assistant {
                text: "Because.".into(),
                model: "m".into(),
                interrupted: false,
            },
        )
        .unwrap();
    store
        .append(
            &c.id,
            &Event::Title {
                title: "netd".into(),
            },
        )
        .unwrap();
    let loaded = store.load(&c.id).unwrap();
    assert_eq!(loaded.title.as_deref(), Some("netd"));
    assert_eq!(loaded.persona, Persona::Diagnose);
    assert_eq!(loaded.system.as_deref(), Some("be brief"));
    assert_eq!(loaded.items.len(), 2);
    fs::remove_dir_all(dir).ok();
}

#[test]
fn ids_cannot_escape_the_folder() {
    let dir = temp_dir("ids");
    let store = Store::new(&dir);
    for bad in ["../x", "a/b", "a\\b", "", "C:x", "UP", &"x".repeat(65)] {
        assert!(store.load(bad).is_err(), "{bad}");
        assert!(
            store
                .append(bad, &Event::Title { title: "t".into() })
                .is_err(),
            "{bad}"
        );
        assert!(store.delete(bad).is_err(), "{bad}");
    }
    fs::remove_dir_all(dir).ok();
}

#[test]
fn a_torn_last_line_and_junk_lines_are_skipped() {
    let dir = temp_dir("torn");
    let store = Store::new(&dir);
    let c = store.create(1, "m", Persona::Chat, false, None).unwrap();
    user(&store, &c.id, "kept");
    let path = dir.join("conversations").join(format!("{}.jsonl", c.id));
    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(b"not json at all\n{\"t\":\"user\",\"text\":\"cut off")
        .unwrap();
    drop(file);
    let loaded = store.load(&c.id).unwrap();
    assert_eq!(loaded.items.len(), 1);
    // An oversized line is skipped without reading all of it, and the next
    // line still counts.
    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(b"\n").unwrap();
    file.write_all(&vec![b'x'; MAX_LINE + 100]).unwrap();
    file.write_all(b"\n{\"t\":\"user\",\"text\":\"after\"}\n")
        .unwrap();
    drop(file);
    let loaded = store.load(&c.id).unwrap();
    assert_eq!(loaded.items.len(), 2);
    fs::remove_dir_all(dir).ok();
}

#[test]
fn listing_orders_by_recent_use_and_titles_come_from_the_first_message() {
    let dir = temp_dir("list");
    let store = Store::new(&dir);
    let a = store.create(1, "m", Persona::Chat, false, None).unwrap();
    user(&store, &a.id, "first   question\nwith lines");
    let b = store.create(2, "m", Persona::Chat, false, None).unwrap();
    assert_eq!(store.list().len(), 2);
    assert_eq!(
        store.list().iter().find(|r| r.id == a.id).unwrap().title,
        "first question with lines"
    );
    assert_eq!(
        store.list().iter().find(|r| r.id == b.id).unwrap().title,
        "New chat"
    );
    store
        .append(
            &b.id,
            &Event::Title {
                title: "Named".into(),
            },
        )
        .unwrap();
    assert_eq!(
        store.list().iter().find(|r| r.id == b.id).unwrap().title,
        "Named"
    );
    // Files that are not conversations are ignored.
    fs::write(dir.join("conversations").join("notes.txt"), "x").unwrap();
    fs::write(dir.join("conversations").join("BAD ID.jsonl"), "x").unwrap();
    assert_eq!(store.list().len(), 2);
    fs::remove_dir_all(dir).ok();
}

#[test]
fn delete_moves_to_the_trash_and_never_overwrites_it() {
    let dir = temp_dir("trash");
    let store = Store::new(&dir);
    let c = store.create(1, "m", Persona::Chat, false, None).unwrap();
    store.delete(&c.id).unwrap();
    assert!(store.list().is_empty());
    assert!(dir.join("trash").join(format!("{}.jsonl", c.id)).exists());
    assert!(store.delete(&c.id).is_err(), "already gone");
    fs::write(
        dir.join("conversations").join(format!("{}.jsonl", c.id)),
        "{}\n",
    )
    .unwrap();
    store.delete(&c.id).unwrap();
    assert_eq!(fs::read_dir(dir.join("trash")).unwrap().count(), 2);
    store.delete_everything().unwrap();
    assert!(!dir.join("trash").exists());
    store.delete_everything().unwrap();
    fs::remove_dir_all(dir).ok();
}

#[test]
fn search_finds_titles_and_message_text_ignoring_case() {
    let dir = temp_dir("search");
    let store = Store::new(&dir);
    let a = store.create(1, "m", Persona::Chat, false, None).unwrap();
    user(&store, &a.id, "The DHCP lease keeps renewing every minute");
    let b = store.create(2, "m", Persona::Chat, false, None).unwrap();
    store
        .append(
            &b.id,
            &Event::Title {
                title: "Audio dropouts".into(),
            },
        )
        .unwrap();
    let hits = store.search("dhcp");
    assert_eq!(hits.len(), 1);
    assert!(hits[0].snippet.contains("DHCP lease"), "{:?}", hits[0]);
    assert_eq!(store.search("AUDIO").len(), 1);
    assert!(store.search("   ").is_empty());
    assert!(store.search("nothing like this").is_empty());
    fs::remove_dir_all(dir).ok();
}

#[test]
fn many_conversations_get_distinct_ids() {
    let dir = temp_dir("ids-many");
    let store = Store::new(&dir);
    let mut ids = std::collections::HashSet::new();
    for _ in 0..200 {
        assert!(ids.insert(store.create(5, "m", Persona::Chat, false, None).unwrap().id));
    }
    fs::remove_dir_all(dir).ok();
}

#[test]
fn a_long_conversation_replays_in_full() {
    let dir = temp_dir("soak");
    let store = Store::new(&dir);
    let c = store.create(1, "m", Persona::Chat, false, None).unwrap();
    for n in 0..3000 {
        user(&store, &c.id, &format!("question {n}"));
        store
            .append(
                &c.id,
                &Event::Assistant {
                    text: format!("answer {n}"),
                    model: "m".into(),
                    interrupted: false,
                },
            )
            .unwrap();
        store
            .append(
                &c.id,
                &Event::Usage {
                    prompt: 10,
                    completion: 5,
                    reasoning: 0,
                },
            )
            .unwrap();
    }
    let loaded = store.load(&c.id).unwrap();
    assert_eq!(loaded.turns(), 3000);
    assert_eq!(loaded.usage.prompt, 30_000);
    assert_eq!(loaded.messages("S").len(), 6001);
    fs::remove_dir_all(dir).ok();
}
