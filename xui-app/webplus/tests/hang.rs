//! A page whose process stopped answering (issue #862), driven through the LazyWebPlus window
//! offscreen against a recording engine: the bar comes up, Wait puts it away for that episode,
//! Reload and Stop ask the engine to end the stuck process. Snapshots go to
//! `xui-app/target/snapshots/lazywebplus-hang*.png`.

mod support;

use support::*;
use xui_webkit::engine::Call;
use xui_webkit::events::Event;
use xui_webplus::app::Msg;
use xui_webplus::hang::Answer;

fn recovers(calls: &[Call]) -> Vec<bool> {
    calls
        .iter()
        .filter_map(|call| match call {
            Call::Recover(reload) => Some(*reload),
            _ => None,
        })
        .collect()
}

#[test]
fn reload_ends_the_stuck_process_and_loads_the_page_again() {
    let (image, calls) = drive("hang-reload", None, |stage, rig| {
        rig.state(stage, loaded("http://example.com/", "Example", 1));
        rig.events(stage, vec![Event::PageResponsive(false)]);
        stage.emit(Msg::Hang(Answer::Reload));
        // The engine says the process is gone (nothing left to wait for).
        rig.events(stage, vec![Event::PageResponsive(true)]);
    });
    save(&image, "lazywebplus-hang-reload.png");
    assert_eq!(recovers(&calls), [true]);
}

#[test]
fn stop_ends_it_and_leaves_the_page_stopped() {
    let (_, calls) = drive("hang-stop", None, |stage, rig| {
        rig.events(stage, vec![Event::PageResponsive(false)]);
        stage.emit(Msg::Hang(Answer::Stop));
    });
    assert_eq!(recovers(&calls), [false]);
}

#[test]
fn wait_asks_nothing_of_the_engine_and_a_new_episode_asks_again() {
    let (image, calls) = drive("hang-wait", None, |stage, rig| {
        rig.events(stage, vec![Event::PageResponsive(false)]);
        stage.emit(Msg::Hang(Answer::Wait));
        rig.events(stage, vec![Event::PageResponsive(true)]);
        rig.events(stage, vec![Event::PageResponsive(false)]);
    });
    // The second episode's bar is up in the picture.
    save(&image, "lazywebplus-hang.png");
    assert!(recovers(&calls).is_empty());
}

#[test]
fn reload_and_stop_of_the_toolbar_and_menu_end_a_stuck_page_even_after_a_wait() {
    let (_, calls) = drive("hang-toolbar", None, |stage, rig| {
        rig.events(stage, vec![Event::PageResponsive(false)]);
        stage.emit(Msg::Hang(Answer::Wait));
        // The toolbar's button is Reload while nothing loads; the menu's Stop is the other way out.
        stage.emit(Msg::ReloadOrStop);
        rig.events(stage, vec![Event::PageResponsive(false)]);
        rig.events(stage, vec![Event::PageResponsive(true)]);
        rig.events(stage, vec![Event::PageResponsive(false)]);
        stage.emit(Msg::Menu(xui_webplus::chrome::Command::Stop));
    });
    assert_eq!(recovers(&calls), [true, false]);
    assert!(!calls.contains(&Call::Reload), "an ordinary reload is not what a stuck page gets");
}

#[test]
fn an_answer_with_no_stuck_page_does_nothing() {
    let (_, calls) = drive("hang-none", None, |stage, _| {
        stage.emit(Msg::Hang(Answer::Reload));
    });
    assert!(recovers(&calls).is_empty());
}
