//! Validation: every field of a `Notify` is bounded, a broken icon falls back,
//! lifetimes follow the level, progress and timeout.

use super::*;

#[test]
fn a_good_request_is_kept_with_control_characters_as_spaces() {
    let mut wire = args(wire::LEVEL_WARN, "Disk\tfull");
    wire.body = "line one\r\nline\ttwo\u{7}".into();
    wire.progress = Some(40);
    wire.timeout = Some(3000);
    wire.replaces_id = Some(7);
    wire.actions = vec![wire::Action {
        id: 1,
        label: "Open".into(),
    }];
    let request = Request::from_wire(wire).expect("valid");
    assert_eq!(request.note.level, Level::Warn);
    assert_eq!(request.note.title, "Disk full");
    // The body keeps its line break; the other control characters are spaces.
    assert_eq!(request.note.body, "line one\nline two ");
    assert_eq!(request.note.progress, Some(40));
    assert_eq!(request.note.timeout, Timeout::Millis(3000));
    assert_eq!(request.replaces, Some(7));
    assert_eq!(request.note.actions.len(), 1);
}

#[test]
fn every_field_is_bounded() {
    let bad = |edit: &dyn Fn(&mut wire::NotifyArgs)| {
        let mut wire = args(wire::LEVEL_INFO, "ok");
        edit(&mut wire);
        Request::from_wire(wire).unwrap_err()
    };
    assert_eq!(bad(&|w| w.level = 9), Invalid::Level);
    assert_eq!(bad(&|w| w.title.clear()), Invalid::Title);
    assert_eq!(bad(&|w| w.title = "   ".into()), Invalid::Title);
    assert_eq!(
        bad(&|w| w.title = "x".repeat(TITLE_MAX + 1)),
        Invalid::Title
    );
    assert_eq!(bad(&|w| w.body = "x".repeat(BODY_MAX + 1)), Invalid::Body);
    assert_eq!(bad(&|w| w.progress = Some(101)), Invalid::Progress);
    let action = |id: u32, label: &str| wire::Action {
        id,
        label: label.into(),
    };
    assert_eq!(
        bad(&|w| w.actions = (1..=4).map(|i| action(i, "a")).collect()),
        Invalid::ActionCount
    );
    assert_eq!(
        bad(&|w| w.actions = vec![action(0, "a")]),
        Invalid::ActionId
    );
    assert_eq!(
        bad(&|w| w.actions = vec![action(1, "a"), action(1, "b")]),
        Invalid::ActionId
    );
    assert_eq!(
        bad(&|w| w.actions = vec![action(1, " ")]),
        Invalid::ActionLabel
    );
    assert_eq!(
        bad(&|w| w.actions = vec![action(1, &"x".repeat(25))]),
        Invalid::ActionLabel
    );
    // The limits themselves are fine.
    let mut edge = args(wire::LEVEL_ERROR, &"t".repeat(TITLE_MAX));
    edge.body = "b".repeat(BODY_MAX);
    edge.progress = Some(100);
    edge.actions = (1..=3).map(|i| action(i, &"l".repeat(24))).collect();
    assert!(Request::from_wire(edge).is_ok());
}

#[test]
fn a_broken_icon_falls_back_instead_of_refusing() {
    let icon = |lucide: Option<&str>, image: Option<wire::Image>| {
        let mut wire = args(wire::LEVEL_INFO, "x");
        wire.icon = Some(wire::Icon {
            lucide: lucide.map(str::to_owned),
            image,
        });
        Request::from_wire(wire).expect("still valid").note.icon
    };
    let image = |side: u32, len: usize| wire::Image {
        width: side,
        height: side,
        data: vec![0; len],
    };
    assert_eq!(icon(Some("usb"), None), Some(Source::Lucide("usb".into())));
    assert_eq!(icon(Some("Not A Name!"), None), None);
    assert_eq!(icon(Some(""), None), None);
    assert!(icon(None, Some(image(16, 16 * 16 * 4))).is_some());
    assert_eq!(icon(None, Some(image(65, 65 * 65 * 4))), None);
    assert_eq!(icon(None, Some(image(16, 7))), None);
    assert_eq!(icon(None, Some(image(0, 0))), None);
    // Two sources are no source.
    assert_eq!(icon(Some("usb"), Some(image(2, 16))), None);
    assert_eq!(icon(None, None), None);
}

#[test]
fn lifetimes_follow_the_level_progress_and_timeout() {
    let note = |level, progress, timeout| Note {
        level,
        title: "t".into(),
        progress,
        timeout,
        ..Note::default()
    };
    let def = Timeout::Default;
    assert_eq!(note(Level::Info, None, def).lifetime_ms(), Some(5000));
    assert_eq!(note(Level::Success, None, def).lifetime_ms(), Some(5000));
    // Missed toasts are gone, so the serious ones wait for the user.
    assert_eq!(note(Level::Warn, None, def).lifetime_ms(), None);
    assert_eq!(
        note(Level::Error, None, Timeout::Millis(2000)).lifetime_ms(),
        None
    );
    assert_eq!(note(Level::Info, None, Timeout::Sticky).lifetime_ms(), None);
    assert_eq!(
        note(Level::Info, None, Timeout::Millis(10)).lifetime_ms(),
        Some(1000)
    );
    assert_eq!(
        note(Level::Info, None, Timeout::Millis(u32::MAX)).lifetime_ms(),
        Some(60_000)
    );
    // Progress holds the toast until it is done, then the timeout runs.
    assert_eq!(note(Level::Info, Some(99), def).lifetime_ms(), None);
    assert_eq!(note(Level::Info, Some(100), def).lifetime_ms(), Some(5000));
}
