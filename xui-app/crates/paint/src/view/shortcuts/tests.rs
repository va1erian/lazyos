//! The shortcut table: coverage of the plan's keys, no collisions, nothing
//! while a dialog is open, and tool-key cycling.

use super::*;

const NONE: Modifiers = Modifiers::NONE;
const CTRL: Modifiers = Modifiers {
    ctrl: true,
    ..Modifiers::NONE
};
const SHIFT: Modifiers = Modifiers {
    shift: true,
    ..Modifiers::NONE
};
const CTRL_SHIFT: Modifiers = Modifiers {
    ctrl: true,
    shift: true,
    ..Modifiers::NONE
};
const CTRL_ALT: Modifiers = Modifiers {
    ctrl: true,
    alt: true,
    ..Modifiers::NONE
};
const ALT: Modifiers = Modifiers {
    alt: true,
    ..Modifiers::NONE
};

fn press(key: Key, modifiers: Modifiers) -> Option<Msg> {
    shortcut(key, modifiers, false)
}

fn command(command: Command) -> Option<Msg> {
    Some(Msg::Command(command))
}

#[test]
fn file_and_edit_keys_map_to_their_messages() {
    assert_eq!(press(Key::N, CTRL), Some(Msg::New));
    assert_eq!(press(Key::O, CTRL), Some(Msg::Open));
    assert_eq!(press(Key::S, CTRL), Some(Msg::Save));
    assert_eq!(press(Key::S, CTRL_SHIFT), Some(Msg::SaveAs));
    assert_eq!(press(Key::Q, CTRL), Some(Msg::Quit));
    assert_eq!(press(Key::Z, CTRL), Some(Msg::Undo));
    assert_eq!(press(Key::Y, CTRL), Some(Msg::Redo));
    assert_eq!(press(Key::Z, CTRL_SHIFT), Some(Msg::Redo));
    assert_eq!(press(Key::R, CTRL_SHIFT), Some(Msg::ResizeAsk));
}

#[test]
fn the_windows_key_counts_as_control() {
    let win = Modifiers {
        win: true,
        ..Modifiers::NONE
    };
    assert_eq!(press(Key::S, win), Some(Msg::Save));
}

#[test]
fn keys_of_unbuilt_features_name_their_command() {
    assert_eq!(press(Key::C, CTRL), command(Command::Copy));
    assert_eq!(press(Key::C, CTRL_SHIFT), command(Command::CopyMerged));
    assert_eq!(press(Key::V, CTRL_ALT), command(Command::PasteNewImage));
    assert_eq!(press(Key::DELETE, NONE), command(Command::EraseSelection));
    assert_eq!(press(Key::BACK, NONE), command(Command::FillSelection));
    assert_eq!(press(Key::F4, NONE), command(Command::LayerProperties));
    assert_eq!(press(Key::I, CTRL_SHIFT), command(Command::InvertColors));
    assert_eq!(press(Key::I, CTRL), command(Command::InvertSelection));
    assert_eq!(press(Key::DIGIT0, CTRL), command(Command::ActualSize));
    assert_eq!(press(Key::M, CTRL), command(Command::MergeLayerDown));
    assert_eq!(press(Key::M, NONE), Some(Msg::CycleTool(ToolGroup::Move)));
}

#[test]
fn zoom_keys_accept_both_plus_forms_and_the_keypad() {
    for modifiers in [CTRL, CTRL_SHIFT] {
        assert_eq!(press(PLUS, modifiers), command(Command::ZoomIn));
    }
    assert_eq!(press(NUM_PLUS, CTRL), command(Command::ZoomIn));
    assert_eq!(press(MINUS, CTRL), command(Command::ZoomOut));
    assert_eq!(press(NUM_MINUS, CTRL), command(Command::ZoomOut));
}

#[test]
fn tool_keys_select_tools_only_without_modifiers() {
    assert_eq!(press(Key::P, NONE), Some(Msg::Tool(Tool::Pencil)));
    assert_eq!(press(Key::B, NONE), Some(Msg::Tool(Tool::Brush)));
    assert_eq!(press(Key::E, NONE), Some(Msg::Tool(Tool::Eraser)));
    assert_eq!(press(Key::F, NONE), Some(Msg::Tool(Tool::Fill)));
    assert_eq!(press(Key::K, NONE), Some(Msg::Tool(Tool::Picker)));
    assert_eq!(press(Key::S, NONE), Some(Msg::CycleTool(ToolGroup::Select)));
    assert_eq!(press(Key::B, SHIFT), None);
    assert_eq!(press(Key::B, ALT), None);
    assert_eq!(press(Key::B, CTRL), command(Command::ZoomToWindow));
}

#[test]
fn brush_size_and_colour_keys() {
    assert_eq!(press(OPEN_BRACKET, NONE), Some(Msg::SizeStep(-1)));
    assert_eq!(press(CLOSE_BRACKET, NONE), Some(Msg::SizeStep(1)));
    assert_eq!(press(Key::X, NONE), Some(Msg::SwapColors));
    assert_eq!(press(Key::D, NONE), Some(Msg::ResetColors));
    assert_eq!(press(Key::D, CTRL), command(Command::Deselect));
}

#[test]
fn nothing_is_taken_while_a_dialog_is_open() {
    for (chord, _) in BINDINGS {
        let modifiers = Modifiers {
            ctrl: chord.ctrl,
            shift: chord.shift,
            alt: chord.alt,
            win: false,
        };
        assert!(
            press(chord.key, modifiers).is_some(),
            "{} is bound",
            chord.label()
        );
        assert_eq!(
            shortcut(chord.key, modifiers, true),
            None,
            "{} must pass through a dialog",
            chord.label()
        );
    }
}

#[test]
fn the_select_and_move_keys_cycle_their_groups() {
    let mut tool = Tool::Pencil;
    let mut seen = Vec::new();
    for _ in 0..5 {
        tool = ToolGroup::Select.next(tool);
        seen.push(tool);
    }
    assert_eq!(
        seen,
        [
            Tool::RectSelect,
            Tool::EllipseSelect,
            Tool::Lasso,
            Tool::MagicWand,
            Tool::RectSelect
        ]
    );
    assert_eq!(ToolGroup::Move.next(Tool::MovePixels), Tool::MoveSelection);
    assert_eq!(ToolGroup::Move.next(Tool::MoveSelection), Tool::MovePixels);
    assert_eq!(tool_hint(Tool::Lasso), "Lasso Select (S)");
    assert_eq!(tool_hint(Tool::MoveSelection), "Move Selection (M)");
}

#[test]
fn float_keys_nudge_land_and_cancel() {
    assert_eq!(float_key(Key::LEFT, NONE), Some(Msg::Nudge(-1, 0)));
    assert_eq!(float_key(Key::DOWN, NONE), Some(Msg::Nudge(0, 1)));
    assert_eq!(float_key(Key::RIGHT, SHIFT), Some(Msg::Nudge(10, 0)));
    assert_eq!(float_key(Key::UP, SHIFT), Some(Msg::Nudge(0, -10)));
    assert_eq!(float_key(Key::RETURN, NONE), Some(Msg::FloatCommit));
    assert_eq!(float_key(Key::ESCAPE, NONE), Some(Msg::FloatCancel));
    assert_eq!(float_key(Key::LEFT, CTRL), None);
    assert_eq!(float_key(Key::A, NONE), None);
}

#[test]
fn unbound_keys_pass_through() {
    assert_eq!(press(Key::ESCAPE, NONE), None);
    assert_eq!(press(Key::RETURN, NONE), None);
    assert_eq!(press(Key::A, NONE), None);
    assert_eq!(press(Key::LEFT, NONE), None);
    assert_eq!(press(Key::Q, NONE), None);
    assert_eq!(press(Key::Z, ALT), None);
}

#[test]
fn no_chord_is_bound_twice() {
    for (index, (chord, msg)) in BINDINGS.iter().enumerate() {
        for (other, other_msg) in &BINDINGS[index + 1..] {
            assert!(
                chord != other,
                "{} is bound to {msg:?} and {other_msg:?}",
                chord.label()
            );
        }
    }
}

#[test]
fn repeated_shape_key_cycles_the_group() {
    let mut tool = Tool::Pencil;
    let mut seen = Vec::new();
    for _ in 0..5 {
        let Some(Msg::CycleTool(group)) = press(Key::O, NONE) else {
            panic!("O cycles the shapes");
        };
        tool = group.next(tool);
        seen.push(tool);
    }
    assert_eq!(
        seen,
        [
            Tool::Line,
            Tool::Rectangle,
            Tool::RoundedRectangle,
            Tool::Ellipse,
            Tool::Line
        ],
        "from outside the group the first tool, then each in turn, wrapping"
    );
}

#[test]
fn chord_labels_match_what_the_menus_print() {
    let label = |msg| chord_for(msg).map(Chord::label);
    assert_eq!(label(Msg::Save).as_deref(), Some("Ctrl+S"));
    assert_eq!(label(Msg::SaveAs).as_deref(), Some("Ctrl+Shift+S"));
    assert_eq!(label(Msg::Redo).as_deref(), Some("Ctrl+Y"));
    assert_eq!(label(Msg::Undo).as_deref(), Some("Ctrl+Z"));
    assert_eq!(
        label(Msg::Command(Command::PasteNewImage)).as_deref(),
        Some("Ctrl+Alt+V")
    );
    assert_eq!(
        label(Msg::Command(Command::EraseSelection)).as_deref(),
        Some("Del")
    );
    assert_eq!(
        label(Msg::Command(Command::LayerProperties)).as_deref(),
        Some("F4")
    );
    assert_eq!(
        label(Msg::Command(Command::ZoomIn)).as_deref(),
        Some("Ctrl++")
    );
    assert_eq!(label(Msg::Clear), None);
}

#[test]
fn the_tool_hint_names_the_key() {
    assert_eq!(tool_hint(Tool::Pencil), "Pencil (P)");
    assert_eq!(tool_hint(Tool::Rectangle), "Rectangle (O)");
    assert_eq!(tool_hint(Tool::Picker), "Picker (K)");
}

#[test]
fn the_p4_tool_keys_select_their_tools() {
    for (key, tool) in [
        (Key::B, Tool::Brush),
        (Key::P, Tool::Pencil),
        (Key::E, Tool::Eraser),
        (Key::F, Tool::Fill),
        (Key::G, Tool::Gradient),
        (Key::K, Tool::Picker),
        (Key::T, Tool::Text),
        (Key::Z, Tool::Zoom),
        (Key::H, Tool::Pan),
    ] {
        assert_eq!(press(key, NONE), Some(Msg::Tool(tool)), "{tool:?}");
    }
    assert_eq!(press(OPEN_BRACKET, NONE), Some(Msg::SizeStep(-1)));
    assert_eq!(press(CLOSE_BRACKET, NONE), Some(Msg::SizeStep(1)));
}

#[test]
fn while_typing_letters_belong_to_the_text() {
    for key in [Key::B, Key::T, Key::X, Key::D, Key::BACK, Key::DELETE, OPEN_BRACKET] {
        assert_eq!(typing_shortcut(key, NONE, false), None, "{key:?}");
        assert_eq!(typing_shortcut(key, SHIFT, false), None, "{key:?}");
    }
    // Command chords and function keys still work.
    assert_eq!(typing_shortcut(Key::Z, CTRL, false), Some(Msg::Undo));
    assert_eq!(typing_shortcut(Key::S, CTRL, false), Some(Msg::Save));
    assert_eq!(
        typing_shortcut(Key::F4, NONE, false),
        command(Command::LayerProperties)
    );
    assert_eq!(typing_shortcut(Key::Z, CTRL, true), None, "a dialog wins");
}
