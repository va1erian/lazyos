//! The window's commands: one per menu item, with the menu ids they are
//! raised under.
//!
//! A command is what a menu item and a keyboard shortcut both name, so the
//! two never drift apart (the id order is the array's order).

use xui_core::widget::MenuId;

/// Every menu and shortcut command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    OpenRom,
    CloseRom,
    SaveState,
    LoadState,
    Pause,
    Reset,
    PowerCycle,
    FastForward,
    Slot1,
    Slot2,
    Slot3,
    Slot4,
    ScaleAuto,
    Scale1,
    Scale2,
    Scale3,
    AspectPixel,
    AspectSquare,
    Ntsc,
    ShowFps,
    Mute,
    Controls,
    About,
    Quit,
}

/// The commands, in menu-id order.
pub const COMMANDS: [Command; 24] = [
    Command::OpenRom,
    Command::CloseRom,
    Command::SaveState,
    Command::LoadState,
    Command::Pause,
    Command::Reset,
    Command::PowerCycle,
    Command::FastForward,
    Command::Slot1,
    Command::Slot2,
    Command::Slot3,
    Command::Slot4,
    Command::ScaleAuto,
    Command::Scale1,
    Command::Scale2,
    Command::Scale3,
    Command::AspectPixel,
    Command::AspectSquare,
    Command::Ntsc,
    Command::ShowFps,
    Command::Mute,
    Command::Controls,
    Command::About,
    Command::Quit,
];

impl Command {
    /// The menu id of this command.
    pub fn id(self) -> MenuId {
        let index = COMMANDS.iter().position(|c| *c == self).unwrap_or(0);
        MenuId::new(index + 1)
    }

    /// The command a menu id names.
    pub fn from_id(id: MenuId) -> Option<Command> {
        COMMANDS.iter().copied().find(|c| c.id() == id)
    }

    /// The save-state slot a `Slot1`..`Slot4` command names.
    pub fn slot(self) -> Option<usize> {
        match self {
            Command::Slot1 => Some(1),
            Command::Slot2 => Some(2),
            Command::Slot3 => Some(3),
            Command::Slot4 => Some(4),
            _ => None,
        }
    }

    /// The scale a `Scale*` command names (`None` for another command).
    pub fn scale(self) -> Option<crate::video::Scale> {
        use crate::video::Scale;
        match self {
            Command::ScaleAuto => Some(Scale::Auto),
            Command::Scale1 => Some(Scale::Fixed(1)),
            Command::Scale2 => Some(Scale::Fixed(2)),
            Command::Scale3 => Some(Scale::Fixed(3)),
            _ => None,
        }
    }

    /// The aspect an `Aspect*` command names (`None` for another command).
    pub fn aspect(self) -> Option<crate::video::Aspect> {
        match self {
            Command::AspectPixel => Some(crate::video::Aspect::Pixel8x7),
            Command::AspectSquare => Some(crate::video::Aspect::Square),
            _ => None,
        }
    }
}
