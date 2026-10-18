//! The window's chrome: the menu bar, the picture, the welcome screen and the
//! status bar.

use std::path::{Path, PathBuf};

use xui_core::arrange::{IntoEntry, 
    button, column, label, menu_bar, panel, separator, status_bar, Build, Entry, Handle, Layout,
    LayoutExt,
};
use xui_core::icon::Lucide;
use xui_core::widget::{Menu, MenuId, Panel, StatusBar};
use xui_core::Dip;

use crate::app::{Command, Msg};
use crate::screen::Screen;

/// The menu bar's and the status bar's heights.
pub const MENU_HEIGHT: Dip = Dip(28.0);
pub const STATUS_HEIGHT: Dip = Dip(24.0);
/// Most recent ROMs the welcome screen lists.
const RECENT_SHOWN: usize = 5;

/// Menu titles' ids, outside the commands' range.
pub const FILE_MENU: MenuId = MenuId::new(100);
pub const EMULATION_MENU: MenuId = MenuId::new(101);
pub const VIDEO_MENU: MenuId = MenuId::new(102);
pub const AUDIO_MENU: MenuId = MenuId::new(103);
pub const HELP_MENU: MenuId = MenuId::new(104);

/// The widgets the window changes after building.
#[derive(Default)]
pub struct Widgets {
    pub menu: Handle<Menu<Msg>>,
    pub welcome: Handle<Panel<Msg>>,
    pub status: Handle<StatusBar<Msg>>,
}

/// The whole window: the menu bar, the picture, the welcome panel (hidden
/// while a cartridge plays) and the status bar.
pub fn window(widgets: &Widgets, screen: Build<Screen, Msg>) -> Layout<Msg> {
    column().children((
        menus().bind(&widgets.menu).fixed(MENU_HEIGHT),
        screen.fill(1),
        panel(column().padding(24))
            .plain()
            .bind(&widgets.welcome)
            .fill(1),
        status_bar(&["NES", ""]).bind(&widgets.status).fixed(STATUS_HEIGHT),
    ))
}

/// The menu bar.
fn menus() -> Build<Menu<Msg>, Msg> {
    menu_bar(|bar| {
        bar.submenu(FILE_MENU, "&File", |m| {
            m.item(Command::OpenRom.id(), "&Open ROM...")
                .icon(Lucide::FolderOpen);
            m.item(Command::CloseRom.id(), "&Close ROM");
            m.separator();
            m.item(Command::SaveState.id(), "&Save State")
                .icon(Lucide::Save);
            m.item(Command::LoadState.id(), "&Load State")
                .icon(Lucide::Download);
            m.separator();
            m.item(Command::Quit.id(), "&Quit").icon(Lucide::X);
        });
        bar.submenu(EMULATION_MENU, "&Emulation", |m| {
            m.check(Command::Pause.id(), "&Paused", false);
            m.item(Command::Reset.id(), "&Reset");
            m.item(Command::PowerCycle.id(), "Power &Cycle");
            m.separator();
            m.check(Command::FastForward.id(), "&Fast Forward", false);
            m.separator();
            for (slot, command) in [
                (1, Command::Slot1),
                (2, Command::Slot2),
                (3, Command::Slot3),
                (4, Command::Slot4),
            ] {
                m.radio(command.id(), &format!("Save Slot &{slot}"), slot == 1);
            }
        });
        bar.submenu(VIDEO_MENU, "&Video", |m| {
            m.radio(Command::ScaleAuto.id(), "Scale: &Auto", true);
            m.radio(Command::Scale1.id(), "Scale: &1x", false);
            m.radio(Command::Scale2.id(), "Scale: &2x", false);
            m.radio(Command::Scale3.id(), "Scale: &3x", false);
            m.separator();
            m.radio(Command::AspectPixel.id(), "&Pixel-perfect (8:7)", true);
            m.radio(Command::AspectSquare.id(), "&Square pixels", false);
            m.separator();
            m.check(Command::Ntsc.id(), "&NTSC filter (CRT look)", false);
            m.check(Command::ShowFps.id(), "Show &FPS", false);
        });
        bar.submenu(AUDIO_MENU, "&Audio", |m| {
            m.check(Command::Mute.id(), "&Mute", false).icon(Lucide::Volume2);
        });
        bar.submenu(HELP_MENU, "&Help", |m| {
            m.item(Command::Controls.id(), "&Controls...")
                .icon(Lucide::Info);
            m.item(Command::About.id(), "&About NES")
                .icon(Lucide::Info);
        });
    })
    .on_select_with(|id| Command::from_id(id).map(Msg::Command))
    .on_toggle(|id, on| {
        Command::from_id(id).map_or(Msg::Noop, |command| Msg::Toggled(command, on))
    })
}

/// The welcome screen: what to do, the sample ROM, and the ROMs played last.
pub fn welcome_layout(recent: &[PathBuf], sample: Option<&Path>) -> Layout<Msg> {
    let mut children: Vec<Entry<Msg>> = vec![
        label("NES").title().into_entry(),
        label("An NES emulator with sound.").into_entry(),
        separator().into_entry(),
        label("Open a ROM (.nes) with File > Open ROM, or drop one on this window.").into_entry(),
    ];
    if let Some(sample) = sample {
        children.push(
            button("Play the sample ROM")
                .on_click(Msg::RomChosen(sample.to_path_buf()))
                .into_entry(),
        );
    }
    children.push(
        button("Open ROM...")
            .on_click(Msg::Command(Command::OpenRom))
            .into_entry(),
    );
    if !recent.is_empty() {
        children.push(separator().into_entry());
        children.push(label("Recent").title().into_entry());
        for path in recent.iter().take(RECENT_SHOWN) {
            children.push(
                button(display_name(path))
                    .on_click(Msg::RomChosen(path.clone()))
                    .into_entry(),
            );
        }
    }
    children.push(separator().into_entry());
    children.push(label("Controls").title().into_entry());
    children.push(label(CONTROLS).into_entry());
    column().padding(24).gap(8).children(children)
}

/// A path as a button reads: its file name, else the whole path.
pub fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// The keys, as the welcome screen and the Controls dialog show them.
pub const CONTROLS: &str = "\
Arrows: D-pad        Z / X: A / B        A / S: turbo A / B
Enter: Start         Tab or Q: Select     Space (hold): fast forward
Escape: pause        Ctrl+O: open ROM     Ctrl+S / Ctrl+L: save / load state
Ctrl+1..4: slot      Ctrl+R: reset        Ctrl+M: mute     Ctrl+Q: quit";

/// The About dialog's text.
pub const ABOUT: &str = "\
NES Player, a core LazyOS app.

The emulation is TetaNES's `tetanes-core` (MIT OR Apache-2.0): CPU, PPU, APU
and 30+ mappers. This window, the mixer integration and the files are LazyOS's
own work. NES is a trademark of Nintendo; no ROMs are shipped, only the
homebrew sample the welcome screen offers.";
