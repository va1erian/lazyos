//! The small news a full view's page sends its client: state, link under the pointer, pointer
//! shape, responsiveness, new windows, and that every text is cut to a size a client can receive.

#![cfg(not(feature = "host"))]

mod support;

use messenger_generated::os_lazy_webd_view_v1 as wire;
use support::full::{data_only, web, FEv, Fx, Via};
use support::origin;
use xui_webd::engine::EngineEvents;
use xui_webd::events::PageNow;
use xui_webd::limits::{EVENT_BYTES_MAX, EVENT_TEXT_BYTES};

#[test]
fn the_pages_texts_are_cut_before_they_are_sent() {
    // Why: a page with scripts can make megabytes of message, title or address; each text is cut
    // to `EVENT_TEXT_BYTES` at a character boundary, when queued and again when sent.
    let mut fx = Fx::new("texts");
    let app = origin(7);
    let config = wire::ViewConfig {
        dialogs: true,
        popups: true,
        ..data_only()
    };
    let (view, events) = fx.view(app, Via::View, config, 4, 4);
    let huge = "\u{e9}".repeat(EVENT_TEXT_BYTES);
    fx.queue.dialog(view, 1, 0, &huge, &huge);
    fx.queue.page_state(
        view,
        PageNow {
            url: huge.clone(),
            title: huge.clone(),
            error: huge.clone(),
            committed_url: huge.clone(),
            ..PageNow::default()
        },
    );
    fx.queue.hover(view, &huge);
    fx.queue.new_window(view, &huge, true);
    fx.pump(0);
    let mut seen = 0;
    for event in fx.news(events) {
        let lengths: Vec<usize> = match event {
            FEv::Dialog(d) => vec![d.message.len(), d.default_value.len()],
            FEv::State(s) => vec![
                s.url.len(),
                s.title.len(),
                s.error.len(),
                s.committed_url.len(),
            ],
            FEv::Hover(h) => vec![h.url.len()],
            FEv::NewWindow(n) => vec![n.url.len()],
            other => panic!("{other:?}"),
        };
        assert!(
            lengths.iter().all(|len| *len == EVENT_TEXT_BYTES),
            "{lengths:?}"
        );
        seen += 1;
    }
    assert_eq!(seen, 4);
}

#[test]
#[ignore = "bug: a full view's State carries four texts (url, title, error, committed_url) of up to \
            EVENT_TEXT_BYTES each, 192 KiB, but EVENT_BYTES_MAX (and the client's 100 KiB receive \
            buffer) is sized for the light State's two texts, so the kernel drops the event \
            (EMSGSIZE); service/deliver.rs deliver_pages, limits.rs EVENT_BYTES_MAX"]
fn the_largest_event_of_a_full_view_fits_the_clients_receive_buffer() {
    // Why: the kernel drops a message larger than the receiver's buffer, so the client would never
    // hear that page state (title, address, progress) at all.
    let mut fx = Fx::new("event-size");
    let app = origin(7);
    let config = wire::ViewConfig {
        dialogs: true,
        ..data_only()
    };
    let (view, events) = fx.view(app, Via::View, config, 4, 4);
    let huge = "a".repeat(EVENT_TEXT_BYTES);
    fx.queue.dialog(view, 1, 0, &huge, &huge);
    fx.queue.page_state(
        view,
        PageNow {
            url: huge.clone(),
            title: huge.clone(),
            error: huge.clone(),
            committed_url: huge,
            ..PageNow::default()
        },
    );
    fx.pump(0);
    // The parcel header travels with the body (as `backpressure.rs` counts it).
    let header = std::mem::size_of::<libmessenger::Header>() + 64;
    let sent = fx.fabric.take_tagged(events);
    assert!(!sent.is_empty());
    for (_, method, body) in sent {
        assert!(
            body.len() + header <= EVENT_BYTES_MAX,
            "method {method}: {} bytes",
            body.len()
        );
    }
}

#[test]
fn the_pages_state_and_small_news_reach_the_client_and_a_new_window_only_if_asked_for() {
    // Why: these are the events a browser's chrome is drawn from (address, progress, lock, link
    // under the pointer, pointer shape, responsiveness).
    let mut fx = Fx::new("page-news");
    let app = origin(7);
    let loud = wire::ViewConfig {
        popups: true,
        ..web()
    };
    let (view, events) = fx.view(app, Via::Net, loud, 4, 4);
    let (quiet, quiet_events) = fx.view(app, Via::Net, web(), 4, 4);
    let page = PageNow {
        url: "https://example.com/next".into(),
        title: "T".into(),
        loading: true,
        can_back: true,
        can_forward: false,
        progress: 420,
        error: String::new(),
        committed_url: "https://example.com/".into(),
        secure: true,
        finished: 3,
    };
    fx.queue.page_state(view, page);
    fx.queue.hover(view, "https://example.com/link");
    fx.queue.cursor(view, wire::CURSOR_SHAPE_HAND as i32);
    fx.queue.responsive(view, false);
    fx.queue.new_window(view, "https://example.com/popup", true);
    fx.queue
        .new_window(quiet, "https://example.com/popup", true);
    fx.queue.link_clicked(view, "https://example.com/c");
    fx.queue.clipboard_write(view, "copied");
    fx.pump(0);
    let sent = fx.news(events);
    let state = wire::StateArgs {
        view,
        title: "T".into(),
        loading: true,
        finished: 3,
        error: String::new(),
        url: "https://example.com/next".into(),
        committed_url: "https://example.com/".into(),
        can_back: true,
        can_forward: false,
        progress: 420,
        secure: true,
    };
    assert!(sent.contains(&FEv::State(state)));
    let hover = wire::HoverArgs {
        view,
        url: "https://example.com/link".into(),
    };
    assert!(sent.contains(&FEv::Hover(hover)));
    let hand = wire::CursorArgs {
        view,
        shape: wire::CURSOR_SHAPE_HAND,
    };
    assert!(sent.contains(&FEv::Cursor(hand)));
    let hung = wire::ResponsiveArgs {
        view,
        responsive: false,
    };
    assert!(sent.contains(&FEv::Responsive(hung)));
    let popup = wire::NewWindowRequestedArgs {
        view,
        url: "https://example.com/popup".into(),
        gesture: true,
    };
    assert!(sent.contains(&FEv::NewWindow(popup)));
    let link = wire::LinkClickedArgs {
        view,
        url: "https://example.com/c".into(),
    };
    assert!(sent.contains(&FEv::Link(link)));
    let copied = wire::ClipboardWriteArgs {
        view,
        text: "copied".into(),
    };
    assert!(sent.contains(&FEv::Clipboard(copied)));
    assert!(
        fx.news(quiet_events).is_empty(),
        "a view that did not ask for popups hears of none"
    );
    let _ = quiet;
}
