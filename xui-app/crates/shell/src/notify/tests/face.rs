//! A toast's face: the card geometry and hit test, and the icon flourishes.

use super::*;

#[test]
fn the_card_wraps_text_and_stacks_progress_and_actions() {
    let mut note = Note::new(
        Level::Error,
        "Backup failed on the external disk",
        "The disk was removed before the copy finished, so nothing was written to it.",
    )
    .with_actions(&[(1, "Retry"), (2, "Details")]);
    note.progress = Some(40);
    let card = Card::new(&note, "Files", &mono);
    assert_eq!(card.width, card::WIDTH);
    assert!(card.title.len() <= card::TITLE_LINES);
    assert!(card.body.len() <= card::BODY_LINES);
    for line in card.title.iter().chain(card.body.iter()) {
        assert!(mono(&line.text, false) <= card.text_w, "{line:?}");
    }
    // Top to bottom: name, title, body, progress, actions, then padding.
    let last_text = card.body.last().unwrap().y + card::LINE_H;
    let bar = card.progress.unwrap();
    assert!(bar.y >= last_text);
    let button = &card.actions[0];
    assert!(button.rect.y >= bar.y + bar.h);
    assert_eq!(card.height, button.rect.y + card::ACTION_H + card::PAD);
    // Buttons are right aligned, in order, not overlapping.
    assert_eq!(card.actions[0].label, "Retry");
    let (a, b) = (&card.actions[0].rect, &card.actions[1].rect);
    assert!(a.x + a.w < b.x);
    assert_eq!(b.x + b.w, card::WIDTH - card::PAD);
}

#[test]
fn long_text_is_cut_with_an_ellipsis() {
    let note = Note::new(Level::Info, &"word ".repeat(20), &"text ".repeat(100));
    let card = Card::new(
        &note,
        "A very long application name that cannot fit the header line",
        &mono,
    );
    assert_eq!(card.title.len(), card::TITLE_LINES);
    assert!(card.title.last().unwrap().text.ends_with("..."));
    assert_eq!(card.body.len(), card::BODY_LINES);
    assert!(card.body.last().unwrap().text.ends_with("..."));
    assert!(card.name.text.ends_with("..."));
    assert!(mono(&card.name.text, true) <= card.text_w);
}

#[test]
fn a_small_toast_is_as_tall_as_its_icon_and_padding() {
    let note = Note::new(Level::Info, "Hi", "");
    let card = Card::new(&note, "A", &mono);
    assert!(card.body.is_empty() && card.progress.is_none() && card.actions.is_empty());
    assert!(card.height >= card::ICON + 2 * card::PAD);
}

#[test]
fn hits_pick_the_close_mark_the_actions_or_the_body() {
    let note = Note::new(Level::Warn, "Title", "body").with_actions(&[(7, "Open")]);
    let card = Card::new(&note, "App", &mono);
    let close = card.close;
    assert_eq!(card.hit(close.x + 1, close.y + 1), Some(Hit::Close));
    let button = card.actions[0].rect;
    assert_eq!(card.hit(button.x + 2, button.y + 2), Some(Hit::Action(7)));
    assert_eq!(card.hit(card.text_x, card::PAD + 20), Some(Hit::Body));
    assert_eq!(card.hit(-1, 0), None);
    assert_eq!(card.hit(card.width, 0), None);
    assert_eq!(card.hit(0, card.height), None);
}

#[test]
fn wrap_never_exceeds_the_width_and_splits_long_words() {
    let lines = card::wrap("the quick brown fox jumps over the lazy dog", 70, &|t| {
        mono(t, false)
    });
    assert!(
        lines.iter().all(|line| mono(line, false) <= 70),
        "{lines:?}"
    );
    assert_eq!(
        lines.join(" "),
        "the quick brown fox jumps over the lazy dog"
    );
    let long = card::wrap("supercalifragilistic", 35, &|t| mono(t, false));
    assert_eq!(long, ["super", "calif", "ragil", "istic"]);
    assert_eq!(card::wrap("", 70, &|t| mono(t, false)), [""]);
}

// ---- the flourishes ----

#[test]
fn flourishes_are_zero_at_rest_and_bounded_while_playing() {
    let end = FLOURISH_DELAY_MS + FLOURISH_MS;
    for level in [Level::Info, Level::Success, Level::Warn, Level::Error] {
        for age in (0..end + 200).step_by(5) {
            let f = flourish(level, age);
            assert!(f.dx.abs() <= 3.01 && f.dy.abs() <= 3.01, "{level:?} {age}");
            assert!((0.0..=1.0).contains(&f.tick));
        }
        assert_eq!(flourish(level, end), Flourish::REST);
        assert_eq!(flourish(level, 100_000), Flourish::REST);
    }
}

#[test]
fn the_success_tick_draws_itself_and_the_others_are_complete() {
    assert_eq!(flourish(Level::Success, 0).tick, 0.0);
    let mid = flourish(Level::Success, FLOURISH_DELAY_MS + 100).tick;
    assert!(mid > 0.3 && mid < 1.0, "{mid}");
    assert_eq!(flourish(Level::Success, FLOURISH_DELAY_MS + 400).tick, 1.0);
    // Info does not draw a tick, and Warn nudges up, never down.
    assert_eq!(flourish(Level::Info, 0).tick, 1.0);
    for age in (FLOURISH_DELAY_MS..FLOURISH_DELAY_MS + FLOURISH_MS).step_by(10) {
        assert!(flourish(Level::Warn, age).dy <= 0.0);
    }
    // Info wiggles sideways, Error shakes, and both do move.
    let moves = |level| {
        (FLOURISH_DELAY_MS..FLOURISH_DELAY_MS + FLOURISH_MS)
            .step_by(5)
            .any(|age| flourish(level, age).dx.abs() > 0.5)
    };
    assert!(moves(Level::Info) && moves(Level::Error));
}

#[test]
fn a_frames_look_changes_only_while_something_moves() {
    let mut center = Center::new();
    let id = center.post("a", req(Level::Info, "a"), 0).unwrap();
    measured(&mut center, 0);
    let look = |tick| {
        center
            .frames(1280, tick)
            .into_iter()
            .find(|f| f.id == id)
            .unwrap()
            .look()
    };
    assert_ne!(look(3), look(10));
    // After the flourish nothing changes: no redraw is needed.
    assert_eq!(look(100), look(200));
    assert_eq!(look(100).flourish, None);
}
