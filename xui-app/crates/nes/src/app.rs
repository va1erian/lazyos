//! The NES Player window: the commands the menus and keys raise, the picture,
//! and the evidence lines the binary prints.
//!
//! The window is xui's own: a menu bar, the custom [`Screen`] control that
//! owns the console, a welcome panel, and a status bar. Every report the
//! screen raises goes to the [`Host`], which is where the LazyOS binary turns
//! it into a `NES:*` trace line; the app itself never prints.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use xui_core::app::{App, Ui};
use xui_core::arrange::{build, Handle, Mounted};
use xui_core::backend::{Result, WidgetId};
use xui_core::message::{Key, Modifiers};
use xui_core::widget::{Dialog, FileDialog, Panel, StdFileSystem, StatusBar};
use xui_core::units::Dip;

use crate::host::{Host, Report};
use crate::paths;
use crate::screen::Screen;
use crate::session::Session;
use crate::settings::Settings;
use crate::ui;
use crate::video::{Aspect, Scale};

/// The window size when a compositor lays the app out: the picture at 2x
/// (585x480 with the NES's own pixel shape) plus the chrome.
pub const WINDOW: (i32, i32) = (620, 580);

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
const COMMANDS: [Command; 24] = [
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
    pub fn id(self) -> xui_core::widget::MenuId {
        let index = COMMANDS.iter().position(|c| *c == self).unwrap_or(0);
        xui_core::widget::MenuId::new(index + 1)
    }

    /// The command a menu id names.
    pub fn from_id(id: xui_core::widget::MenuId) -> Option<Command> {
        COMMANDS.iter().copied().find(|c| c.id() == id)
    }

    /// The slot a `Slot1`..`Slot4` command names.
    fn slot(self) -> Option<usize> {
        match self {
            Command::Slot1 => Some(1),
            Command::Slot2 => Some(2),
            Command::Slot3 => Some(3),
            Command::Slot4 => Some(4),
            _ => None,
        }
    }
}

/// One application message.
#[derive(Clone, Debug, PartialEq)]
pub enum Msg {
    /// A menu command (or a shortcut that raises one).
    Command(Command),
    /// A check or radio item was picked.
    Toggled(Command, bool),
    /// Something the screen wants reported.
    Report(Report),
    /// The Open dialog returned a path.
    RomChosen(PathBuf),
    /// The Open dialog was dismissed.
    RomCancelled,
    /// The once-a-second status refresh.
    Tick,
    /// A menu id no command owns (a submenu title).
    Noop,
}

/// The NES Player.
pub struct NesApp {
    host: Rc<RefCell<dyn Host>>,
    widgets: ui::Widgets,
    screen: Handle<Screen>,
    open: FileDialog<Msg>,
    controls: Dialog<Msg>,
    about: Dialog<Msg>,
    /// The mounted window; dropping it destroys the widgets.
    _mounted: Mounted<Msg>,
}

impl NesApp {
    /// Builds the window, plays `rom` when one is named, and shows the
    /// welcome screen otherwise.
    pub fn build(
        ui: &mut Ui<Msg>,
        host: Rc<RefCell<dyn Host>>,
        session: Session,
        rom: Option<PathBuf>,
    ) -> Result<NesApp> {
        let widgets = ui::Widgets::default();
        let screen: Handle<Screen> = Handle::new();
        let screen_entry = {
            let host = Rc::clone(&host);
            build(move |ui: &Ui<Msg>| Ok(Screen::new(ui, host, session))).bind(&screen)
        };
        let mounted = ui.mount(ui::window(&widgets, screen_entry))?;
        let open = FileDialog::open_file(ui, "Open ROM")?
            .file_system(Rc::new(StdFileSystem))
            .initial_dir(host.borrow().default_dir())
            .filter("NES ROMs (.nes)", &["nes"])
            .filter("All files", &[])
            .require_existing(true)
            .on_accept(|path| Some(Msg::RomChosen(path)))
            .on_cancel(|| Some(Msg::RomCancelled));
        let controls = Dialog::message(ui, "Controls", ui::CONTROLS)?.on_action(|_| None);
        let about = Dialog::message(ui, "About NES", ui::ABOUT)?.on_action(|_| None);
        let app = NesApp {
            host,
            widgets,
            screen,
            open,
            controls,
            about,
            _mounted: mounted,
        };
        app.restore_settings();
        ui.on_key(shortcut);
        ui.on_close(|| Some(Msg::Command(Command::Quit)));
        ui.every(1000, Msg::Tick);
        app.show_welcome(ui);
        if let Some(rom) = rom {
            app.open_rom(ui, &rom);
        }
        Ok(app)
    }

    /// The screen, for a host or a test to drive.
    pub fn screen(&self) -> Rc<Screen> {
        self.screen.get()
    }

    /// Applies the settings saved by the last run.
    fn restore_settings(&self) {
        let settings = Settings::load(&self.host.borrow().app_dir());
        {
            let screen = self.screen.get();
            let mut state = screen.state();
            let mut state = state.borrow_mut();
            state.set_scale(settings.scale);
            state.set_aspect(settings.aspect);
            state.set_show_fps(settings.show_fps);
            state.session_mut().set_slot(settings.slot);
            state.session_mut().set_ntsc(settings.ntsc);
        }
        if settings.muted {
            self.host.borrow_mut().set_muted(true);
        }
    }

    /// Writes the current options back, so the next run starts the same way.
    fn save_settings(&self) {
        let settings = {
            let screen = self.screen.get();
            let state = screen.state();
            let state = state.borrow();
            Settings {
                scale: state.scale(),
                aspect: state.aspect(),
                ntsc: state.session().ntsc(),
                show_fps: state.show_fps(),
                slot: state.session().slot(),
                muted: self.host.borrow().muted(),
            }
        };
        let dir = self.host.borrow().app_dir();
        if let Err(error) = settings.save(&dir) {
            self.host
                .borrow()
                .report(Report::Failed(format!("settings: {error}")));
        }
    }

    /// Shows the welcome screen and hides the picture.
    fn show_welcome(&self, ui: &mut Ui<Msg>) {
        let host = self.host.borrow();
        let recent = paths::read_recent(&host.app_dir());
        let sample = host.sample_rom();
        drop(host);
        let layout = ui::welcome_layout(&recent, sample.as_deref());
        let welcome = self.widgets.welcome.get();
        if let Err(error) = welcome.set_layout(layout) {
            self.host
                .borrow()
                .report(Report::Failed(format!("welcome screen: {error}")));
        }
        welcome.set_visible(true);
        ui.set_visible(self.screen.get().control().id(), false);
        ui.set_window_title("NES");
    }

    /// Shows the picture and hides the welcome screen.
    fn show_picture(&self, ui: &mut Ui<Msg>, title: &str) {
        self.widgets.welcome.get().set_visible(false);
        ui.set_visible(self.screen.get().control().id(), true);
        ui.set_window_title(title);
    }

    /// Loads `path`, switching the window over when it works.
    fn open_rom(&self, ui: &mut Ui<Msg>, path: &std::path::Path) {
        let screen = self.screen.get();
        let loaded = screen.state().borrow_mut().load_rom(path);
        let state = screen.state();
        let title = state.borrow().session().info().map(|info| info.name.clone());
        if loaded {
            self.show_picture(ui, &format!("NES — {}", title.unwrap_or_default()));
        }
        self.refresh(ui);
    }

    /// One menu command.
    fn command(&self, ui: &mut Ui<Msg>, command: Command) {
        let screen = self.screen.get();
        match command {
            Command::OpenRom => {
                if let Some(info) = screen.state().borrow().session().info() {
                    if let Some(dir) = info.path.parent() {
                        self.open.set_initial_dir(dir);
                    }
                }
                self.open.open();
                return;
            }
            Command::CloseRom => {
                screen.state().borrow_mut().unload_rom();
                self.show_welcome(ui);
            }
            Command::SaveState => {
                let slot = screen.state().borrow().session().slot();
                screen.state().borrow_mut().save_state(slot);
            }
            Command::LoadState => {
                let slot = screen.state().borrow().session().slot();
                screen.state().borrow_mut().load_state(slot);
            }
            Command::Pause => {
                let state = screen.state();
                let mut state = state.borrow_mut();
                let paused = !state.session().paused();
                state.session_mut().set_paused(paused);
            }
            Command::Reset => screen.state().borrow_mut().session_mut().reset(false),
            Command::PowerCycle => {
                let outcome = screen.state().borrow_mut().session_mut().power_cycle();
                if let Err(error) = outcome {
                    self.host.borrow().report(Report::Failed(error.to_string()));
                }
            }
            Command::FastForward => {
                let state = screen.state();
                let on = !state.borrow().fast_forward();
                state.borrow_mut().set_fast_forward(on);
            }
            Command::ScaleAuto | Command::Scale1 | Command::Scale2 | Command::Scale3 => {
                let scale = match command {
                    Command::ScaleAuto => Scale::Auto,
                    Command::Scale1 => Scale::Fixed(1),
                    Command::Scale2 => Scale::Fixed(2),
                    _ => Scale::Fixed(3),
                };
                screen.state().borrow_mut().set_scale(scale);
            }
            Command::AspectPixel | Command::AspectSquare => {
                let aspect = if command == Command::AspectPixel {
                    Aspect::Pixel8x7
                } else {
                    Aspect::Square
                };
                screen.state().borrow_mut().set_aspect(aspect);
            }
            Command::Ntsc => {
                let mut state = screen.state();
                let on = !state.borrow().session().ntsc();
                state.borrow_mut().session_mut().set_ntsc(on);
            }
            Command::ShowFps => {
                let state = screen.state();
                let on = !state.borrow().show_fps();
                state.borrow_mut().set_show_fps(on);
            }
            Command::Mute => {
                let on = !self.host.borrow().muted();
                self.host.borrow_mut().set_muted(on);
                self.report_sound();
            }
            Command::Controls => self.controls.open(),
            Command::About => self.about.open(),
            Command::Quit => {
                screen.state().borrow_mut().save_battery();
                self.save_settings();
                self.host.borrow().report(Report::Quit);
                ui.quit();
                return;
            }
            slot if slot.slot().is_some() => {
                screen
                    .state()
                    .borrow_mut()
                    .session_mut()
                    .set_slot(slot.slot().unwrap());
            }
            _ => return,
        }
        self.save_settings();
        self.refresh(ui);
    }

    /// A check or radio item.
    fn toggled(&self, ui: &mut Ui<Msg>, command: Command, on: bool) {
        match command {
            Command::Pause => self.screen.get().state().borrow_mut().session_mut().set_paused(on),
            Command::FastForward => self.screen.get().state().borrow_mut().set_fast_forward(on),
            Command::ShowFps => self.screen.get().state().borrow_mut().set_show_fps(on),
            Command::Mute => {
                self.host.borrow_mut().set_muted(on);
                self.report_sound();
            }
            slot if slot.slot().is_some() => {
                let slot = slot.slot().unwrap();
                self.screen
                    .get()
                    .state()
                    .borrow_mut()
                    .session_mut()
                    .set_slot(slot);
            }
            Command::ScaleAuto | Command::Scale1 | Command::Scale2 | Command::Scale3 => {
                self.command(ui, command);
            }
            Command::AspectPixel | Command::AspectSquare => self.command(ui, command),
            Command::Ntsc => {
                self.screen
                    .get()
                    .state()
                    .borrow_mut()
                    .session_mut()
                    .set_ntsc(on);
            }
            _ => {}
        }
        self.save_settings();
        self.refresh(ui);
    }

    /// Tells the host whether sound is playing.
    fn report_sound(&self) {
        let host = self.host.borrow();
        if host.muted() {
            host.report(Report::Silent("muted".to_string()));
        } else {
            host.report(Report::Sound {
                rate: host.audio_rate(),
                channels: host.audio_channels(),
            });
        }
    }

    /// The status bar and the menu check marks, once a second and after every
    /// command.
    fn refresh(&self, _ui: &mut Ui<Msg>) {
        let screen = self.screen.get();
        // Whatever the screen raised since the last look (the timer forwards
        // them too; whichever gets there first reports it).
        let reports = screen.state().borrow_mut().take_reports();
        for report in reports {
            self.host.borrow().report(report);
        }
        let state = screen.state();
        let mut state = state.borrow_mut();
        let fps = state.fps();
        let session = state.session();
        let rom = session
            .info()
            .map(|info| info.summary())
            .unwrap_or_else(|| "No cartridge".to_string());
        let playing = if session.paused() { "Paused" } else { "Playing" };
        let sound = {
            let mut host = self.host.borrow_mut();
            if host.muted() {
                "Sound: muted"
            } else if host.audio_free().is_some() {
                "Sound: on"
            } else {
                "Sound: off"
            }
        };
        let slot = format!("Slot {}", session.slot());
        let speed = if session.fast_forward() { "2x" } else { "1x" };
        let fps = if fps > 0 { format!("{fps} fps") } else { String::new() };
        drop(state);
        self.widgets
            .status
            .get()
            .set_parts(&[&rom, playing, &fps, speed, &slot, sound]);

        let menu = self.widgets.menu.get();
        let state = screen.state();
        let state = state.borrow();
        let session = state.session();
        menu.set_checked(Command::Pause.id(), session.paused());
        menu.set_checked(Command::FastForward.id(), state.fast_forward());
        menu.set_checked(Command::ShowFps.id(), state.show_fps());
        menu.set_checked(Command::Mute.id(), self.host.borrow().muted());
        menu.set_checked(Command::ScaleAuto.id(), state.scale() == Scale::Auto);
        menu.set_checked(Command::Scale1.id(), state.scale() == Scale::Fixed(1));
        menu.set_checked(Command::Scale2.id(), state.scale() == Scale::Fixed(2));
        menu.set_checked(Command::Scale3.id(), state.scale() == Scale::Fixed(3));
        menu.set_checked(Command::AspectPixel.id(), state.aspect() == Aspect::Pixel8x7);
        menu.set_checked(Command::AspectSquare.id(), state.aspect() == Aspect::Square);
        for slot in 1..=crate::session::MAX_SLOTS {
            if let Some(command) = [
                Command::Slot1,
                Command::Slot2,
                Command::Slot3,
                Command::Slot4,
            ]
            .get(slot - 1)
            {
                menu.set_checked(command.id(), session.slot() == slot);
            }
        }
    }

    /// The picture's node, so a test can check it is the visible one.
    pub fn screen_id(&self) -> WidgetId {
        self.screen.get().control().id()
    }
}

/// The keyboard shortcuts the window owns. A key with no shortcut is left to
/// the screen control (which is why Ctrl+S never also presses turbo B).
fn shortcut(key: Key, modifiers: Modifiers) -> Option<Msg> {
    if modifiers.ctrl {
        let command = match key {
            Key::O => Command::OpenRom,
            Key::S => Command::SaveState,
            Key::L => Command::LoadState,
            Key::R => Command::Reset,
            Key::Q => Command::Quit,
            Key::M => Command::Mute,
            Key::DIGIT1 => Command::Slot1,
            Key::DIGIT2 => Command::Slot2,
            Key::DIGIT3 => Command::Slot3,
            Key::DIGIT4 => Command::Slot4,
            _ => return None,
        };
        return Some(Msg::Command(command));
    }
    match key {
        Key::ESCAPE => Some(Msg::Command(Command::Pause)),
        Key::F1 => Some(Msg::Command(Command::Controls)),
        _ => None,
    }
}

impl App for NesApp {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            Msg::Command(command) => self.command(ui, command),
            Msg::Toggled(command, on) => self.toggled(ui, command, on),
            Msg::Report(report) => {
                self.host.borrow().report(report);
                self.refresh(ui);
            }
            Msg::RomChosen(path) => self.open_rom(ui, &path),
            Msg::RomCancelled => {}
            Msg::Tick => self.refresh(ui),
            Msg::Noop => {}
        }
    }
}

/// The panel type the welcome screen is mounted in, re-exported so the binary
/// and the tests can name it.
pub type WelcomePanel = Panel<Msg>;

/// The status bar's type, likewise.
pub type NesStatusBar = StatusBar<Msg>;

/// A `Dip` conversion the layout constants use.
pub const fn dip(value: i32) -> Dip {
    Dip(value as f32)
}
