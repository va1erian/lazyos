//! The pages beyond the timeline, against the in-process Bluesky: a profile
//! with its three tabs, bookmarks, notifications and the unread poll, search
//! (accounts, posts, a hashtag) and settings.

mod support;

use std::sync::Arc;
use std::time::Duration;

use lazysky_ui::{Msg, Report};
use support::{bluesky, render, save, sign_in_and_load, tick_until, watchdog, Mock};
use xui_canvas::snapshot::Stage;
use xui_core::Theme;

/// Ticks until the mock has seen a request to `part`.
fn wait_for(mock: &Mock, stage: &Stage<'_, Msg>, part: &str) -> bool {
    tick_until(stage, Duration::from_secs(20), || !mock.to(part).is_empty());
    !mock.to(part).is_empty()
}

fn settle(stage: &Stage<'_, Msg>, ms: u64) {
    tick_until(stage, Duration::from_millis(ms), || false);
}

#[test]
fn a_profile_loads_its_counts_and_its_three_tabs() {
    watchdog(|| {
        let mock = Arc::new(bluesky());
        let m = Arc::clone(&mock);
        let shot = render(Theme::light(), mock.base(), move |stage, heard| {
            sign_in_and_load(stage, heard);
            stage.emit(Msg::OpenProfile(None));
            assert!(wait_for(&m, stage, "getAuthorFeed"));
            tick_until(stage, Duration::from_secs(20), || {
                heard.borrow().contains(&Report::DetailRows(5))
            });
            settle(stage, 300);
            stage.emit(Msg::ProfileTab(1));
            tick_until(stage, Duration::from_secs(20), || {
                m.to("getAuthorFeed").len() >= 2
            });
            stage.emit(Msg::ProfileTab(2));
            assert!(wait_for(&m, stage, "getActorLikes"));
            settle(stage, 300);
        });
        let profile = mock.to("getProfile");
        assert!(
            profile[0].path.contains("actor=did%3Aplc%3Ame"),
            "{}",
            profile[0].path
        );
        let feeds = mock.to("getAuthorFeed");
        assert!(feeds[0].path.contains("filter=posts_no_replies"));
        assert!(feeds[1].path.contains("filter=posts_with_media"));
        save(&shot.image, "lazysky-profile-likes-light.png");
    });
}

#[test]
fn bookmarks_notifications_and_the_unread_poll() {
    watchdog(|| {
        let mock = Arc::new(bluesky());
        let m = Arc::clone(&mock);
        render(Theme::dark(), mock.base(), move |stage, heard| {
            sign_in_and_load(stage, heard);
            // The unread count is asked for as soon as the timeline shows.
            assert!(wait_for(&m, stage, "getUnreadCount"));
            stage.emit(Msg::OpenBookmarks);
            tick_until(stage, Duration::from_secs(20), || {
                heard.borrow().contains(&Report::DetailRows(3))
            });
            stage.emit(Msg::Back);
            stage.emit(Msg::OpenNotifications);
            assert!(wait_for(&m, stage, "listNotifications"));
            // Opening the list marks everything seen.
            assert!(wait_for(&m, stage, "updateSeen"));
            settle(stage, 300);
        });
        assert!(!mock.to("getBookmarks").is_empty());
        assert!(mock.to("updateSeen")[0].json()["seenAt"].is_string());
    });
}

#[test]
fn search_finds_accounts_then_posts_and_a_hashtag_goes_to_posts() {
    watchdog(|| {
        let mock = Arc::new(bluesky());
        let m = Arc::clone(&mock);
        render(Theme::light(), mock.base(), move |stage, heard| {
            sign_in_and_load(stage, heard);
            stage.emit(Msg::SearchText("alice".into()));
            stage.emit(Msg::SearchGo);
            assert!(wait_for(&m, stage, "searchActors"));
            stage.emit(Msg::SearchToggle);
            assert!(wait_for(&m, stage, "searchPosts"));
            settle(stage, 300);
            stage.emit(Msg::Back);
            stage.emit(Msg::SearchText("#rust".into()));
            stage.emit(Msg::SearchGo);
            tick_until(stage, Duration::from_secs(20), || {
                m.to("searchPosts").len() >= 2
            });
        });
        assert!(mock.to("searchActors")[0].path.contains("q=alice"));
        let posts = mock.to("searchPosts");
        assert!(posts[0].path.contains("q=alice"));
        assert!(posts[1].path.contains("q=%23rust"), "{}", posts[1].path);
    });
}

#[test]
fn settings_are_saved_and_the_picture_height_relays_the_rows() {
    watchdog(|| {
        let mock = Arc::new(bluesky());
        let shot = render(Theme::light(), mock.base(), move |stage, heard| {
            sign_in_and_load(stage, heard);
            stage.emit(Msg::OpenSettings);
            stage.emit(Msg::SettingsTheme(2));
            stage.emit(Msg::SettingsMaxHeight(200));
            stage.emit(Msg::SettingsRemember(true));
            settle(stage, 400);
        });
        assert!(
            shot.log.has("SKY:SETTINGS:SAVED:"),
            "{:?}",
            shot.log.lines()
        );
        let saved = lazysky_core::Settings::load(&std::env::temp_dir());
        assert_eq!(saved.theme, lazysky_core::ThemeChoice::Dark);
        assert_eq!(saved.max_image_height, 200);
        assert!(saved.remember_handle);
        assert_eq!(saved.handle.as_deref(), Some("me.test"));
        let _ = std::fs::remove_file(std::env::temp_dir().join("settings.toml"));
        save(&shot.image, "lazysky-settings-dark.png");
    });
}
