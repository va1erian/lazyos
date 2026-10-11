//! Pictures: the viewer (fit, zoom asks the core for a bigger picture),
//! Save Image As from the picture's menu, and Download all images of a
//! profile, against the in-process Bluesky.

mod support;

use std::sync::Arc;
use std::time::Duration;

use lazysky_ui::prepare::Target;
use lazysky_ui::{FeedAction, Msg, Report};
use support::{
    bluesky, picture, render, row_id, save, sign_in_and_load, tick_until, watchdog, Mock,
};
use xui_canvas::snapshot::Stage;
use xui_core::widget::MenuId;
use xui_core::Theme;

const HOME: u8 = 0;

fn target(row: u64, target: Target) -> FeedAction {
    FeedAction::Target {
        row,
        target,
        at: (20, 20),
    }
}

fn settle(stage: &Stage<'_, Msg>, ms: u64) {
    tick_until(stage, Duration::from_millis(ms), || false);
}

fn wait_for(mock: &Mock, stage: &Stage<'_, Msg>, part: &str, count: usize) -> bool {
    tick_until(stage, Duration::from_secs(20), || {
        mock.to(part).len() >= count
    });
    mock.to(part).len() >= count
}

#[test]
fn the_viewer_shows_the_picture_and_zooming_asks_for_a_bigger_one() {
    watchdog(|| {
        let mock = Arc::new(bluesky());
        let base = mock.base();
        let m = Arc::clone(&mock);
        let shot = render(Theme::dark(), base.clone(), move |stage, heard| {
            sign_in_and_load(stage, heard);
            // Post 1 has one picture: open it.
            stage.emit(Msg::Feed(HOME, target(row_id(1, &base), Target::Image(0))));
            assert!(wait_for(&m, stage, "/img/1@png", 1), "no full-size fetch");
            settle(stage, 600);
            stage.emit(Msg::ViewerZoom(2));
            assert!(wait_for(&m, stage, "/img/1@png", 2), "no zoomed fetch");
            settle(stage, 600);
        });
        let hits = mock.to("/img/1@png");
        assert!(hits.len() >= 2, "{}", hits.len());
        save(&shot.image, "lazysky-viewer-zoomed-dark.png");
    });
}

#[test]
fn save_image_as_writes_the_server_bytes_to_the_chosen_path() {
    watchdog(|| {
        let mock = Arc::new(bluesky());
        let base = mock.base();
        let dir = std::env::temp_dir().join(format!("lazysky-ui-save-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let target_path = dir.join("test.png");
        let chosen = target_path.clone();
        let shot = render(Theme::light(), base.clone(), move |stage, heard| {
            sign_in_and_load(stage, heard);
            // Right click on the picture of post 1, then "Save image as...".
            stage.emit(Msg::Feed(
                HOME,
                FeedAction::Context {
                    row: row_id(1, &base),
                    target: Target::Image(0),
                    at: (30, 30),
                },
            ));
            stage.emit(Msg::ImageMenu(MenuId::new(0)));
            stage.emit(Msg::SavePicked(chosen));
            settle(stage, 1000);
        });
        // The save thread is independent of the window's ticks: give it a
        // moment after the render if it has not landed.
        for _ in 0..200 {
            if target_path.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let saved = std::fs::read(&target_path).expect("the picture was saved");
        assert_eq!(saved, picture(400, 300, 1));
        assert!(shot.log.has("SKY:SAVE:DONE:") || target_path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    });
}

#[test]
fn download_all_images_saves_a_profiles_pictures_into_its_folder() {
    watchdog(|| {
        let mock = Arc::new(bluesky());
        let base = mock.base();
        let folder = std::env::temp_dir().join("LazySky").join("me.test");
        let _ = std::fs::remove_dir_all(&folder);
        let m = Arc::clone(&mock);
        let shot = render(Theme::light(), base, move |stage, heard| {
            sign_in_and_load(stage, heard);
            stage.emit(Msg::OpenProfile(None));
            tick_until(stage, Duration::from_secs(20), || {
                heard.borrow().contains(&Report::DetailRows(5))
            });
            settle(stage, 200);
            stage.emit(Msg::BulkStart);
            tick_until(stage, Duration::from_secs(20), || {
                std::fs::read_dir(std::env::temp_dir().join("LazySky").join("me.test"))
                    .map(|d| d.count() >= 5)
                    .unwrap_or(false)
            });
            assert!(wait_for(&m, stage, "getAuthorFeed", 2));
            settle(stage, 500);
        });
        let names: Vec<String> = std::fs::read_dir(&folder)
            .expect("the folder exists")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 5, "{names:?}");
        assert!(names.iter().all(|n| !n.ends_with(".part")));
        assert!(shot.log.has("SKY:BULK:DONE:5:0"), "{:?}", shot.log.lines());
        save(&shot.image, "lazysky-bulk-done-light.png");
        let _ = std::fs::remove_dir_all(&folder);
    });
}
