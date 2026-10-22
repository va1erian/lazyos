//! `RemoteEngine` against the service half over a fake web (`xui-webd-client`'s test world): the
//! seam's calls reach webd, and what webd sends reaches the window's mailbox as it did from the
//! in-process engine (frames, deltas, the page's state, the news), with the browser's own decisions
//! (held navigations, the person's choices replayed into a new view, a lost view).

#[path = "../../webd-client/tests/support/mod.rs"]
mod support;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use xui_webd_client::transport::ScopedTransport;
use xui_webd_client::SlotPlan;
use xui_webkit::abi::NavKind;
use xui_webkit::engine::{Engine, NotifyAnswer};
use xui_webkit::events::Event;
use xui_webkit::mailbox::{Mailbox, PageState};
use xui_webkit_remote::{browser_config, Health, Navigation, Options, RemoteEngine};

use support::full::{web, Web};
use support::web::{FakeWeb, BASE};
use support::TestTransport;

type Remote = RemoteEngine<TestTransport<FakeWeb>>;

struct Rig {
    web: Web,
    mailbox: Arc<Mailbox>,
    engine: Remote,
    asked: Rc<RefCell<Vec<Navigation>>>,
    channels: Rc<RefCell<Vec<Option<u64>>>>,
}

fn rig_with(permissions: Vec<(String, NotifyAnswer)>) -> Rig {
    let web = web();
    let mailbox = Arc::new(Mailbox::default());
    let asked = Rc::new(RefCell::new(Vec::new()));
    let channels = Rc::new(RefCell::new(Vec::new()));
    let options = Options {
        config: browser_config(false),
        slots: SlotPlan::default(),
        navigator: {
            let asked = Rc::clone(&asked);
            Box::new(move |navigation: &Navigation| {
                asked.borrow_mut().push(navigation.clone());
                Some(Event::Launch {
                    url: navigation.url.clone(),
                    by_user: false,
                })
            })
        },
        permissions,
        watch: {
            let channels = Rc::clone(&channels);
            Box::new(move |channel| channels.borrow_mut().push(channel))
        },
        wake: Box::new(|| {}),
    };
    let transport = TestTransport::full(&web.world, 7);
    let engine = RemoteEngine::new(transport, Arc::clone(&mailbox), options);
    Rig {
        web,
        mailbox,
        engine,
        asked,
        channels,
    }
}

fn rig() -> Rig {
    rig_with(Vec::new())
}

fn url(path: &str) -> String {
    format!("{BASE}{path}")
}

impl Rig {
    fn pump(&self) {
        for _ in 0..4 {
            self.engine.pump();
        }
    }

    fn state(&self) -> PageState {
        self.mailbox.take_state().expect("a state was posted")
    }

    fn open(&self, path: &str) -> PageState {
        self.engine.load(&url(path));
        self.pump();
        self.state()
    }

    fn view(&self) -> u64 {
        self.web.web.web_view()
    }
}

/// The fake web's newest view (the engine makes one at a time).
trait NewestView {
    fn web_view(&self) -> u64;
}

impl NewestView for FakeWeb {
    fn web_view(&self) -> u64 {
        (1..64).rev().find(|v| self.size(*v) != (0, 0)).unwrap_or(0)
    }
}

#[test]
fn a_load_reaches_webd_and_its_page_and_picture_reach_the_mailbox() {
    let rig = rig();
    rig.engine.load(&url("/"));
    assert_eq!(rig.engine.health(), Health::Waiting, "no size, no view yet");
    rig.engine.resize(64, 48);
    assert_eq!(rig.engine.health(), Health::Live);
    assert_eq!(
        rig.channels.borrow().len(),
        1,
        "the window is told the channel"
    );
    rig.pump();
    let state = rig.state();
    assert_eq!(state.title, "Index");
    assert_eq!(state.committed_url, url("/"));
    assert!(!state.loading && state.finished == 1 && !state.crashed);
    let update = rig.mailbox.take_update(Vec::new()).expect("a frame");
    let frame = update.full.expect("a whole frame");
    assert_eq!((frame.width, frame.height), (64, 48));
    assert_eq!(frame.rgba.len(), 64 * 48 * 4);
    // No rule list yet: the page's picture is fetched too.
    assert_eq!(rig.web.web.requests(), vec![url("/"), url("/blocked.png")]);
}

#[test]
fn a_wheel_turn_reaches_the_window_as_a_delta_with_its_shift() {
    let rig = rig();
    rig.engine.resize(64, 48);
    rig.open("/long");
    let _ = rig.mailbox.take_update(Vec::new());
    rig.engine.wheel(10, 10, 0, -1, 0);
    rig.pump();
    let update = rig
        .mailbox
        .take_update(Vec::new())
        .expect("news of the scroll");
    assert!(update.full.is_none(), "a delta, not a whole frame");
    let delta = &update.deltas[0];
    assert_eq!(delta.size, (64, 48));
    assert!(delta.scroll.is_some_and(|s| s.dy < 0), "{:?}", delta.scroll);
    assert!(!delta.patches.is_empty());
    assert!(update.scroll_y.is_some_and(|y| y > 0));
}

#[test]
fn a_held_navigation_is_refused_in_webd_and_decided_by_the_window() {
    let rig = rig();
    rig.engine.resize(64, 48);
    rig.open("/mailto");
    let asked = rig.asked.borrow().clone();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].url, "mailto:test@example.com");
    assert_eq!(asked[0].kind, NavKind::Other);
    assert!(!asked[0].gesture);
    assert_eq!(asked[0].committed_url, url("/mailto"));
    assert!(rig.web.web.answers().iter().all(|(_, _, allow)| !allow));
    assert!(!rig.web.web.answers().is_empty());
    let news = rig.mailbox.take_events();
    assert!(news.contains(&Event::Launch {
        url: "mailto:test@example.com".into(),
        by_user: false
    }));
}

#[test]
fn the_blockers_calls_wait_for_the_view_and_the_persons_choices_are_replayed() {
    let rig = rig_with(vec![("https://a.test".into(), NotifyAnswer::Allow)]);
    rig.engine.set_blocking(true, &["example.com".into()]);
    rig.engine.blocking_compile("test", "[]");
    rig.engine.resize(64, 48);
    rig.pump();
    let view = rig.view();
    assert!(rig
        .web
        .web
        .permissions()
        .contains(&(view, "https://a.test".into(), 1)));
    let news = rig.mailbox.take_events();
    assert!(news
        .iter()
        .any(|e| matches!(e, Event::BlockingList { id, .. } if id == "test")));
    // The list blocks the index page's picture.
    rig.open("/");
    assert!(!rig.web.web.requests().contains(&url("/blocked.png")));
}

#[test]
fn recover_and_forget_reach_webd() {
    let rig = rig();
    rig.engine.resize(64, 48);
    rig.open("/");
    rig.engine.recover(true);
    rig.engine.notify_forget("https://a.test");
    let view = rig.view();
    assert_eq!(rig.web.web.recovers(), vec![(view, true)]);
    assert_eq!(rig.web.web.forgets(), vec![(view, "https://a.test".into())]);
}

#[test]
fn webd_going_away_blanks_the_page_and_a_new_view_loads_it_again() {
    let rig = rig();
    rig.engine.resize(64, 48);
    rig.open("/page2");
    rig.web.world.borrow_mut().fail_recv = Some(-32);
    rig.pump();
    let state = rig.state();
    assert!(state.crashed && !state.error.is_empty());
    assert_eq!(rig.engine.health(), Health::NotRunning);
    assert_eq!(rig.channels.borrow().last(), Some(&None));
    rig.web.world.borrow_mut().fail_recv = None;
    std::thread::sleep(std::time::Duration::from_millis(1100));
    rig.pump();
    assert_eq!(rig.engine.health(), Health::Live);
    let state = rig.state();
    assert_eq!(state.title, "Page Two");
    assert!(!state.crashed);
    assert_eq!(state.finished, 2, "the count goes on from the old view's");
}

#[test]
fn no_webd_says_so_and_a_page_asked_for_meanwhile_loads_when_it_comes() {
    let rig = rig();
    rig.web.world.borrow_mut().down = true;
    rig.engine.load(&url("/page2"));
    rig.engine.resize(64, 48);
    assert_eq!(rig.engine.health(), Health::NotRunning);
    // An app opened at login may start before webd: the first tries are quiet.
    assert!(rig.mailbox.take_state().is_none());
    // The window's slow beat covers the retries: no fast timer for them.
    assert!(!rig.engine.pump());
    let mut said = None;
    for _ in 0..6 {
        std::thread::sleep(std::time::Duration::from_millis(1050));
        rig.engine.pump();
        if let Some(state) = rig.mailbox.take_state() {
            said = Some(state);
            break;
        }
    }
    let state = said.expect("after a few tries the page says webd is missing");
    assert!(state.error.contains("not running"), "{}", state.error);
    rig.web.world.borrow_mut().down = false;
    std::thread::sleep(std::time::Duration::from_millis(1100));
    rig.pump();
    assert_eq!(rig.state().title, "Page Two");
}

#[test]
fn a_dialog_and_a_window_request_become_the_windows_news() {
    let rig = rig();
    rig.engine.resize(64, 48);
    rig.open("/alert");
    let news = rig.mailbox.take_events();
    let Some(Event::Dialog { id, text, .. }) = news
        .iter()
        .find(|e| matches!(e, Event::Dialog { .. }))
        .cloned()
    else {
        panic!("no dialog in {news:?}");
    };
    assert_eq!(text, "Hello");
    rig.engine.dialog_reply(id, true, "");
    let view = rig.view();
    assert_eq!(rig.web.web.replies(), vec![(view, id, true)]);
    rig.open("/popup");
    let news = rig.mailbox.take_events();
    assert!(
        !news.iter().any(|e| matches!(e, Event::Popup { .. })),
        "the page's own window.open opens nothing"
    );
}

#[test]
fn a_page_whose_process_ended_stays_blank_through_a_resize_and_reload_loads_it_again() {
    let rig = rig();
    rig.engine.resize(64, 48);
    rig.open("/page2");
    let first = rig.view();
    let queue = rig.web.world.borrow().queue.clone();
    xui_webd::engine::EngineEvents::view_lost(&*queue, first, xui_webd::engine::LOST_CRASHED);
    rig.pump();
    let state = rig.state();
    assert!(
        state.crashed && state.error.contains("crashed"),
        "{state:?}"
    );
    assert_eq!(rig.engine.health(), Health::Ended);
    // A resize (a maximize) makes no new, empty view: the page stays blank and says why.
    rig.engine.resize(80, 60);
    assert_eq!(rig.engine.health(), Health::Ended);
    assert!(rig.mailbox.take_state().is_none());
    // Reload loads the page on show in a new view.
    rig.engine.reload();
    rig.pump();
    assert_eq!(rig.engine.health(), Health::Live);
    let state = rig.state();
    assert_eq!(state.title, "Page Two");
    assert!(!state.crashed);
    assert_ne!(rig.view(), first);
}

#[test]
fn a_decision_forgotten_while_there_is_no_view_is_told_to_the_next_one() {
    let rig = rig_with(vec![("https://a.test".into(), NotifyAnswer::Allow)]);
    rig.engine.notify_forget("https://a.test");
    rig.engine.resize(64, 48);
    rig.pump();
    let view = rig.view();
    assert!(
        !rig.web
            .web
            .permissions()
            .contains(&(view, "https://a.test".into(), 1)),
        "a forgotten decision is not replayed"
    );
    assert_eq!(rig.web.web.forgets(), vec![(view, "https://a.test".into())]);
}

#[test]
fn a_pages_notification_reaches_the_window() {
    let rig = rig_with(vec![("https://a.test".into(), NotifyAnswer::Allow)]);
    rig.engine.resize(64, 48);
    rig.pump();
    let view = rig.view();
    let queue = rig.web.world.borrow().queue.clone();
    queue.notify_show_from(
        view,
        5,
        "https://a.test",
        "https://a.test/p",
        "Hi",
        "There",
        "t",
    );
    rig.pump();
    let news = rig.mailbox.take_events();
    let shown = Event::NotifyShow {
        id: 5,
        origin: "https://a.test".into(),
        page_url: "https://a.test/p".into(),
        title: "Hi".into(),
        body: "There".into(),
        tag: "t".into(),
    };
    assert!(news.contains(&shown), "{news:?}");
    // The window's answers go to webd and leave the view as it was.
    rig.engine.notify_shown(5);
    rig.engine.notify_closed(5);
    assert_eq!(rig.engine.health(), Health::Live);
}

#[test]
fn a_long_page_of_the_browsers_own_is_given_as_html_and_told_back_by_its_address() {
    let rig = rig();
    rig.engine.resize(64, 48);
    rig.pump();
    let page = format!(
        "data:text/html;charset=utf-8,%3Ctitle%3EHistory%3C%2Ftitle%3E{}",
        "x".repeat(9000)
    );
    rig.engine.load(&page);
    rig.pump();
    let state = rig.state();
    assert_eq!(state.title, "History");
    assert_eq!(
        state.committed_url, page,
        "the window knows its page by its address"
    );
    assert!(rig.web.web.requests().is_empty(), "nothing fetched");
}

#[test]
fn a_transport_for_the_tests_is_a_scoped_one() {
    fn scoped<T: ScopedTransport>() {}
    scoped::<TestTransport<FakeWeb>>();
}
