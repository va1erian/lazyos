//! The demo client's logic against the real service half: what it prints, in order, when `webd` is
//! late, when the page paints, when a link is clicked and when the view is lost.

mod support;

use support::{world, SharedWorld, TestTransport};
use xui_webd::engine::EngineEvents;
use xui_webd::recording::Call;
use xui_webd_client::demo::{Demo, Marker, RECREATE_AFTER_MS, RETRY_MS};
use xui_webd_client::page::{BLOCKED_IMAGE_URL, LINK_URL, TITLE};

const SIZE: (u32, u32) = (64, 48);

fn texts(markers: &[Marker]) -> Vec<String> {
    markers.iter().map(Marker::text).collect()
}

fn demo(world: &SharedWorld) -> Demo<TestTransport> {
    Demo::new(TestTransport::new(world, 7))
}

fn paint(world: &SharedWorld, view: u64, rgba: [u8; 4]) {
    world
        .borrow()
        .paint(view, SIZE.0 as i32, SIZE.1 as i32, rgba);
}

#[test]
fn nothing_is_said_while_webd_is_not_there_and_the_page_comes_up_when_it_is() {
    let world = world();
    let mut demo = demo(&world);
    world.borrow_mut().down = true;
    assert!(demo.tick(SIZE, 0).is_empty());
    assert!(demo.tick(SIZE, 100).is_empty(), "retries are paced");
    assert!(demo.tick(SIZE, 2000).is_empty());
    // A window with no size yet does nothing.
    assert!(demo.tick((0, 0), 2500).is_empty());

    world.borrow_mut().down = false;
    let markers = demo.tick(SIZE, 3000);
    let id = demo.view().view_id().unwrap();
    assert_eq!(texts(&markers), vec![format!("VIEW:PASS id={id}")]);

    // The page asked for is the demo page, with its picture addressed at the harness's listener.
    let html = world
        .borrow()
        .engine
        .calls()
        .into_iter()
        .find_map(|call| match call {
            Call::LoadHtml { html, base_url, .. } => Some((html, base_url)),
            _ => None,
        })
        .expect("a LoadHtml");
    let page = String::from_utf8(html.0).unwrap();
    assert!(page.contains(BLOCKED_IMAGE_URL) && page.contains(LINK_URL));
    assert_eq!(html.1, "about:blank");
    assert!(RETRY_MS > 0);
}

#[test]
fn the_first_frame_that_is_not_blank_is_the_page_and_the_wait_is_reported() {
    let world = world();
    let mut demo = demo(&world);
    demo.tick(SIZE, 1000);
    let id = demo.view().view_id().unwrap();

    // A blank page first: nothing to report yet.
    paint(&world, id, [255, 255, 255, 255]);
    assert!(demo.tick(SIZE, 1010).is_empty());
    // Then the blue page.
    world.borrow().queue.state(id, TITLE, false, 1, "");
    paint(&world, id, [0, 0, 255, 255]);
    let markers = demo.tick(SIZE, 1042);
    assert_eq!(
        texts(&markers),
        vec![
            format!("STATE:{TITLE}"),
            "FRAME:PASS colour=blue w=64 h=48 seq=2 ms=42".to_owned(),
            "LATENCY:42".to_owned(),
        ]
    );
    // Later frames and unchanged titles say nothing more.
    paint(&world, id, [0, 0, 255, 255]);
    world.borrow().queue.state(id, TITLE, false, 2, "");
    assert!(demo.tick(SIZE, 1100).is_empty());
}

#[test]
fn a_click_on_a_link_is_reported_and_nothing_else_happens() {
    let world = world();
    let mut demo = demo(&world);
    demo.tick(SIZE, 0);
    let id = demo.view().view_id().unwrap();
    world.borrow().queue.link_clicked(id, LINK_URL);
    assert_eq!(
        texts(&demo.tick(SIZE, 10)),
        vec![format!("LINK:{LINK_URL}")]
    );
}

#[test]
fn a_lost_view_is_made_again_once_and_a_second_loss_stays_lost() {
    let world = world();
    let mut demo = demo(&world);
    demo.tick(SIZE, 0);
    let first = demo.view().view_id().unwrap();
    paint(&world, first, [0, 0, 255, 255]);
    demo.tick(SIZE, 20);

    world.borrow().queue.view_lost(first, 2);
    assert_eq!(
        texts(&demo.tick(SIZE, 1000)),
        vec!["LOST:crashed".to_owned()]
    );
    assert!(
        demo.tick(SIZE, 1000 + RECREATE_AFTER_MS - 1).is_empty(),
        "the new view waits a moment"
    );
    let markers = demo.tick(SIZE, 1000 + RECREATE_AFTER_MS);
    let second = demo.view().view_id().unwrap();
    assert_ne!(first, second);
    assert_eq!(texts(&markers), vec![format!("VIEW:PASS id={second}")]);

    paint(&world, second, [0, 0, 255, 255]);
    assert_eq!(
        texts(&demo.tick(SIZE, 2000)),
        vec!["RECREATE:PASS".to_owned()]
    );

    // Lost again: reported, and not made again by itself.
    world.borrow().queue.view_lost(second, 1);
    assert_eq!(
        texts(&demo.tick(SIZE, 3000)),
        vec!["LOST:refused".to_owned()]
    );
    assert!(demo.tick(SIZE, 9000).is_empty());
    // The R key does.
    demo.recreate_now();
    let markers = demo.tick(SIZE, 9001);
    assert_eq!(markers.len(), 1);
    assert!(matches!(markers[0], Marker::View { .. }));
}

#[test]
fn a_service_that_dies_loses_the_view_and_the_demo_waits_for_it_to_return() {
    let world = world();
    let mut demo = demo(&world);
    demo.tick(SIZE, 0);
    {
        let mut world = world.borrow_mut();
        world.down = true;
        world.fail_recv = Some(-32);
    }
    assert_eq!(
        texts(&demo.tick(SIZE, 100)),
        vec!["LOST:service_gone".to_owned()]
    );
    // Still down: no marker, tries again each half second.
    assert!(demo.tick(SIZE, 700).is_empty());
    assert!(demo.tick(SIZE, 1300).is_empty());
    {
        let mut world = world.borrow_mut();
        world.down = false;
        world.fail_recv = None;
    }
    let markers = demo.tick(SIZE, 1900);
    assert!(matches!(markers.as_slice(), [Marker::View { .. }]));
    let id = demo.view().view_id().unwrap();
    paint(&world, id, [0, 0, 255, 255]);
    assert_eq!(
        texts(&demo.tick(SIZE, 2000)),
        vec!["RECREATE:PASS".to_owned()]
    );
}

#[test]
fn the_window_growing_resizes_the_view() {
    let world = world();
    let mut demo = demo(&world);
    demo.tick(SIZE, 0);
    let id = demo.view().view_id().unwrap();
    demo.tick((128, 96), 10);
    assert!(world
        .borrow()
        .engine
        .calls()
        .contains(&Call::Resize(id, 128, 96)));
    assert_eq!(demo.view().size(), (128, 96));
}

#[test]
fn marker_lines_have_the_documented_shapes() {
    use xui_webd_client::Colour;
    assert_eq!(
        Marker::Frame {
            colour: Colour::Red,
            width: 1,
            height: 2,
            seq: 3,
            ms: 4
        }
        .text(),
        "FRAME:PASS colour=red w=1 h=2 seq=3 ms=4"
    );
    assert_eq!(Marker::Latency { ms: 9 }.text(), "LATENCY:9");
    assert_eq!(
        Marker::Failed {
            what: "CREATE",
            why: "refused (errno 28)".into()
        }
        .text(),
        "CREATE:FAIL:refused (errno 28)"
    );
}
