//! The window built offscreen: the sign-in page, a sign-in against an
//! in-process Bluesky (timeline loaded through the tick drain and the layout
//! thread, pictures through the image threads), scrolling, "Load more" and a
//! refused password, in both themes. Snapshots land in
//! `target/snapshots/lazysky-*.png` for a human to look at.

mod support;

use std::time::Duration;

use lazysky_core::feed::LOAD_MORE_ID;
use lazysky_ui::prepare::Target;
use lazysky_ui::{FeedAction, Msg, Report};
use support::{bluesky, login, render, save, serve, sign_in_and_load, tick_until, watchdog, wheel};
use xui_core::Theme;

#[test]
fn the_sign_in_page_renders_in_both_themes() {
    watchdog(|| {
        for (theme, tag) in [(Theme::light(), "light"), (Theme::dark(), "dark")] {
            let shot = render(theme, "http://127.0.0.1:1".into(), |_, _| {});
            assert!(shot.reports.is_empty());
            save(&shot.image, &format!("lazysky-login-{tag}.png"));
        }
    });
}

#[test]
fn signing_in_loads_a_prepared_timeline_and_it_scrolls() {
    watchdog(|| {
        let mock = bluesky();
        for (theme, tag) in [(Theme::light(), "light"), (Theme::dark(), "dark")] {
            let shot = render(theme, mock.base(), |stage, heard| {
                sign_in_and_load(stage, heard);
                // Pictures near the view are fetched, decoded and cropped on
                // the image threads, then drawn.
                tick_until(stage, Duration::from_secs(30), || {
                    heard
                        .borrow()
                        .iter()
                        .any(|r| matches!(r, Report::Pictures(n) if *n >= 3))
                });
                // Scroll down 20 notches; rows there are prepared by the
                // layout thread and their pictures fetched while ticking.
                wheel(stage, -20);
                tick_until(stage, Duration::from_millis(1500), || false);
            });
            assert!(shot.reports.contains(&Report::FeedRows(30)), "{tag}");
            save(&shot.image, &format!("lazysky-feed-{tag}.png"));
        }
    });
}

#[test]
fn load_more_appends_the_next_page() {
    watchdog(|| {
        let mock = bluesky();
        let shot = render(Theme::light(), mock.base(), |stage, heard| {
            sign_in_and_load(stage, heard);
            stage.emit(Msg::Feed(
                0,
                FeedAction::Target {
                    row: LOAD_MORE_ID,
                    target: Target::LoadMore,
                    at: (0, 0),
                },
            ));
            tick_until(stage, Duration::from_secs(30), || {
                heard.borrow().contains(&Report::FeedRows(60))
            });
        });
        let rows: Vec<&Report> = shot
            .reports
            .iter()
            .filter(|r| matches!(r, Report::FeedRows(_)))
            .collect();
        assert_eq!(rows, [&Report::FeedRows(30), &Report::FeedRows(60)]);
    });
}

#[test]
fn a_refused_password_is_shown_on_the_sign_in_page() {
    watchdog(|| {
        let mock = serve(|_| {
            (
                401,
                br#"{"error":"AuthenticationRequired","message":"Invalid identifier or password"}"#
                    .to_vec(),
            )
        });
        let shot = render(Theme::light(), mock.base(), |stage, _| {
            login(stage);
            // No report marks a failure: give the round trip a moment.
            tick_until(stage, Duration::from_millis(1500), || false);
        });
        assert!(shot.reports.is_empty());
        save(&shot.image, "lazysky-login-refused-light.png");
    });
}
