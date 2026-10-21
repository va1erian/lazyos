//! The pane (`xui_webd_client::pane`) against the real service half: what an app like Docs or Mail
//! sees while `webd` is late, when a page is drawn, when a link is clicked, when pages change (one
//! view for all of them) and when the view is lost.

mod support;

use support::{world, SharedWorld, TestTransport};
use xui_webd::engine::EngineEvents;
use xui_webd::recording::Call;
use xui_webd_client::pane::{
    Pane, PaneEvent, Status, GIVE_UP_AFTER, NOT_RUNNING_AFTER_MS, RECREATE_AFTER_MS,
    RESIZE_AFTER_MS,
};
use xui_webd_client::LightConfig;

const SIZE: (u32, u32) = (64, 48);

fn pane(world: &SharedWorld) -> Pane<TestTransport> {
    Pane::new(TestTransport::new(world, 7), LightConfig::default())
}

fn paint(world: &SharedWorld, view: u64) {
    world
        .borrow()
        .paint(view, SIZE.0 as i32, SIZE.1 as i32, [255, 255, 255, 255]);
}

fn loads(world: &SharedWorld) -> Vec<String> {
    world
        .borrow()
        .engine
        .calls()
        .into_iter()
        .filter_map(|call| match call {
            Call::LoadHtml { html, .. } => Some(String::from_utf8(html).unwrap()),
            _ => None,
        })
        .collect()
}

fn creates(world: &SharedWorld) -> usize {
    world
        .borrow()
        .engine
        .calls()
        .iter()
        .filter(|call| matches!(call, Call::ViewCreate(..)))
        .count()
}

/// Finish the page in the live view and draw it.
fn finish(world: &SharedWorld, view: u64, finished: i32) {
    world.borrow().queue.state(view, "t", false, finished, "");
    paint(world, view);
}

#[test]
fn webd_late_is_quiet_then_not_running_then_the_page_appears() {
    let world = world();
    let mut pane = pane(&world);
    pane.set_html("<p>one</p>".to_owned());
    world.borrow_mut().down = true;
    assert!(pane.tick(SIZE, 0).is_empty());
    assert!(pane.tick(SIZE, 1000).is_empty(), "not yet 'not running'");
    assert_eq!(*pane.status(), Status::Starting);
    assert_eq!(
        pane.tick(SIZE, NOT_RUNNING_AFTER_MS + 10),
        vec![PaneEvent::Status(Status::NotRunning)]
    );
    // A window with no size yet does nothing.
    assert!(pane.tick((0, 0), 4000).is_empty());

    world.borrow_mut().down = false;
    assert_eq!(pane.tick(SIZE, 4000), vec![PaneEvent::Status(Status::Live)]);
    // The page goes in with the next turn.
    pane.tick(SIZE, 4016);
    assert_eq!(loads(&world), vec!["<p>one</p>".to_owned()]);
}

#[test]
fn a_page_is_painted_once_after_its_load_finishes() {
    let world = world();
    let mut pane = pane(&world);
    pane.set_html("<p>one</p>".to_owned());
    pane.tick(SIZE, 0);
    pane.tick(SIZE, 16);
    let view = pane.view().view_id().unwrap();

    // A frame before the load has finished is not the page.
    paint(&world, view);
    assert!(pane.tick(SIZE, 32).is_empty());
    world.borrow().queue.state(view, "t", false, 1, "");
    assert!(pane.tick(SIZE, 48).is_empty(), "finished, not yet drawn");
    assert!(world.borrow().engine.calls().contains(&Call::Refresh(view)));
    paint(&world, view);
    assert_eq!(pane.tick(SIZE, 64), vec![PaneEvent::Painted]);
    assert!(!pane.picture().is_empty());
    assert_eq!(pane.title(), "t");
    // More frames say nothing.
    paint(&world, view);
    assert!(pane.tick(SIZE, 80).is_empty());
}

#[test]
fn pages_replace_each_other_in_one_view() {
    let world = world();
    let mut pane = pane(&world);
    pane.set_html("<p>one</p>".to_owned());
    pane.tick(SIZE, 0);
    pane.tick(SIZE, 16);
    let view = pane.view().view_id().unwrap();
    finish(&world, view, 1);
    assert_eq!(pane.tick(SIZE, 32), vec![PaneEvent::Painted]);

    // Two pages set between turns are loaded once, the newest.
    pane.set_html("<p>two</p>".to_owned());
    pane.set_html("<p>three</p>".to_owned());
    pane.tick(SIZE, 48);
    assert_eq!(loads(&world), vec!["<p>one</p>", "<p>three</p>"]);
    assert_eq!(creates(&world), 1, "one view for every page");
    assert_eq!(pane.view().view_id(), Some(view));

    // The first finished load of the old page is not the new page's.
    world.borrow().queue.state(view, "t", false, 1, "");
    assert!(pane.tick(SIZE, 64).is_empty());
    finish(&world, view, 2);
    assert_eq!(pane.tick(SIZE, 80), vec![PaneEvent::Painted]);
}

#[test]
fn links_and_copied_text_come_back_to_the_app() {
    let world = world();
    let mut pane = pane(&world);
    pane.tick(SIZE, 0);
    let view = pane.view().view_id().unwrap();
    world.borrow().queue.link_clicked(view, "mailto:a@b.c");
    world.borrow().queue.clipboard_write(view, "some text");
    assert_eq!(
        pane.tick(SIZE, 16),
        vec![
            PaneEvent::Link("mailto:a@b.c".to_owned()),
            PaneEvent::Copy("some text".to_owned()),
        ]
    );
}

#[test]
fn a_lost_view_is_made_again_and_the_newest_page_comes_back() {
    let world = world();
    let mut pane = pane(&world);
    pane.set_html("<p>one</p>".to_owned());
    pane.tick(SIZE, 0);
    pane.tick(SIZE, 16);
    let first = pane.view().view_id().unwrap();
    finish(&world, first, 1);
    assert_eq!(pane.tick(SIZE, 32), vec![PaneEvent::Painted]);

    world.borrow().queue.view_lost(first, 2);
    pane.set_html("<p>two</p>".to_owned());
    assert!(pane.tick(SIZE, 1000).is_empty());
    assert!(
        pane.tick(SIZE, 1000 + RECREATE_AFTER_MS - 1).is_empty(),
        "the new view waits a moment"
    );
    pane.tick(SIZE, 1000 + RECREATE_AFTER_MS);
    let second = pane.view().view_id().unwrap();
    assert_ne!(first, second);
    pane.tick(SIZE, 1600);
    assert_eq!(loads(&world), vec!["<p>one</p>", "<p>two</p>"]);
    finish(&world, second, 1);
    assert_eq!(pane.tick(SIZE, 1700), vec![PaneEvent::Painted]);
}

#[test]
fn a_view_that_keeps_getting_lost_ends_in_failed_until_retried() {
    let world = world();
    let mut pane = pane(&world);
    pane.set_html("<p>one</p>".to_owned());
    let mut now = 0;
    pane.tick(SIZE, now);
    for round in 0..GIVE_UP_AFTER {
        let view = pane.view().view_id().unwrap();
        world.borrow().queue.view_lost(view, 2);
        now += 1000;
        let events = pane.tick(SIZE, now);
        if round + 1 < GIVE_UP_AFTER {
            assert!(events.is_empty());
            now += RECREATE_AFTER_MS;
            pane.tick(SIZE, now);
        } else {
            assert!(matches!(
                events.as_slice(),
                [PaneEvent::Status(Status::Failed(_))]
            ));
        }
    }
    let before = creates(&world);
    assert!(pane.tick(SIZE, now + 60_000).is_empty());
    assert_eq!(creates(&world), before, "no more tries by itself");

    pane.retry();
    pane.tick(SIZE, now + 61_000);
    assert_eq!(creates(&world), before + 1);
    assert_eq!(*pane.status(), Status::Live);
}

#[test]
fn a_service_that_dies_loses_the_view_and_the_pane_waits_for_it_to_return() {
    let world = world();
    let mut pane = pane(&world);
    pane.set_html("<p>one</p>".to_owned());
    pane.tick(SIZE, 0);
    {
        let mut world = world.borrow_mut();
        world.down = true;
        world.fail_recv = Some(-32);
    }
    assert!(pane.tick(SIZE, 100).is_empty());
    // Still down: it keeps trying, and says so after a while.
    assert!(pane.tick(SIZE, 700).is_empty());
    let events = pane.tick(SIZE, 700 + NOT_RUNNING_AFTER_MS + 300);
    assert_eq!(events, vec![PaneEvent::Status(Status::NotRunning)]);
    {
        let mut world = world.borrow_mut();
        world.down = false;
        world.fail_recv = None;
    }
    let mut now = 5000;
    let mut seen = Vec::new();
    for _ in 0..8 {
        now += 300;
        seen.extend(pane.tick(SIZE, now));
    }
    assert!(seen.contains(&PaneEvent::Status(Status::Live)), "{seen:?}");
    // The first load met a dead service; the new view gets the page once.
    assert_eq!(loads(&world), vec!["<p>one</p>"]);
}

#[test]
fn the_window_growing_resizes_the_view() {
    let world = world();
    let mut pane = pane(&world);
    pane.tick(SIZE, 0);
    let view = pane.view().view_id().unwrap();
    pane.tick((128, 96), 16);
    let resized = |world: &SharedWorld| {
        world
            .borrow()
            .engine
            .calls()
            .contains(&Call::Resize(view, 128, 96))
    };
    assert!(!resized(&world), "a new size must hold still first");
    pane.tick((128, 96), 16 + RESIZE_AFTER_MS);
    assert!(resized(&world));
}

#[test]
fn a_size_that_keeps_changing_is_not_followed_until_it_stops() {
    let world = world();
    let mut pane = pane(&world);
    pane.tick(SIZE, 0);
    let view = pane.view().view_id().unwrap();
    for (step, width) in (100..140).step_by(4).enumerate() {
        pane.tick((width, 96), 16 * (step as u64 + 1));
    }
    let resizes = |world: &SharedWorld| {
        world
            .borrow()
            .engine
            .calls()
            .iter()
            .filter(|call| matches!(call, Call::Resize(id, ..) if *id == view))
            .count()
    };
    assert_eq!(resizes(&world), 0, "a drag in progress resizes nothing");
    pane.tick((136, 96), 16 * 20 + RESIZE_AFTER_MS);
    assert_eq!(resizes(&world), 1);
}

#[test]
fn a_service_lost_while_the_view_is_made_is_tried_again_not_given_up() {
    let world = world();
    let mut pane = pane(&world);
    {
        let mut world = world.borrow_mut();
        world.down = true;
    }
    pane.tick(SIZE, 0);
    world.borrow_mut().down = false;
    let events = pane.tick(SIZE, 300);
    assert!(
        events.contains(&PaneEvent::Status(Status::Live)),
        "{events:?}"
    );
}

#[test]
fn a_page_the_service_refuses_leaves_the_old_one_and_is_reported() {
    let world = world();
    let mut pane = pane(&world);
    pane.tick(SIZE, 0);
    // More than the service accepts (8 MiB): `E2BIG`.
    pane.set_html("x".repeat(9 << 20));
    let events = pane.tick(SIZE, 16);
    assert!(
        matches!(events.as_slice(), [PaneEvent::LoadRefused(_)]),
        "{events:?}"
    );
    assert_eq!(*pane.status(), Status::Live);
}

#[test]
fn a_page_still_loading_when_the_next_is_set_does_not_stand_for_it() {
    let world = world();
    let mut pane = pane(&world);
    pane.set_html("<p>one</p>".to_owned());
    pane.tick(SIZE, 0);
    pane.tick(SIZE, 16);
    let view = pane.view().view_id().unwrap();
    // Page one is still loading when the user moves on to page two.
    world.borrow().queue.state(view, "one", true, 0, "");
    pane.tick(SIZE, 32);
    pane.set_html("<p>two</p>".to_owned());
    pane.tick(SIZE, 48);
    // Page one finishes: that is not page two.
    finish(&world, view, 1);
    assert!(pane.tick(SIZE, 64).is_empty());
    // Page two finishes and paints.
    finish(&world, view, 2);
    assert_eq!(pane.tick(SIZE, 80), vec![PaneEvent::Painted]);
}
