//! What the user does in the window, against the in-process Bluesky: like
//! and unlike (optimistic, rolled back on a refusal), open a thread and go
//! back, the people list, delete an own post, a link, compose a reply.

mod support;

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use lazysky_ui::prepare::{Action, Target};
use lazysky_ui::{FeedAction, Msg, Report};
use support::{bluesky, render, row_id, save, sign_in_and_load, tick_until, watchdog, Mock, ME};
use xui_canvas::snapshot::Stage;
use xui_core::widget::MenuId;
use xui_core::Theme;

const HOME: u8 = 0;

fn click(row: u64, target: Target) -> Msg {
    Msg::Feed(
        HOME,
        FeedAction::Target {
            row,
            target,
            at: (20, 20),
        },
    )
}

/// Ticks until the mock has seen a request to `part`.
fn wait_for(mock: &Mock, stage: &Stage<'_, Msg>, part: &str) -> bool {
    tick_until(stage, Duration::from_secs(20), || !mock.to(part).is_empty());
    !mock.to(part).is_empty()
}

/// Ticks for `ms` so what is in flight lands.
fn settle(stage: &Stage<'_, Msg>, ms: u64) {
    tick_until(stage, Duration::from_millis(ms), || false);
}

#[test]
fn like_then_unlike_round_trip() {
    watchdog(|| {
        let mock = Arc::new(bluesky());
        let base = mock.base();
        let m = Arc::clone(&mock);
        let shot = render(Theme::light(), base.clone(), move |stage, heard| {
            sign_in_and_load(stage, heard);
            let row = row_id(1, &base);
            stage.emit(click(row, Target::Action(Action::Like)));
            assert!(wait_for(&m, stage, "createRecord"), "no like was sent");
            // Let the confirmation land, then unlike: the record is deleted.
            settle(stage, 300);
            stage.emit(click(row, Target::Action(Action::Like)));
            assert!(wait_for(&m, stage, "deleteRecord"), "no delete was sent");
            settle(stage, 300);
        });
        let like = mock.to("createRecord")[0].json();
        assert_eq!(like["repo"], ME);
        assert_eq!(like["collection"], "app.bsky.feed.like");
        assert_eq!(
            like["record"]["subject"]["uri"],
            "at://did:plc:a/app.bsky.feed.post/1"
        );
        assert_eq!(like["record"]["subject"]["cid"], "bafy1");
        let undo = mock.to("deleteRecord")[0].json();
        assert_eq!(undo["collection"], "app.bsky.feed.like");
        assert_eq!(undo["rkey"], "3knew");
        assert!(shot.log.has("SKY:LIKE:ok"));
    });
}

#[test]
fn a_refused_like_is_rolled_back_and_said() {
    watchdog(|| {
        let mock = Arc::new(bluesky());
        let base = mock.base();
        let m = Arc::clone(&mock);
        let shot = render(Theme::light(), base.clone(), move |stage, heard| {
            sign_in_and_load(stage, heard);
            m.fail_writes.store(true, Ordering::SeqCst);
            stage.emit(click(row_id(1, &base), Target::Action(Action::Like)));
            assert!(wait_for(&m, stage, "createRecord"));
            settle(stage, 500);
        });
        assert!(shot.log.has("SKY:LIKE:fail"), "{:?}", shot.log.lines());
        save(&shot.image, "lazysky-like-refused-light.png");
    });
}

#[test]
fn a_row_opens_its_thread_and_back_returns() {
    watchdog(|| {
        let mock = Arc::new(bluesky());
        let base = mock.base();
        let m = Arc::clone(&mock);
        let shot = render(Theme::dark(), base.clone(), move |stage, heard| {
            sign_in_and_load(stage, heard);
            stage.emit(Msg::Feed(HOME, FeedAction::Row(row_id(2, &base))));
            assert!(wait_for(&m, stage, "getPostThread"));
            tick_until(stage, Duration::from_secs(20), || {
                heard.borrow().contains(&Report::DetailRows(3))
            });
            settle(stage, 300);
        });
        assert!(
            shot.reports.contains(&Report::DetailRows(3)),
            "{:?}",
            shot.reports
        );
        let thread = mock.to("getPostThread");
        assert!(thread[0]
            .path
            .contains("at%3A%2F%2Fdid%3Aplc%3Aa%2Fapp.bsky.feed.post%2F2"));
        save(&shot.image, "lazysky-thread-dark.png");

        // And back: the timeline is on top again.
        let base = mock.base();
        let shot = render(Theme::dark(), base.clone(), move |stage, heard| {
            sign_in_and_load(stage, heard);
            stage.emit(Msg::Feed(HOME, FeedAction::Row(row_id(2, &base))));
            tick_until(stage, Duration::from_secs(20), || {
                heard.borrow().contains(&Report::DetailRows(3))
            });
            stage.emit(Msg::Back);
            settle(stage, 300);
        });
        save(&shot.image, "lazysky-after-back-dark.png");
    });
}

#[test]
fn the_menu_shows_who_liked_a_post() {
    watchdog(|| {
        let mock = Arc::new(bluesky());
        let base = mock.base();
        let m = Arc::clone(&mock);
        let shot = render(Theme::light(), base.clone(), move |stage, heard| {
            sign_in_and_load(stage, heard);
            // "..." on post 1 opens the menu; "Liked by..." is entry 2.
            stage.emit(click(row_id(1, &base), Target::Action(Action::More)));
            stage.emit(Msg::PostMenu(MenuId::new(2)));
            assert!(wait_for(&m, stage, "getLikes"));
            settle(stage, 500);
        });
        assert!(!mock.to("getLikes").is_empty());
        save(&shot.image, "lazysky-people-light.png");
    });
}

#[test]
fn only_an_own_post_can_be_deleted() {
    watchdog(|| {
        // Post 1 is Alice's: the menu's Delete (entry 4) must not send.
        let mock = Arc::new(bluesky());
        let base = mock.base();
        let m = Arc::clone(&mock);
        render(Theme::light(), base.clone(), move |stage, heard| {
            sign_in_and_load(stage, heard);
            stage.emit(click(row_id(1, &base), Target::Action(Action::More)));
            stage.emit(Msg::PostMenu(MenuId::new(4)));
            settle(stage, 500);
            assert!(
                m.to("deleteRecord").is_empty(),
                "deleted someone else's post"
            );
        });
        // Post 5 is the user's own: it is deleted.
        let mock = Arc::new(bluesky());
        let base = mock.base();
        let m = Arc::clone(&mock);
        render(Theme::light(), base.clone(), move |stage, heard| {
            sign_in_and_load(stage, heard);
            stage.emit(click(row_id(5, &base), Target::Action(Action::More)));
            stage.emit(Msg::PostMenu(MenuId::new(4)));
            assert!(wait_for(&m, stage, "deleteRecord"));
        });
        let del = mock.to("deleteRecord")[0].json();
        assert_eq!(del["collection"], "app.bsky.feed.post");
        assert_eq!(del["rkey"], "5");
    });
}

#[test]
fn a_link_goes_to_the_platform_and_a_reply_to_the_server() {
    watchdog(|| {
        let mock = Arc::new(bluesky());
        let base = mock.base();
        let m = Arc::clone(&mock);
        let shot = render(Theme::light(), base.clone(), move |stage, heard| {
            sign_in_and_load(stage, heard);
            stage.emit(click(
                row_id(7, &base),
                Target::Link("https://example.test/7".into()),
            ));
            // Reply to post 2: open the composer, type, post.
            stage.emit(Msg::Compose(Some(
                "at://did:plc:a/app.bsky.feed.post/2".into(),
            )));
            stage.emit(Msg::ComposerText("good point".into()));
            stage.emit(Msg::ComposerPost);
            assert!(wait_for(&m, stage, "createRecord"));
        });
        assert!(shot.log.has("OPEN:https://example.test/7"));
        let rec = mock.to("createRecord")[0].json();
        assert_eq!(rec["collection"], "app.bsky.feed.post");
        let reply = &rec["record"]["reply"];
        assert_eq!(
            reply["parent"]["uri"],
            "at://did:plc:a/app.bsky.feed.post/2"
        );
        assert_eq!(reply["root"]["uri"], "at://did:plc:a/app.bsky.feed.post/2");
        assert!(rec["record"]["text"]
            .as_str()
            .unwrap()
            .contains("good point"));
    });
}
