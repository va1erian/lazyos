//! The tabs model (docs/lazyweb-plan.md 2.6) with no window: order, the front tab, closing and
//! reopening, cycling, jumping, discarding and the session file. A recording engine per tab stands
//! for the views the window holds: a live tab has one, a discarded tab has none.

use std::collections::BTreeMap;
use std::fs;
use std::rc::Rc;

use xui_lazyweb::session;
use xui_lazyweb::tabs::{Place, TabId, Tabs, MAX_TABS};
use xui_webkit::engine::{Call, Engine, Recorder};

fn page(n: u32) -> String {
    format!("https://site{n}.example/")
}

/// Tabs 0..count at `page(n)`, the first in front.
fn tabs_of(count: u32) -> (Tabs, Vec<TabId>) {
    let mut tabs = Tabs::with_first(&page(0));
    for n in 1..count {
        tabs.open(&page(n), Place::End).unwrap();
    }
    let ids = tabs.ids();
    (tabs, ids)
}

fn urls(tabs: &Tabs) -> Vec<String> {
    tabs.iter().map(|t| t.url.clone()).collect()
}

#[test]
fn a_new_window_has_one_tab_in_front() {
    let tabs = Tabs::with_first("https://a.example/");
    assert_eq!(tabs.len(), 1);
    assert_eq!(tabs.active_id(), tabs.ids().first().copied());
    assert!(tabs.active().unwrap().is_live());
}

#[test]
fn a_tab_opens_at_the_end_or_beside_the_front_one_in_order() {
    let (mut tabs, ids) = tabs_of(3);
    tabs.raise(ids[0]);
    // Links opened in a row from tab 0 keep their order after it.
    let a = tabs
        .open("https://a.example/", Place::BesideActive)
        .unwrap();
    let b = tabs
        .open("https://b.example/", Place::BesideActive)
        .unwrap();
    assert_eq!(tabs.ids(), [ids[0], a, b, ids[1], ids[2]]);
    // Ctrl+T goes last and opens in the background until the window raises it.
    let end = tabs.open("https://end.example/", Place::End).unwrap();
    assert_eq!(tabs.index_of(end), Some(5));
    assert_eq!(tabs.active_id(), Some(ids[0]));
    // Coming to the front of another tab starts a new run of links.
    tabs.raise(ids[2]);
    let c = tabs
        .open("https://c.example/", Place::BesideActive)
        .unwrap();
    assert_eq!(tabs.index_of(c), Some(tabs.index_of(ids[2]).unwrap() + 1));
}

#[test]
fn the_window_holds_a_bounded_number_of_tabs() {
    let (mut tabs, _) = tabs_of(MAX_TABS as u32);
    assert_eq!(tabs.len(), MAX_TABS);
    assert!(tabs.open("https://more.example/", Place::End).is_none());
}

#[test]
fn closing_the_front_tab_raises_its_right_neighbour_else_the_left() {
    let (mut tabs, ids) = tabs_of(4);
    tabs.raise(ids[1]);
    let closure = tabs.close(ids[1]).unwrap();
    assert_eq!(closure.front.map(|r| r.id), Some(ids[2]));
    assert!(!closure.last);
    assert_eq!(tabs.active_id(), Some(ids[2]));
    // The last tab of the strip: the one before it.
    tabs.raise(ids[3]);
    assert_eq!(
        tabs.close(ids[3]).unwrap().front.map(|r| r.id),
        Some(ids[2])
    );
    // A tab behind the front one: the front one stays.
    let closure = tabs.close(ids[0]).unwrap();
    assert!(closure.front.is_none());
    assert_eq!(tabs.active_id(), Some(ids[2]));
    assert!(tabs.close(ids[0]).is_none(), "an id is closed once");
}

#[test]
fn closing_the_last_tab_says_so() {
    let (mut tabs, ids) = tabs_of(1);
    let closure = tabs.close(ids[0]).unwrap();
    assert!(closure.last);
    assert!(tabs.is_empty());
    assert_eq!(tabs.active_id(), None);
}

#[test]
fn a_closed_tab_comes_back_where_it_was_and_needs_a_view() {
    let (mut tabs, ids) = tabs_of(4);
    tabs.note(ids[1], "https://site1.example/page", "Page one");
    tabs.close(ids[1]).unwrap();
    assert_eq!(tabs.closed_count(), 1);
    let raised = tabs.reopen().unwrap();
    assert!(raised.needs_view, "the closed tab's view is gone");
    assert_eq!(tabs.index_of(raised.id), Some(1));
    assert_eq!(tabs.active_id(), Some(raised.id));
    let tab = tabs.get(raised.id).unwrap();
    assert_eq!(tab.url, "https://site1.example/page");
    assert_eq!(tab.title, "Page one");
    assert_ne!(raised.id, ids[1], "an id is never used twice");
    assert!(tabs.reopen().is_none());
}

#[test]
fn the_closed_stack_holds_ten_newest_first_and_only_web_pages() {
    let (mut tabs, ids) = tabs_of(13);
    for id in &ids[..12] {
        tabs.close(*id).unwrap();
    }
    assert_eq!(tabs.closed_count(), 10);
    // The newest comes back first: tab 11 was closed last.
    let first = tabs.reopen().unwrap().id;
    assert_eq!(tabs.get(first).unwrap().url, page(11));
    // The browser's own pages are not worth bringing back.
    let own = tabs.open("data:text/html,start", Place::End).unwrap();
    let before = tabs.closed_count();
    tabs.close(own).unwrap();
    assert_eq!(tabs.closed_count(), before);
}

#[test]
fn ctrl_tab_cycles_round_and_ctrl_digits_jump() {
    let (mut tabs, ids) = tabs_of(3);
    assert_eq!(tabs.cycle(true).unwrap().id, ids[1]);
    assert_eq!(tabs.cycle(true).unwrap().id, ids[2]);
    assert_eq!(
        tabs.cycle(true).unwrap().id,
        ids[0],
        "past the last: the first"
    );
    assert_eq!(
        tabs.cycle(false).unwrap().id,
        ids[2],
        "before the first: the last"
    );
    assert_eq!(tabs.jump(1).unwrap().id, ids[0]);
    assert_eq!(tabs.jump(2).unwrap().id, ids[1]);
    assert!(tabs.jump(5).is_none(), "no fifth tab: nothing happens");
    assert_eq!(tabs.active_id(), Some(ids[1]));
    assert_eq!(tabs.jump(9).unwrap().id, ids[2], "Ctrl+9 is the last tab");
    assert!(tabs.jump(0).is_none());
    let (mut one, _) = tabs_of(1);
    assert!(one.cycle(true).is_none(), "one tab has nowhere to cycle to");
}

#[test]
fn ctrl_nine_is_the_last_tab_however_many() {
    let (mut tabs, ids) = tabs_of(12);
    assert_eq!(tabs.jump(9).unwrap().id, ids[11]);
    assert_eq!(tabs.jump(8).unwrap().id, ids[7]);
}

#[test]
fn a_discarded_tab_keeps_its_address_and_title_and_needs_a_view_when_raised() {
    let (mut tabs, ids) = tabs_of(3);
    tabs.note(ids[1], "https://site1.example/deep", "Deep");
    assert!(tabs.discard(ids[1]));
    let tab = tabs.get(ids[1]).unwrap();
    assert!(tab.discarded && !tab.is_live());
    assert_eq!(
        (tab.url.as_str(), tab.title.as_str()),
        ("https://site1.example/deep", "Deep")
    );
    // The news of a view that is gone must not blank what was kept.
    tabs.note(ids[1], "", "");
    assert_eq!(tabs.get(ids[1]).unwrap().url, "https://site1.example/deep");
    let raised = tabs.raise(ids[1]).unwrap();
    assert!(raised.needs_view);
    assert_eq!(raised.previous, Some(ids[0]));
    assert!(tabs.get(ids[1]).unwrap().is_live());
    assert!(!tabs.raise(ids[1]).unwrap().needs_view);
}

#[test]
fn the_tab_in_front_a_dialog_or_a_download_is_never_discarded() {
    let (mut tabs, ids) = tabs_of(3);
    assert!(!tabs.discard(ids[0]), "the front tab stays");
    tabs.set_busy(ids[1], true);
    assert!(
        !tabs.discard(ids[1]),
        "a dialog or a download keeps its tab"
    );
    assert!(tabs.discard(ids[2]));
    assert!(!tabs.discard(ids[2]), "already discarded");
    tabs.set_busy(ids[1], false);
    assert!(tabs.discard(ids[1]));
    assert!(!tabs.discard(9999), "no such tab");
}

#[test]
fn the_oldest_used_tab_is_discarded_first() {
    let (mut tabs, ids) = tabs_of(5);
    // Use order, oldest first: 3, 1, 2, 4, then 0 in front.
    for n in [3, 1, 2, 4, 0] {
        tabs.raise(ids[n]);
    }
    assert_eq!(tabs.discard_choice(), Some(ids[3]));
    // A tab that cannot be discarded is passed over.
    tabs.set_busy(ids[3], true);
    assert_eq!(tabs.discard_choice(), Some(ids[1]));
    assert_eq!(tabs.excess(3), [ids[1], ids[2]]);
    assert_eq!(tabs.excess(5), Vec::<TabId>::new());
    // Never more than can go: the busy one and the front one stay.
    assert_eq!(tabs.excess(0), [ids[1], ids[2], ids[4]]);
}

#[test]
fn a_crashed_tab_holds_no_view_and_is_not_a_candidate() {
    let (mut tabs, ids) = tabs_of(3);
    tabs.set_crashed(ids[1], true);
    assert_eq!(tabs.live_count(), 2);
    assert!(!tabs.discard(ids[1]));
    assert_eq!(tabs.discard_choice(), Some(ids[2]));
    let raised = tabs.raise(ids[1]).unwrap();
    assert!(
        raised.needs_view && raised.was_crashed,
        "Reload makes a new view"
    );
    assert!(!tabs.get(ids[1]).unwrap().crashed);
}

#[test]
fn a_view_per_live_tab_follows_the_model() {
    // The window's loop in miniature: an engine for each tab that holds a view, none otherwise.
    let (mut tabs, ids) = tabs_of(3);
    let mut engines: BTreeMap<TabId, Rc<Recorder>> = BTreeMap::new();
    for id in &ids {
        engines.insert(*id, Rc::default());
    }
    for id in tabs.excess(1) {
        assert!(tabs.discard(id));
        engines.remove(&id);
    }
    assert_eq!(engines.len(), 1);
    assert_eq!(tabs.live_count(), engines.len());
    let raised = tabs.raise(ids[2]).unwrap();
    assert!(raised.needs_view);
    let engine: Rc<Recorder> = Rc::default();
    engine.load(&tabs.get(ids[2]).unwrap().url);
    engines.insert(ids[2], Rc::clone(&engine));
    assert_eq!(engine.calls(), [Call::Load(page(2))]);
    assert_eq!(tabs.live_count(), engines.len());
}

#[test]
fn dragging_a_tab_moves_it() {
    let (mut tabs, ids) = tabs_of(4);
    assert!(tabs.move_to(ids[0], 2));
    assert_eq!(tabs.ids(), [ids[1], ids[2], ids[0], ids[3]]);
    assert!(tabs.move_to(ids[3], 0));
    assert_eq!(tabs.id_at(0), Some(ids[3]));
    assert!(tabs.move_to(ids[3], 99), "past the end is the end");
    assert_eq!(tabs.id_at(3), Some(ids[3]));
    assert!(!tabs.move_to(777, 0));
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("lazyweb-tabs-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn the_tabs_survive_the_session_file_discarded_until_focused() {
    let (mut tabs, ids) = tabs_of(4);
    tabs.note(ids[2], "https://site2.example/x", "Two \"quoted\"\n");
    tabs.raise(ids[2]);
    tabs.close(ids[0]).unwrap();
    let dir = scratch("roundtrip");
    session::save(&dir, &tabs.to_session()).unwrap();

    let saved = session::load(&dir).unwrap();
    assert_eq!(saved.windows.len(), 1);
    let restored = Tabs::restore(&saved.windows[0]);
    assert_eq!(urls(&restored), urls(&tabs));
    assert!(
        restored.iter().all(|t| t.discarded),
        "nothing loads at start"
    );
    assert_eq!(restored.live_count(), 0);
    assert_eq!(
        restored.closed_count(),
        1,
        "Ctrl+Shift+T works after a restart"
    );
    assert_eq!(restored.iter().nth(1).unwrap().title, "Two \"quoted\" ");
    // The tab that was in front is the one the window raises, and it alone needs a view.
    let front = saved.windows[0].active;
    let mut restored = restored;
    let id = restored.id_at(front).unwrap();
    assert_eq!(restored.get(id).unwrap().url, "https://site2.example/x");
    assert!(restored.raise(id).unwrap().needs_view);
    assert_eq!(restored.live_count(), 1);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn only_web_pages_are_written_and_the_front_tab_follows_the_ones_left_out() {
    let mut tabs = Tabs::with_first("data:text/html,start");
    let web = tabs.open("https://a.example/", Place::End).unwrap();
    tabs.open("about:blank", Place::End).unwrap();
    let window = tabs.to_window();
    assert_eq!(window.tabs.len(), 1);
    assert_eq!(window.tabs[0].url, "https://a.example/");
    assert_eq!(
        window.active, 0,
        "the front tab was the start page: the next page is kept"
    );
    tabs.raise(web);
    assert_eq!(tabs.to_window().active, 0);
    // A window with nothing worth keeping writes no window at all.
    let only_own = Tabs::with_first("data:text/html,start");
    assert!(only_own.to_session().windows.is_empty());
    assert!(Tabs::new().to_session().windows.is_empty());
}

#[test]
fn a_hostile_session_file_gives_http_tabs_only_and_few() {
    let dir = scratch("hostile");
    let mut tabs = String::new();
    for n in 0..200 {
        tabs.push_str(&format!(
            "{{\"url\":\"https://h{n}.example/\",\"title\":\"t\"}},{{\"url\":\"javascript:alert({n})\"}},"
        ));
    }
    let text = format!(
        "{{\"version\":1,\"windows\":[{{\"tabs\":[{tabs}{{\"url\":\"file:///etc/passwd\"}}],\"active\":99999,\
         \"closed\":[{{\"url\":\"data:text/html,x\"}},{{\"url\":\"http://c.example/\"}}]}}]}}"
    );
    fs::write(dir.join(session::FILE), text).unwrap();
    let saved = session::load(&dir).unwrap();
    let window = &saved.windows[0];
    assert!(window.tabs.len() <= 64);
    assert!(window.tabs.iter().all(|t| t.url.starts_with("https://")));
    assert!(window.active < window.tabs.len());
    assert_eq!(window.closed.len(), 1);
    assert_eq!(window.closed[0].url, "http://c.example/");
    // A file from before the closed list is still a session.
    let old = r#"{"version":1,"windows":[{"tabs":[{"url":"https://a.example/","title":"A"}],"active":0}]}"#;
    let parsed = session::Session::parse(old).unwrap();
    assert!(parsed.windows[0].closed.is_empty());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn the_session_is_written_whole_or_not_at_all() {
    let dir = scratch("atomic");
    let (tabs, _) = tabs_of(3);
    session::save(&dir, &tabs.to_session()).unwrap();
    assert!(
        !dir.join("session.json.tmp").exists(),
        "the temporary file is renamed away"
    );
    let first = fs::read(dir.join(session::FILE)).unwrap();
    let (more, _) = tabs_of(5);
    session::save(&dir, &more.to_session()).unwrap();
    assert_ne!(fs::read(dir.join(session::FILE)).unwrap(), first);
    assert_eq!(session::load(&dir).unwrap().windows[0].tabs.len(), 5);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn the_throttle_writes_a_whole_session_at_most_once_a_second() {
    let (tabs, ids) = tabs_of(2);
    let mut throttle: session::Throttle<session::Session> = session::Throttle::default();
    let first = tabs.to_session();
    assert_eq!(throttle.note(first.clone(), 0), Some(first));
    let mut more = tabs;
    more.close(ids[1]).unwrap();
    let newer = more.to_session();
    assert_eq!(throttle.note(newer.clone(), 500), None);
    assert_eq!(throttle.due(900), None);
    assert_eq!(throttle.due(1000), Some(newer));
}

#[test]
fn replacing_the_tabs_with_a_saved_window_never_reuses_an_id() {
    let (mut tabs, old) = tabs_of(3);
    let window = session::Window::new(
        vec![
            session::Tab {
                url: page(7),
                title: "Seven".into(),
            },
            session::Tab {
                url: page(8),
                title: String::new(),
            },
        ],
        1,
    );
    tabs.replace_with(&window);
    assert_eq!(tabs.len(), 2);
    assert!(
        tabs.active_id().is_none(),
        "the window raises the one that was in front"
    );
    assert!(tabs.iter().all(|t| t.discarded && !old.contains(&t.id)));
    assert_eq!(tabs.closed_count(), 0);
}
