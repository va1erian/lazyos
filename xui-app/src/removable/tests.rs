use super::*;

fn drive(disk: &str, name: &str, phase: Phase) -> Drive {
    Drive {
        disk: disk.into(),
        name: name.into(),
        path: if name.is_empty() {
            String::new()
        } else {
            format!("/media/{name}")
        },
        phase,
        detail: String::new(),
    }
}

#[test]
fn what_was_there_at_the_start_is_silent_until_it_leaves_badly() {
    let mut model = Model::new();
    assert_eq!(model.poll(vec![drive("usb1", "", Phase::Starting)]), None);
    // It mounts a moment later: not news, it was plugged in at login.
    assert_eq!(
        model.poll(vec![drive("usb1", "PHOTOS", Phase::Mounted)]),
        None
    );
    assert_eq!(model.status(), wire::STATUS_ACTIVE);
    assert_eq!(model.tooltip(), "PHOTOS at /media/PHOTOS");
    // Pulled without ejecting: told, though it was in the baseline.
    let notice = model.poll(vec![]).unwrap();
    assert!(
        notice.contains("PHOTOS was removed without ejecting"),
        "{notice}"
    );
    assert_eq!(model.status(), wire::STATUS_ATTENTION);
    assert_eq!(model.tooltip(), notice);
}

#[test]
fn a_stick_that_arrives_is_announced_once_and_the_attention_fades() {
    let mut model = Model::new();
    model.poll(vec![]);
    assert_eq!(model.poll(vec![drive("usb1", "", Phase::Starting)]), None);
    let notice = model.poll(vec![drive("usb1", "MY_STICK", Phase::Mounted)]);
    assert_eq!(
        notice.as_deref(),
        Some("MY_STICK mounted at /media/MY_STICK")
    );
    assert_eq!(model.badge(), "1");
    // The same list again says nothing, and the attention runs out.
    for _ in 0..ATTENTION_POLLS {
        assert_eq!(
            model.poll(vec![drive("usb1", "MY_STICK", Phase::Mounted)]),
            None
        );
    }
    assert_eq!(model.status(), wire::STATUS_ACTIVE);
    assert_eq!(model.tooltip(), "MY_STICK at /media/MY_STICK");
}

#[test]
fn an_eject_is_told_when_the_stick_may_be_pulled_and_its_removal_is_mild() {
    let mut model = Model::new();
    model.poll(vec![]);
    model.poll(vec![drive("usb1", "A", Phase::Mounted)]);
    model.poll(vec![drive("usb1", "A", Phase::Ejecting)]);
    assert_eq!(model.tooltip(), "Ejecting A...");
    let rows = model.menu();
    assert!(rows
        .iter()
        .any(|r| r.label == "Ejecting A..." && !r.enabled));
    let notice = model
        .poll(vec![drive("usb1", "A", Phase::Ejected)])
        .unwrap();
    assert_eq!(notice, "A can be removed");
    let notice = model.poll(vec![]).unwrap();
    assert_eq!(notice, "A removed");
    assert!(!notice.contains("data may be lost"));
}

#[test]
fn a_plug_that_comes_back_is_news_again() {
    let mut model = Model::new();
    model.poll(vec![drive("usb1", "A", Phase::Mounted)]);
    // At login: silent. Pulled, then plugged in again: announced.
    model.poll(vec![]);
    model.poll(vec![drive("usb1", "", Phase::Starting)]);
    let notice = model.poll(vec![drive("usb1", "A", Phase::Mounted)]);
    assert_eq!(notice.as_deref(), Some("A mounted at /media/A"));
}

#[test]
fn several_changes_of_one_look_are_one_notice() {
    let mut model = Model::new();
    model.poll(vec![]);
    let notice = model
        .poll(vec![
            drive("usb1", "A", Phase::Mounted),
            drive("usb2", "B", Phase::Mounted),
        ])
        .unwrap();
    assert_eq!(notice, "A mounted at /media/A; B mounted at /media/B");
    assert_eq!(model.badge(), "2");
    assert_eq!(model.tooltip(), notice);
}

#[test]
fn a_stick_nothing_can_mount_and_a_failure_say_so() {
    let mut model = Model::new();
    model.poll(vec![]);
    let notice = model
        .poll(vec![drive("usb3", "", Phase::Unsupported)])
        .unwrap();
    assert!(notice.starts_with("usb3: no volume"), "{notice}");
    let mut failed = drive("usb4", "X", Phase::Failed);
    failed.detail = String::from("the volume cannot be read");
    let notice = model.poll(vec![failed]).unwrap();
    assert_eq!(notice, "X: the volume cannot be read");
}

#[test]
fn the_menu_has_open_and_eject_per_drive_and_actions_resolve() {
    let mut model = Model::new();
    model.poll(vec![]);
    model.poll(vec![
        drive("usb1", "A", Phase::Mounted),
        drive("usb2", "B", Phase::Mounted),
    ]);
    let labels: Vec<String> = model.menu().into_iter().map(|r| r.label).collect();
    assert_eq!(
        labels,
        ["Open A", "Eject A", "Open B", "Eject B", "", "Eject all"]
    );
    assert!(model.menu()[0].is_default);
    assert_eq!(model.action(100), Some(Action::Open("/media/A".into())));
    assert_eq!(model.action(201), Some(Action::Eject("usb2".into())));
    assert_eq!(
        model.action(300),
        Some(Action::EjectAll(vec!["usb1".into(), "usb2".into()]))
    );
    // A row of a drive that has gone (a stale menu) does nothing.
    assert_eq!(model.action(102), None);
    assert_eq!(model.action(7), None);
    assert_eq!(
        model.default_action(),
        Some(Action::Open("/media/A".into()))
    );
}

#[test]
fn nothing_attached_shows_a_passive_item_with_one_dead_row() {
    let mut model = Model::new();
    model.poll(vec![]);
    assert_eq!(model.tooltip(), "No removable drives");
    assert_eq!(model.status(), wire::STATUS_PASSIVE);
    assert_eq!(model.badge(), "");
    let rows = model.menu();
    assert_eq!(rows.len(), 1);
    assert!(!rows[0].enabled);
    assert_eq!(model.default_action(), None);
}

#[test]
fn a_refusal_names_the_holder_and_expires() {
    let mut model = Model::new();
    model.poll(vec![]);
    model.poll(vec![drive("usb1", "A", Phase::Mounted)]);
    model.refused(
        "A",
        "sh (19): working directory /media/A/x\nvim (22): /media/A/y\n",
    );
    let tooltip = model.tooltip();
    assert_eq!(
        tooltip,
        "Cannot eject A: sh (19): working directory /media/A/x (+1 more)"
    );
    // The menu row says who, short enough for the menu's width.
    assert!(model
        .menu()
        .iter()
        .any(|r| r.label == "In use: sh (19) (+1 more)" && !r.enabled));
    for _ in 0..REFUSAL_POLLS {
        model.poll(vec![drive("usb1", "A", Phase::Mounted)]);
    }
    assert!(!model
        .menu()
        .iter()
        .any(|r| r.label.starts_with("In use")));
}

#[test]
fn long_text_is_clipped_to_a_row() {
    let long = "x".repeat(500);
    let mut model = Model::new();
    model.poll(vec![]);
    model.refused("A", &long);
    assert!(model
        .menu()
        .iter()
        .all(|r| r.label.chars().count() <= MAX_ROW));
}

#[test]
fn the_mask_is_a_tilted_stick() {
    let side = MASK_SIDE as usize;
    let mask = mask();
    assert_eq!(mask.len(), side * side * 4);
    let alpha = |x: usize, y: usize| mask[(y * side + x) * 4 + 3];
    // Symmetric about the stick's own axis, the corners empty, the middle
    // solid, and a sensible amount of ink (not a block, not a speck).
    for y in 0..side {
        for x in 0..side {
            assert_eq!(alpha(x, y), alpha(side - 1 - y, side - 1 - x), "({x},{y})");
        }
    }
    assert_eq!(alpha(0, 0), 0);
    assert_eq!(alpha(side - 1, side - 1), 0);
    assert_eq!(alpha(0, side - 1), 0);
    assert_eq!(alpha(side - 1, 0), 0);
    assert_eq!(alpha(15, 17), 255);
    let ink = mask.chunks(4).filter(|px| px[3] > 0).count();
    assert!((side * side / 5..side * side / 2).contains(&ink), "{ink} px");
}
