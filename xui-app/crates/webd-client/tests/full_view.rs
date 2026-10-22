//! A full view against the service half over a fake web (`support::web`): made through each scope
//! (and refused what a scope does not cover), navigated, its events decoded, its questions
//! answered, and lost.

mod support;

use messenger_generated::os_lazy_webd_view_v1 as wire;
use xui_webd::engine::EngineEvents;
use xui_webd_client::full::{FullEvent, FullState, Scope, ViewConfig};
use xui_webd_client::transport::errno::EACCES;
use xui_webd_client::{Error, Lost};

use support::full::{browser_config, data_config, net_config, web, Full};
use support::web::BASE;

fn drain(view: &mut Full) -> Vec<FullEvent> {
    let mut all = Vec::new();
    for _ in 0..4 {
        all.extend(view.poll());
    }
    all
}

fn states(events: &[FullEvent]) -> Vec<FullState> {
    events
        .iter()
        .filter_map(|event| match event {
            FullEvent::State(state) => Some(state.clone()),
            _ => None,
        })
        .collect()
}

fn url(path: &str) -> String {
    format!("{BASE}{path}")
}

#[test]
fn each_scope_makes_the_views_it_allows() {
    let web = web();
    let mut data = web.view(7, Scope::View, data_config());
    let mut net = web.view(7, Scope::Net, net_config());
    let mut browser = web.view(7, Scope::Browser, browser_config());
    for view in [&mut data, &mut net, &mut browser] {
        view.create(64, 48).expect("create");
        assert!(view.is_live());
    }
    let config = |view: &Full| web.web.config(view.view_id().unwrap()).unwrap();
    let full = |view: &Full| config(view).full.unwrap();
    assert!(full(&data).no_network);
    assert!(!full(&net).no_network);
    assert_eq!(full(&net).profile, "", "the app's own profile");
    assert!(full(&browser).profile.ends_with("browser"));
    assert!(full(&browser).deltas && !full(&net).deltas);
}

#[test]
fn a_scope_refuses_what_it_does_not_cover() {
    let web = web();
    let refused = |scope: Scope, config: ViewConfig| {
        let mut view = web.view(7, scope, config);
        let result = view.create(64, 48);
        assert!(!view.is_live());
        result
    };
    let cases = [
        (Scope::View, net_config(), "the network through view.v1"),
        (
            Scope::View,
            ViewConfig {
                profile: "browser".into(),
                ..data_config()
            },
            "the browser profile through view.v1",
        ),
        (
            Scope::Net,
            ViewConfig {
                profile: "browser".into(),
                ..net_config()
            },
            "the browser profile through net.v1",
        ),
        (
            Scope::Net,
            ViewConfig {
                downloads: true,
                ..net_config()
            },
            "downloads through net.v1",
        ),
        (
            Scope::Net,
            ViewConfig {
                handoff: wire::HANDOFF_BROWSER,
                ..net_config()
            },
            "a hand-off through net.v1",
        ),
    ];
    for (scope, config, what) in cases {
        assert_eq!(
            refused(scope, config),
            Err(Error::Refused(EACCES)),
            "{what}"
        );
    }
    // Nothing was left behind: no view, no channel.
    let world = web.world.borrow();
    assert_eq!(world.service.view_count(), 0);
    assert_eq!(world.fabric.open_handles(), 0);
}

#[test]
fn a_view_without_the_network_scope_loads_nothing_from_it() {
    let web = web();
    let mut view = web.view(7, Scope::View, data_config());
    view.create(64, 48).expect("create");
    assert_eq!(view.load(&url("/")), Err(Error::Refused(EACCES)));
    assert_eq!(view.load("file:///etc/passwd"), Err(Error::Refused(EACCES)));
    let html = format!("<title>Here</title><img src=\"{}\">", url("/nonet.png"));
    view.load_html(&html, "about:blank").expect("LoadHtml");
    let shown = states(&drain(&mut view));
    assert!(shown
        .iter()
        .any(|s| s.title == "Here" && !s.loading && s.finished > 0));
    assert!(web.web.requests().is_empty(), "{:?}", web.web.requests());
    assert!(!view.picture().is_empty());
}

#[test]
fn navigation_and_history_report_every_state_field() {
    let web = web();
    let mut view = web.view(7, Scope::Net, net_config());
    view.create(64, 48).expect("create");
    view.load(&url("/")).unwrap();
    drain(&mut view);
    assert_eq!(view.page().title, "Index");
    assert_eq!(view.page().committed_url, url("/"));
    assert!(!view.page().can_back);
    view.load(&url("/page2")).unwrap();
    drain(&mut view);
    let page = view.page().clone();
    assert_eq!((page.title.as_str(), page.can_back), ("Page Two", true));
    assert_eq!(page.progress, 1000);
    view.back().unwrap();
    drain(&mut view);
    assert_eq!(view.page().title, "Index");
    assert!(view.page().can_forward);
    view.forward().unwrap();
    drain(&mut view);
    assert_eq!(view.page().committed_url, url("/page2"));
    view.reload(false).unwrap();
    view.stop().unwrap();
    assert!(view.page().finished >= 4);
}

#[test]
fn a_held_navigation_is_asked_once_and_a_mailto_never_loads() {
    let web = web();
    let mut view = web.view(7, Scope::Net, net_config());
    view.create(64, 48).expect("create");
    let id = view.view_id().unwrap();
    view.load(&url("/mailto")).unwrap();
    let held: Vec<_> = drain(&mut view)
        .into_iter()
        .filter_map(|event| match event {
            FullEvent::NavigationRequested {
                request,
                url,
                kind,
                gesture,
            } => Some((request, url, kind, gesture)),
            _ => None,
        })
        .collect();
    assert_eq!(held.len(), 1);
    let (request, address, kind, gesture) = held[0].clone();
    assert_eq!(address, "mailto:test@example.com");
    assert_eq!((kind, gesture), (wire::NAVIGATION_KIND_OTHER, false));
    view.navigation_answer(request, true).expect("answered");
    // A `mailto:` is not the engine's to load: webd says no for the app.
    assert_eq!(web.web.answers(), vec![(id, request, false)]);
    // A second answer finds no request (webd says ESRCH): refused,
    // and the view goes on.
    assert_eq!(
        view.navigation_answer(request, true),
        Err(Error::Refused(xui_webd_client::transport::errno::ESRCH))
    );
    assert!(view.is_live());
    assert_eq!(view.page().committed_url, url("/mailto"));
}

#[test]
fn dialogs_reach_the_app_and_its_answers_reach_the_page() {
    let web = web();
    let mut view = web.view(7, Scope::Net, net_config());
    view.create(64, 48).expect("create");
    view.load(&url("/alert")).unwrap();
    let mut kinds = Vec::new();
    for _ in 0..4 {
        for event in drain(&mut view) {
            if let FullEvent::Dialog { dialog, kind, .. } = event {
                kinds.push(kind);
                view.dialog_reply(dialog, true, "").unwrap();
            }
        }
    }
    assert_eq!(
        kinds,
        vec![wire::DIALOG_KIND_ALERT, wire::DIALOG_KIND_CONFIRM]
    );
    assert_eq!(view.page().title, "confirm=true");
}

#[test]
fn blocking_downloads_popups_and_the_small_news_are_decoded() {
    let web = web();
    let mut view = web.view(7, Scope::Browser, browser_config());
    view.create(64, 48).expect("create");
    let id = view.view_id().unwrap();
    let rules = br#"[{"trigger":{"url-filter":"blocked\\.png"},"action":{"type":"block"}}]"#;
    view.blocking_compile("test", rules).unwrap();
    let events = drain(&mut view);
    let ready = FullEvent::BlockingList {
        list: "test".into(),
        status: wire::BLOCKING_STATUS_READY,
    };
    assert!(events.contains(&ready));
    view.load(&url("/")).unwrap();
    assert!(drain(&mut view).contains(&FullEvent::Blocked { count: 1 }));
    assert!(!web.web.requests().contains(&url("/blocked.png")));
    view.reload(true).unwrap();
    drain(&mut view);
    assert!(web.web.requests().contains(&url("/blocked.png")));
    // A download: started, progress, done, and the file is in the Downloads folder.
    view.load(&url("/file.bin")).unwrap();
    let events = drain(&mut view);
    let started = events.iter().any(|e| {
        matches!(e, FullEvent::DownloadStarted { name, total, .. } if name == "file.bin" && *total == 4096)
    });
    assert!(started, "{events:?}");
    assert!(events
        .iter()
        .any(|e| matches!(e, FullEvent::DownloadProgress { received: 4096, .. })));
    assert!(events
        .iter()
        .any(|e| matches!(e, FullEvent::DownloadDone { name, .. } if name == "file.bin")));
    assert!(web.downloads.join("file.bin").is_file());
    // A popup, and the latest-per-view news.
    view.load(&url("/popup")).unwrap();
    let events = drain(&mut view);
    let popup = FullEvent::NewWindowRequested {
        url: url("/popped"),
        gesture: false,
    };
    assert!(events.contains(&popup));
    let queue = web.world.borrow().queue.clone();
    queue.hover(id, "http://web.test/x");
    queue.cursor(id, wire::CURSOR_SHAPE_HAND as i32);
    queue.responsive(id, false);
    let events = drain(&mut view);
    assert!(events.contains(&FullEvent::Hover("http://web.test/x".into())));
    assert!(events.contains(&FullEvent::Cursor(wire::CURSOR_SHAPE_HAND)));
    assert!(events.contains(&FullEvent::Responsive(false)));
    view.set_blocking(false, &["web.test".into()]).unwrap();
    view.blocking_load("test").unwrap();
    view.blocking_forget("test").unwrap();
}

#[test]
fn a_lost_view_says_so_and_refuses_calls() {
    let web = web();
    let mut view = web.view(7, Scope::Net, net_config());
    view.create(64, 48).expect("create");
    let id = view.view_id().unwrap();
    web.world
        .borrow()
        .queue
        .view_lost(id, xui_webd::engine::LOST_CRASHED);
    let events = drain(&mut view);
    let lost = Lost::Engine(wire::LOST_REASON_CRASHED);
    assert_eq!(events, vec![FullEvent::Lost(lost)]);
    assert_eq!(view.lost(), Some(lost));
    assert_eq!(view.load(&url("/")), Err(Error::Lost(lost)));
    // A new view replaces it.
    view.create(64, 48).expect("a new view");
    assert!(view.is_live() && view.view_id() != Some(id));
}

#[test]
fn a_service_that_is_not_there_is_not_running() {
    let web = web();
    web.world.borrow_mut().down = true;
    let mut view = web.view(7, Scope::Browser, browser_config());
    assert_eq!(view.create(64, 48), Err(Error::NotRunning));
    web.world.borrow_mut().down = false;
    view.create(64, 48).expect("webd is back");
    // Dropping the view destroys it.
    let id = view.view_id().unwrap();
    drop(view);
    assert_eq!(web.web.destroyed(), vec![id]);
}
