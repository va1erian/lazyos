#![forbid(unsafe_code)]

//! The keyboard: Paint.NET's keys as one table, and the pure [`shortcut`]
//! function the window's key hook calls.
//!
//! The table is the single source of truth. The menus read it to print each
//! item's chord ([`chord_for`]), so a label cannot disagree with the key that
//! does the work, and the tests walk it to check that no two bindings collide.
//!
//! Nothing here is claimed while a dialog, the colour editor or a menu is open
//! (`dialog_open`): Esc and Enter belong to the dialog, and a letter typed into
//! a hex or path field must not switch the tool.

use xui_core::message::{Key, Modifiers};

use super::{Command, Msg};
use crate::model::Tool;

/// A set of tools one key cycles through when it is pressed again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolGroup {
    /// Line, rectangle and ellipse share `O`.
    Shapes,
    /// Rectangle select, ellipse select, lasso and magic wand share `S`.
    Select,
    /// Move Selected Pixels and Move Selection share `M`.
    Move,
}

impl ToolGroup {
    /// The tools of the group, in cycling order.
    pub const fn tools(self) -> &'static [Tool] {
        match self {
            ToolGroup::Shapes => &[Tool::Line, Tool::Rectangle, Tool::Ellipse],
            ToolGroup::Select => &[
                Tool::RectSelect,
                Tool::EllipseSelect,
                Tool::Lasso,
                Tool::MagicWand,
            ],
            ToolGroup::Move => &[Tool::MovePixels, Tool::MoveSelection],
        }
    }

    /// The tool a press of the group's key selects when `current` is active: the
    /// next one of the group, or the first when `current` is not in it.
    pub fn next(self, current: Tool) -> Tool {
        let tools = self.tools();
        match tools.iter().position(|tool| *tool == current) {
            Some(index) => tools[(index + 1) % tools.len()],
            None => tools[0],
        }
    }
}

/// A key and the modifiers that must be held with it, exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chord {
    /// The key.
    pub key: Key,
    /// Ctrl (the Windows key counts too).
    pub ctrl: bool,
    /// Shift.
    pub shift: bool,
    /// Alt.
    pub alt: bool,
}

/// `[` (`VK_OEM_4`).
const OPEN_BRACKET: Key = Key::from_code(0xDB);
/// `]` (`VK_OEM_6`).
const CLOSE_BRACKET: Key = Key::from_code(0xDD);
/// `=` / `+` (`VK_OEM_PLUS`).
const PLUS: Key = Key::from_code(0xBB);
/// `-` / `_` (`VK_OEM_MINUS`).
const MINUS: Key = Key::from_code(0xBD);
/// The numeric keypad's `+`.
const NUM_PLUS: Key = Key::from_code(0x6B);
/// The numeric keypad's `-`.
const NUM_MINUS: Key = Key::from_code(0x6D);
/// The numeric keypad's `0`.
const NUM_ZERO: Key = Key::from_code(0x60);

const fn plain(key: Key) -> Chord {
    Chord {
        key,
        ctrl: false,
        shift: false,
        alt: false,
    }
}

const fn ctrl(key: Key) -> Chord {
    Chord {
        ctrl: true,
        ..plain(key)
    }
}

const fn ctrl_shift(key: Key) -> Chord {
    Chord {
        ctrl: true,
        shift: true,
        ..plain(key)
    }
}

const fn ctrl_alt(key: Key) -> Chord {
    Chord {
        ctrl: true,
        alt: true,
        ..plain(key)
    }
}

impl Chord {
    /// Whether `key` with `modifiers` is this chord.
    fn matches(self, key: Key, modifiers: Modifiers) -> bool {
        self.key == key
            && self.ctrl == (modifiers.ctrl || modifiers.win)
            && self.shift == modifiers.shift
            && self.alt == modifiers.alt
    }

    /// The chord as a menu prints it: `Ctrl+Shift+S`.
    pub fn label(self) -> String {
        let mut text = String::new();
        for (held, name) in [
            (self.ctrl, "Ctrl+"),
            (self.shift, "Shift+"),
            (self.alt, "Alt+"),
        ] {
            if held {
                text.push_str(name);
            }
        }
        text.push_str(&key_name(self.key));
        text
    }
}

/// The name a key goes by in a menu.
fn key_name(key: Key) -> String {
    match key {
        Key::BACK => "Backspace".to_string(),
        Key::DELETE => "Del".to_string(),
        PLUS | NUM_PLUS => "+".to_string(),
        MINUS | NUM_MINUS => "-".to_string(),
        OPEN_BRACKET => "[".to_string(),
        CLOSE_BRACKET => "]".to_string(),
        NUM_ZERO => "0".to_string(),
        other => match other.code() {
            code @ (0x30..=0x39 | 0x41..=0x5A) => char::from(code as u8).to_string(),
            code @ 0x70..=0x7B => format!("F{}", code - 0x6F),
            code => format!("#{code:02X}"),
        },
    }
}

/// Every binding. The first chord listed for a message is the one a menu shows.
pub static BINDINGS: &[(Chord, Msg)] = &[
    // File.
    (ctrl(Key::N), Msg::New),
    (ctrl(Key::O), Msg::Open),
    (ctrl(Key::S), Msg::Save),
    (ctrl_shift(Key::S), Msg::SaveAs),
    (ctrl(Key::Q), Msg::Quit),
    // Edit.
    (ctrl(Key::Z), Msg::Undo),
    (ctrl(Key::Y), Msg::Redo),
    (ctrl_shift(Key::Z), Msg::Redo),
    (ctrl(Key::X), Msg::Command(Command::Cut)),
    (ctrl(Key::C), Msg::Command(Command::Copy)),
    (ctrl_shift(Key::C), Msg::Command(Command::CopyMerged)),
    (ctrl(Key::V), Msg::Command(Command::Paste)),
    (ctrl_shift(Key::V), Msg::Command(Command::PasteNewLayer)),
    (ctrl_alt(Key::V), Msg::Command(Command::PasteNewImage)),
    (plain(Key::DELETE), Msg::Command(Command::EraseSelection)),
    (plain(Key::BACK), Msg::Command(Command::FillSelection)),
    (ctrl(Key::I), Msg::Command(Command::InvertSelection)),
    (ctrl(Key::A), Msg::Command(Command::SelectAll)),
    (ctrl(Key::D), Msg::Command(Command::Deselect)),
    // View: `+` is a shifted key on a US layout, so both forms count.
    (ctrl(PLUS), Msg::Command(Command::ZoomIn)),
    (ctrl_shift(PLUS), Msg::Command(Command::ZoomIn)),
    (ctrl(NUM_PLUS), Msg::Command(Command::ZoomIn)),
    (ctrl(MINUS), Msg::Command(Command::ZoomOut)),
    (ctrl(NUM_MINUS), Msg::Command(Command::ZoomOut)),
    (ctrl(Key::B), Msg::Command(Command::ZoomToWindow)),
    (ctrl(Key::DIGIT0), Msg::Command(Command::ActualSize)),
    (ctrl(NUM_ZERO), Msg::Command(Command::ActualSize)),
    // Image.
    (ctrl_shift(Key::X), Msg::Command(Command::CropToSelection)),
    (ctrl(Key::R), Msg::Command(Command::ResizeImage)),
    (ctrl_shift(Key::R), Msg::ResizeAsk),
    (ctrl_shift(Key::F), Msg::Command(Command::Flatten)),
    // Layers.
    (ctrl_shift(Key::N), Msg::Command(Command::AddLayer)),
    (ctrl_shift(Key::D), Msg::Command(Command::DuplicateLayer)),
    (ctrl(Key::M), Msg::Command(Command::MergeLayerDown)),
    (plain(Key::F4), Msg::Command(Command::LayerProperties)),
    // Adjustments.
    (ctrl_shift(Key::I), Msg::Command(Command::InvertColors)),
    (ctrl_shift(Key::G), Msg::Command(Command::BlackAndWhite)),
    (ctrl_shift(Key::E), Msg::Command(Command::Sepia)),
    // Tools.
    (plain(Key::S), Msg::CycleTool(ToolGroup::Select)),
    (plain(Key::M), Msg::CycleTool(ToolGroup::Move)),
    (plain(Key::Z), Msg::Command(Command::ZoomTool)),
    (plain(Key::H), Msg::Command(Command::PanTool)),
    (plain(Key::P), Msg::Tool(Tool::Pencil)),
    (plain(Key::B), Msg::Tool(Tool::Brush)),
    (plain(Key::E), Msg::Tool(Tool::Eraser)),
    (plain(Key::F), Msg::Tool(Tool::Fill)),
    (plain(Key::G), Msg::Command(Command::GradientTool)),
    (plain(Key::K), Msg::Tool(Tool::Picker)),
    (plain(Key::T), Msg::Command(Command::TextTool)),
    (plain(Key::O), Msg::CycleTool(ToolGroup::Shapes)),
    // Brush size and colours.
    (plain(OPEN_BRACKET), Msg::SizeStep(-1)),
    (plain(CLOSE_BRACKET), Msg::SizeStep(1)),
    (plain(Key::X), Msg::SwapColors),
    (plain(Key::D), Msg::ResetColors),
];

/// The message a key press means, if any. `dialog_open` is true while a
/// dialog, the colour editor or a menu has the keyboard: nothing is taken.
pub fn shortcut(key: Key, modifiers: Modifiers, dialog_open: bool) -> Option<Msg> {
    if dialog_open {
        return None;
    }
    BINDINGS
        .iter()
        .find(|(chord, _)| chord.matches(key, modifiers))
        .map(|(_, msg)| *msg)
}

/// The keys that place floating pixels or move the selection, claimed only
/// while a move tool is active or pixels float (the window's key hook asks
/// [`PaintApp`](super::PaintApp), which knows): the arrows nudge by one pixel
/// (Shift: ten), Enter lands the float, Esc cancels it.
pub fn float_key(key: Key, modifiers: Modifiers) -> Option<Msg> {
    if modifiers.ctrl || modifiers.alt || modifiers.win {
        return None;
    }
    let step = if modifiers.shift { 10 } else { 1 };
    match key {
        Key::LEFT => Some(Msg::Nudge(-step, 0)),
        Key::RIGHT => Some(Msg::Nudge(step, 0)),
        Key::UP => Some(Msg::Nudge(0, -step)),
        Key::DOWN => Some(Msg::Nudge(0, step)),
        Key::RETURN => Some(Msg::FloatCommit),
        Key::ESCAPE => Some(Msg::FloatCancel),
        _ => None,
    }
}

/// The chord a menu prints for `msg`: the first binding that raises it.
pub fn chord_for(msg: Msg) -> Option<Chord> {
    BINDINGS
        .iter()
        .find(|(_, bound)| *bound == msg)
        .map(|(chord, _)| *chord)
}

/// The key that selects `tool`, as the tool grid's hint prints it: `Line (O)`.
pub fn tool_hint(tool: Tool) -> String {
    let key = BINDINGS.iter().find_map(|(chord, msg)| {
        let selects = match msg {
            Msg::Tool(bound) => *bound == tool,
            Msg::CycleTool(group) => group.tools().contains(&tool),
            _ => false,
        };
        selects.then(|| chord.label())
    });
    match key {
        Some(key) => format!("{} ({key})", tool.label()),
        None => tool.label().to_string(),
    }
}

#[cfg(test)]
mod tests;
