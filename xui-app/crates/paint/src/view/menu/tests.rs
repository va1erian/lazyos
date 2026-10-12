//! The menu table: ids, the id -> message map, the plan's items and their keys.

use std::collections::HashSet;

use super::*;
use crate::view::shortcuts::{BINDINGS, Chord};

/// The label text of every row, with its message.
fn rows() -> Vec<(usize, Msg, &'static str)> {
    let mut all = Vec::new();
    for (menu, top) in MENUS.iter().enumerate() {
        for row in top.rows {
            if let Row::Item(msg, text) = row {
                all.push((menu, *msg, *text));
            }
        }
    }
    all
}

#[test]
fn the_bar_has_the_seven_v1_menus_in_order() {
    let titles: Vec<&str> = MENUS.iter().map(|top| top.title).collect();
    assert_eq!(
        titles,
        [
            "&File",
            "&Edit",
            "&View",
            "&Image",
            "&Layers",
            "&Adjustments",
            "&Help"
        ]
    );
}

#[test]
fn ids_are_unique_and_map_back_to_their_message() {
    let mut seen = HashSet::new();
    for (id, msg) in items() {
        assert!(seen.insert(id), "{id:?} is used twice");
        assert_eq!(message(id), Some(msg));
        assert_eq!(id_of(msg), Some(id), "{msg:?} has one row");
    }
    for menu in 0..MENUS.len() {
        assert_eq!(message(title_id(menu)), None, "a title raises nothing");
    }
    assert_eq!(message(MenuId::new(99_999)), None);
}

#[test]
fn every_message_appears_in_one_row_only() {
    let mut seen = HashSet::new();
    for (_, msg, text) in rows() {
        assert!(seen.insert(format!("{msg:?}")), "{text} repeats {msg:?}");
    }
}

#[test]
fn every_menu_command_of_the_plan_is_present() {
    // Tools are chosen by key and the tool grid, not from a menu.
    let tools = [
        Command::ZoomTool,
        Command::PanTool,
        Command::GradientTool,
        Command::TextTool,
    ];
    for command in Command::ALL {
        let in_menu = id_of(Msg::Command(command)).is_some();
        assert_eq!(
            in_menu,
            !tools.contains(&command),
            "{} should{} have a row",
            command.label(),
            if tools.contains(&command) { " not" } else { "" }
        );
    }
    for msg in [
        Msg::New,
        Msg::Open,
        Msg::Save,
        Msg::SaveAs,
        Msg::Quit,
        Msg::Undo,
        Msg::Redo,
        Msg::ResizeAsk,
        Msg::About,
    ] {
        assert!(id_of(msg).is_some(), "{msg:?} has a row");
    }
}

#[test]
fn mnemonics_are_unique_within_a_menu() {
    for (menu, top) in MENUS.iter().enumerate() {
        let mut letters = HashSet::new();
        for (_, _, text) in rows().into_iter().filter(|(owner, _, _)| *owner == menu) {
            let letter = text
                .split('&')
                .nth(1)
                .and_then(|rest| rest.chars().next())
                .unwrap_or_else(|| panic!("{text} has no mnemonic"))
                .to_ascii_lowercase();
            assert!(
                letters.insert(letter),
                "{} reuses {letter} in {text}",
                top.title
            );
        }
    }
    let bar: HashSet<char> = MENUS
        .iter()
        .map(|top| {
            top.title
                .split('&')
                .nth(1)
                .and_then(|rest| rest.chars().next())
                .expect("a mnemonic")
        })
        .collect();
    assert_eq!(bar.len(), MENUS.len(), "the bar's mnemonics are distinct");
}

#[test]
fn a_row_prints_the_key_that_does_its_work() {
    assert_eq!(label(Msg::Save, "&Save"), "&Save    Ctrl+S");
    assert_eq!(
        label(Msg::SaveAs, "Save &As..."),
        "Save &As...    Ctrl+Shift+S"
    );
    assert_eq!(label(Msg::New, "&New"), "&New    Ctrl+N");
    assert_eq!(label(Msg::About, "&About Paint"), "&About Paint");
    // Every chord in a menu is a real binding for that row's message.
    for (_, msg, _) in rows() {
        if let Some(chord) = chord_for(msg) {
            assert!(
                BINDINGS
                    .iter()
                    .any(|(bound, m)| *bound == chord && *m == msg),
                "{msg:?}"
            );
            let _: Chord = chord;
        }
    }
}

#[test]
fn unbuilt_features_start_disabled_and_the_rest_enabled() {
    let none = Enabled::default();
    for command in Command::ALL {
        assert!(
            is_enabled(
                Msg::Command(command),
                Enabled {
                    io: true,
                    can_undo: true,
                    can_redo: true,
                    selection: true,
                    can_paste: true,
                    layers: 3,
                    active: 1,
                }
            ) == command.available(),
            "{} follows its availability",
            command.label()
        );
    }
    assert!(!is_enabled(Msg::Undo, none));
    assert!(!is_enabled(Msg::Redo, none));
    assert!(is_enabled(
        Msg::Undo,
        Enabled {
            can_undo: true,
            ..none
        }
    ));
    // Rows that act on a selection or a picture to paste wait for one.
    let cut = Msg::Command(Command::Cut);
    let paste = Msg::Command(Command::Paste);
    assert!(!is_enabled(cut, none) && !is_enabled(paste, none));
    assert!(is_enabled(
        cut,
        Enabled {
            selection: true,
            ..none
        }
    ));
    assert!(!is_enabled(
        paste,
        Enabled {
            selection: true,
            ..none
        }
    ));
    assert!(is_enabled(
        paste,
        Enabled {
            can_paste: true,
            ..none
        }
    ));
    assert!(is_enabled(Msg::Command(Command::SelectAll), none));
    assert!(!is_enabled(Msg::Open, none));
    assert!(is_enabled(Msg::Open, Enabled { io: true, ..none }));
    for msg in [Msg::New, Msg::Quit, Msg::ResizeAsk, Msg::Clear, Msg::About] {
        assert!(is_enabled(msg, none), "{msg:?} always works");
    }
}

#[test]
fn layer_rows_follow_the_stack() {
    let at = |layers, active| Enabled {
        layers,
        active,
        ..Enabled::default()
    };
    let on = |command, state| is_enabled(Msg::Command(command), state);
    // A lone layer cannot be deleted, merged, moved or flattened.
    let lone = at(1, 0);
    for command in [
        Command::DeleteLayer,
        Command::MergeLayerDown,
        Command::MoveLayerUp,
        Command::MoveLayerDown,
        Command::Flatten,
    ] {
        assert!(!on(command, lone), "{} needs more layers", command.label());
    }
    for command in [
        Command::AddLayer,
        Command::DuplicateLayer,
        Command::LayerProperties,
        Command::FlipLayerHorizontal,
        Command::FlipVertical,
        Command::Rotate180,
    ] {
        assert!(on(command, lone), "{} works on one layer", command.label());
    }
    // The bottom layer cannot go down or merge; the top cannot go up.
    let bottom = at(3, 0);
    assert!(!on(Command::MergeLayerDown, bottom) && !on(Command::MoveLayerDown, bottom));
    assert!(on(Command::MoveLayerUp, bottom) && on(Command::DeleteLayer, bottom));
    let top = at(3, 2);
    assert!(!on(Command::MoveLayerUp, top));
    assert!(on(Command::MergeLayerDown, top) && on(Command::Flatten, top));
}
