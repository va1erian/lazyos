//! JavaScript dialogs of a full view: they reach the app, are rate limited per view, and the
//! app's reply goes back to the engine.

#![cfg(not(feature = "host"))]

mod support;

use messenger_generated::os_lazy_webd_view_v1 as wire;
use support::full::{data_only, FEv, Fx, Via};
use support::origin;
use xui_webd::fabric::errno::*;
use xui_webd::limits::{DIALOG_WINDOW_MS, EVENT_TEXT_BYTES};
use xui_webd::recording::Call as EngineCall;

fn reply(view: u64, dialog: u64, accepted: bool, value: &str) -> EngineCall {
    EngineCall::DialogReply {
        view,
        dialog,
        accepted,
        value: value.into(),
    }
}

#[test]
fn dialogs_reach_the_app_and_a_page_that_loops_alert_is_dismissed() {
    // Why: a dialog blocks the page's script until answered; three in ten seconds is the most
    // an app is asked, so a page that loops `alert()` cannot keep a window busy.
    let mut fx = Fx::new("dialogs");
    let app = origin(7);
    let config = wire::ViewConfig {
        dialogs: true,
        ..data_only()
    };
    let (view, events) = fx.view(app, Via::View, config, 4, 4);
    fx.engine.take();
    let prompt = wire::DIALOG_KIND_PROMPT;
    fx.queue.dialog(view, 1, prompt as i32, "Name?", "bob");
    fx.pump(100);
    assert_eq!(
        fx.news(events),
        vec![FEv::Dialog(wire::DialogArgs {
            view,
            dialog: 1,
            kind: prompt,
            message: "Name?".into(),
            default_value: "bob".into(),
        })]
    );
    fx.dialog_reply(app, view, 1, true, "alice").unwrap();
    assert_eq!(fx.engine.take(), vec![reply(view, 1, true, "alice")]);

    // The 2nd and 3rd are asked; the 4th within the window is dismissed without asking.
    for dialog in 2..=4 {
        let alert = wire::DIALOG_KIND_ALERT as i32;
        fx.queue.dialog(view, dialog, alert, "hi", "");
    }
    fx.pump(100);
    let asked: Vec<u64> = fx
        .news(events)
        .into_iter()
        .map(|e| match e {
            FEv::Dialog(d) => d.dialog,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(asked, vec![2, 3]);
    assert_eq!(fx.engine.take(), vec![reply(view, 4, false, "")]);
    let confirm = wire::DIALOG_KIND_CONFIRM as i32;
    fx.queue.dialog(view, 5, confirm, "again", "");
    fx.pump(100 + DIALOG_WINDOW_MS - 1);
    assert!(fx.news(events).is_empty());
    assert_eq!(
        fx.engine.take(),
        vec![reply(view, 5, false, "")],
        "still inside the window"
    );
    // When the window has passed, the page may ask again.
    fx.queue.dialog(view, 6, confirm, "later", "");
    fx.pump(100 + DIALOG_WINDOW_MS);
    assert_eq!(fx.news(events).len(), 1);
    assert!(fx.engine.take().is_empty());
}

#[test]
fn the_dialog_limit_is_per_view() {
    // Why: one page looping `alert()` must not use up another view's dialogs.
    let mut fx = Fx::new("dialogs-per-view");
    let app = origin(7);
    let config = wire::ViewConfig {
        dialogs: true,
        ..data_only()
    };
    let (noisy, noisy_events) = fx.view(app, Via::View, config.clone(), 4, 4);
    let (calm, calm_events) = fx.view(app, Via::View, config, 4, 4);
    fx.engine.take();
    for dialog in 1..=6 {
        fx.queue.dialog(noisy, dialog, 0, "spam", "");
    }
    fx.queue.dialog(calm, 1, 0, "hello", "");
    fx.pump(5);
    assert_eq!(fx.news(noisy_events).len(), 3);
    assert_eq!(fx.news(calm_events).len(), 1);
    assert_eq!(fx.engine.take().len(), 3, "the other three were dismissed");
}

#[test]
fn a_reply_needs_a_view_made_with_dialogs_and_a_value_that_fits() {
    // Why: a view made without dialogs has them dismissed by the engine; a reply to one would be
    // a call for something that never happened. The value travels as a C string.
    let mut fx = Fx::new("dialog-reply");
    let app = origin(7);
    let (quiet, _) = fx.view(app, Via::View, data_only(), 4, 4);
    let config = wire::ViewConfig {
        dialogs: true,
        ..data_only()
    };
    let (loud, _) = fx.view(app, Via::View, config, 4, 4);
    fx.engine.take();
    assert_eq!(fx.dialog_reply(app, quiet, 1, true, "x"), Err(EACCES));
    assert_eq!(fx.dialog_reply(app, loud, 1, true, "a\0b"), Err(EINVAL));
    let long = "a".repeat(EVENT_TEXT_BYTES + 1);
    assert_eq!(fx.dialog_reply(app, loud, 1, true, &long), Err(EINVAL));
    assert!(fx.engine.take().is_empty());
    assert!(fx.dialog_reply(app, loud, 1, false, &long[1..]).is_ok());
    assert_eq!(fx.engine.take().len(), 1);
    assert_eq!(fx.dialog_reply(origin(8), loud, 1, true, "x"), Err(ENOENT));
}
