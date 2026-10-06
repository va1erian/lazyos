//! The open-folder flash: started on activation, expired by its timer.

use std::ffi::OsStr;
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

use xui_explorer::MemPlatform;
use xui_explorer::platform::Platform;
use xui_explorer::window::Msg;

use super::harness::{TestLauncher, advanceable_clock, drive, drive_with_clock, mem};

#[test]
fn activating_a_folder_flashes_it_open_until_the_timer_expires() {
    let (now, clock) = advanceable_clock();
    let step_now = Rc::clone(&now);
    let (_, _) = drive_with_clock(
        mem(),
        Rc::new(TestLauncher::default()),
        "/a",
        clock,
        move |stage, handles| {
            // Entries are folders first: b is item 0.
            handles.view.set_selection(&[1]);
            stage.emit(Msg::Activate(0));
            assert!(handles.flash.is_flashing(OsStr::new("b")), "b flashes");
            assert!(handles.flash.timer_running(), "the tick timer started");
            assert_eq!(
                handles.view.selection(),
                vec![1],
                "flashing does not change the selection"
            );

            step_now.set(step_now.get() + Duration::from_millis(2_000));
            stage.emit(Msg::FlashTick);
            assert!(
                !handles.flash.is_flashing(OsStr::new("b")),
                "b reverted at the deadline"
            );
            assert!(handles.flash.is_empty());
            assert!(
                !handles.flash.timer_running(),
                "the tick timer stopped when the list emptied"
            );
        },
    );
}

#[test]
fn re_activating_a_folder_restarts_its_flash() {
    let (now, clock) = advanceable_clock();
    let step_now = Rc::clone(&now);
    let (_, _) = drive_with_clock(
        mem(),
        Rc::new(TestLauncher::default()),
        "/a",
        clock,
        move |stage, handles| {
            stage.emit(Msg::Activate(0));
            step_now.set(step_now.get() + Duration::from_millis(1_500));
            stage.emit(Msg::Activate(0));
            step_now.set(step_now.get() + Duration::from_millis(1_500));
            assert!(
                handles.flash.is_flashing(OsStr::new("b")),
                "1000 ms since the restart, not yet expired"
            );
            assert_eq!(handles.flash.len(), 1, "no duplicate entry");

            step_now.set(step_now.get() + Duration::from_millis(500));
            stage.emit(Msg::FlashTick);
            assert!(!handles.flash.is_flashing(OsStr::new("b")));
        },
    );
}

#[test]
fn several_folders_flash_at_once_under_one_timer() {
    let platform = Rc::new(
        MemPlatform::new()
            .dir("/a")
            .dir("/a/b")
            .dir("/a/c")
            .file("/a/z.txt", 1),
    );
    let (now, clock) = advanceable_clock();
    let step_now = Rc::clone(&now);
    let (_, _) = drive_with_clock(
        platform,
        Rc::new(TestLauncher::default()),
        "/a",
        clock,
        move |stage, handles| {
            stage.emit(Msg::Activate(0)); // b
            stage.emit(Msg::Activate(1)); // c
            assert!(handles.flash.is_flashing(OsStr::new("b")));
            assert!(handles.flash.is_flashing(OsStr::new("c")));
            assert_eq!(handles.flash.len(), 2);
            assert!(handles.flash.timer_running(), "one timer serves both");

            step_now.set(step_now.get() + Duration::from_millis(2_000));
            stage.emit(Msg::FlashTick);
            assert!(handles.flash.is_empty());
            assert!(!handles.flash.timer_running());
        },
    );
}

#[test]
fn deleting_a_flashing_folder_drops_its_flash_on_refresh() {
    let mem = Rc::new(
        MemPlatform::new()
            .dir("/a")
            .dir("/a/b")
            .file("/a/top.txt", 1),
    );
    let platform: Rc<dyn Platform> = mem.clone();
    let remover = Rc::clone(&mem);
    let (_, _) = drive(
        platform,
        Rc::new(TestLauncher::default()),
        "/a",
        move |stage, handles| {
            stage.emit(Msg::Activate(0)); // b
            assert!(handles.flash.is_flashing(OsStr::new("b")));
            remover.remove(Path::new("/a/b"), true).expect("remove");
            stage.emit(Msg::Refresh);
            assert!(
                !handles.flash.is_flashing(OsStr::new("b")),
                "a folder that vanished stops flashing"
            );
            assert!(handles.flash.is_empty());
            assert!(!handles.flash.timer_running(), "no idle timer is left");
        },
    );
}
