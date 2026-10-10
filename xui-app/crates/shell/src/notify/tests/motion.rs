//! Placement and motion: the stack, the spring, reflow, reduced motion and
//! the progress fill.

use super::*;

#[test]
fn the_stack_sits_above_the_bar_newest_nearest_it() {
    let mut center = Center::new();
    let a = center.post("a", req(Level::Warn, "a"), 0).unwrap();
    let b = center.post("b", req(Level::Warn, "b"), 0).unwrap();
    // Unmeasured toasts take no space and are not drawn.
    center.layout(BAR_TOP, 0);
    assert!(center.frames(1280, 0).is_empty());
    measured(&mut center, 0);
    // Long after the slide: at rest.
    let frames = center.frames(1280, 1000);
    let by_id = |id| frames.iter().find(|f| f.id == id).unwrap();
    let (fa, fb) = (by_id(a), by_id(b));
    assert_eq!(fb.rect.y + fb.rect.h, BAR_TOP - BAR_GAP);
    assert_eq!(fa.rect.y + fa.rect.h + STACK_GAP, fb.rect.y);
    assert_eq!(fb.rect.x + fb.rect.w, 1280 - MARGIN);
    assert_eq!(fb.rect.w, card::WIDTH);
    assert_eq!(fb.stage, Stage::Shown);
    assert!(!fb.moving);
}

#[test]
fn a_toast_springs_in_from_below_and_overshoots_its_place() {
    let mut center = Center::new();
    let id = center.post("a", req(Level::Warn, "a"), 0).unwrap();
    measured(&mut center, 0);
    let y_at = |c: &Center, tick| {
        c.frames(1280, tick)
            .iter()
            .find(|f| f.id == id)
            .unwrap()
            .rect
            .y
    };
    let rest = BAR_TOP - BAR_GAP - 48;
    assert_eq!(y_at(&center, 0), rest + SLIDE_PX);
    assert_eq!(y_at(&center, 1000), rest);
    let path: Vec<i32> = (0..40).map(|tick| y_at(&center, tick)).collect();
    // It overshoots (goes above its place) and then comes back.
    assert!(path.iter().any(|y| *y < rest), "{path:?}");
    assert!(path.iter().all(|y| *y >= rest - 6), "{path:?}");
    let frame = center.frames(1280, 5).pop().unwrap();
    assert_eq!(frame.stage, Stage::Entering);
    assert!(frame.moving);
    assert!(center.animating(5));
}

#[test]
fn the_others_reflow_when_one_leaves() {
    let mut center = Center::new();
    let a = center.post("a", req(Level::Warn, "a"), 0).unwrap();
    let b = center.post("a", req(Level::Warn, "b"), 0).unwrap();
    measured(&mut center, 0);
    let y_of = |c: &Center, id, tick| {
        c.frames(1280, tick)
            .iter()
            .find(|f| f.id == id)
            .map(|f| f.rect.y)
    };
    let before = y_of(&center, a, 1000).unwrap();
    center.dismiss(b, 1000);
    center.layout(BAR_TOP, 1000);
    // It moves down over time, with no jump on the first tick.
    assert_eq!(y_of(&center, a, 1000), Some(before));
    let mid = y_of(&center, a, 1010).unwrap();
    let end = y_of(&center, a, 1100).unwrap();
    assert!(before < mid && mid < end, "{before} {mid} {end}");
    assert_eq!(end, BAR_TOP - BAR_GAP - 48);
    // The one that left sinks out and is then forgotten.
    assert!(y_of(&center, b, 1005).is_some());
    center.tick(1100);
    assert!(y_of(&center, b, 1100).is_none());
    assert!(!center.animating(1200 + 100));
}

#[test]
fn reduced_motion_fades_in_place_and_none_is_instant() {
    for (motion, expect_fade) in [(Motion::Reduced, true), (Motion::None, false)] {
        let mut center = Center::new();
        center.set_motion(motion);
        let id = center.post("a", req(Level::Warn, "a"), 0).unwrap();
        measured(&mut center, 0);
        let rest = BAR_TOP - BAR_GAP - 48;
        let first = center.frames(1280, 0).pop().unwrap();
        // The same layout, with nothing travelling.
        assert_eq!(first.rect.y, rest);
        assert!(!first.moving);
        assert_eq!(first.fade < 1.0, expect_fade);
        let later = center.frames(1280, 30).pop().unwrap();
        assert_eq!(later.fade, 1.0);
        center.dismiss(id, 40);
        if motion == Motion::None {
            assert!(center.is_idle());
        } else {
            let gone = center.frames(1280, 41).pop().unwrap();
            assert_eq!(gone.stage, Stage::Leaving);
            assert_eq!(gone.rect.y, rest);
            assert!(gone.fade < 1.0);
        }
    }
}

#[test]
fn progress_fills_smoothly_between_updates() {
    let mut center = Center::new();
    let mut r = req(Level::Info, "Copying");
    r.note.progress = Some(0);
    let id = center.post("a", r, 0).unwrap();
    measured(&mut center, 0);
    let fill = |c: &Center, tick| {
        c.frames(1280, tick)
            .iter()
            .find(|f| f.id == id)
            .and_then(|f| f.progress)
            .unwrap()
    };
    assert_eq!(fill(&center, 0), 0.0);
    let mut update = req(Level::Info, "Copying");
    update.note.progress = Some(80);
    update.replaces = Some(id);
    center.post("a", update, 100).unwrap();
    assert_eq!(fill(&center, 100), 0.0);
    let mid = fill(&center, 112);
    assert!(mid > 0.1 && mid < 0.8, "{mid}");
    assert!((fill(&center, 200) - 0.8).abs() < 1e-3);
}
