//! The window: first-run consent, the key prompt, then the chat (and the
//! settings over it).

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use lazyagent_core::model::PRESETS;
use lazyagent_core::settings::{Settings, ThemeChoice};
use lazyagent_core::store::{Hit, Summary};
use lazyagent_core::{Conversation, Core, Events, Platform, Request, Secret};
use xui_core::arrange::{column, panel, Handle, LayoutExt};
use xui_core::backend::{Result, TimerId};
use xui_core::widget::{FileDialog, Panel, TaskDialog, TaskDialogAction, TaskDialogIcon};
use xui_core::{App, Theme, Ui};

use crate::drain;
use crate::layouts::Layouts;
use crate::model::RowId;
use crate::pages::{effort_of, ChatPage, ConsentPage, KeyPage, SettingsPage};
use crate::rows::Live;
use crate::view::ViewAction;
use crate::{Msg, Report};
use keys::{shortcut, Shared, Typing};

mod actions;
mod events;
mod keys;
mod sync;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Consent,
    Key,
    Chat,
    Settings,
}

impl Page {
    fn index(self) -> usize {
        match self {
            Page::Consent => 0,
            Page::Key => 1,
            Page::Chat => 2,
            Page::Settings => 3,
        }
    }
}

pub struct LazyAgentApp {
    core: Core,
    events: Events,
    layouts: Layouts,
    platform: Arc<dyn Platform>,
    _consent: ConsentPage,
    key: KeyPage,
    chat: ChatPage,
    settings_page: SettingsPage,
    panels: Vec<Rc<Panel<Msg>>>,
    shared: Rc<Shared>,
    page: Page,
    settings: Settings,
    settings_dirty: bool,
    have_key: bool,
    /// The conversations listed, and the ids of the rows shown (a search
    /// shows its hits instead).
    conversations: Vec<Summary>,
    shown: Vec<String>,
    hits: Option<Vec<Hit>>,
    current: Option<Box<Conversation>>,
    live: Live,
    /// The message just sent, until the conversation comes back with it.
    pending_user: Option<String>,
    failure: Option<String>,
    opened: HashSet<RowId>,
    rows_dirty: bool,
    busy: bool,
    turn_started: Option<Instant>,
    first_chunk: bool,
    /// The newest layout job per row: its revision, the width it was laid
    /// out for and the identity of the row's spec.
    submitted: HashMap<RowId, (u64, i32, usize)>,
    /// Layout results a tick could not apply because the view was borrowed.
    carried: Vec<crate::layouts::Done>,
    settings_known: bool,
    rev: u64,
    reported_rows: usize,
    reported_following: bool,
    export_picker: FileDialog<Msg>,
    link_dialog: Option<TaskDialog<Msg>>,
    pending_link: Option<String>,
    system_theme: Theme,
    timer: Option<(TimerId, u32)>,
    report: Box<dyn Fn(&Msg)>,
}

impl LazyAgentApp {
    /// Builds the window in `ui`; `report` hears every [`Report`] and the
    /// quit.
    pub fn build(
        ui: &mut Ui<Msg>,
        platform: Arc<dyn Platform>,
        report: impl Fn(&Msg) + 'static,
    ) -> Result<LazyAgentApp> {
        let frames: Vec<Handle<Panel<Msg>>> = (0..4).map(|_| Handle::new()).collect();
        let mut root = column();
        for frame in &frames {
            root = root.child(panel(column()).plain().bind(frame).fill(1));
        }
        ui.root(root)?;
        let panels: Vec<Rc<Panel<Msg>>> = frames.iter().map(Handle::get).collect();
        let consent = ConsentPage::build(ui, panels[0].id())?;
        let key = KeyPage::build(ui, panels[1].id())?;
        let chat = ChatPage::build(ui, panels[2].id())?;
        let settings_page = SettingsPage::build(ui, panels[3].id())?;
        let shared = Rc::new(Shared {
            page: Cell::new(Page::Consent),
            typing: Cell::new(Typing::Composer),
            busy: Cell::new(false),
        });
        let system_theme = ui.theme_handle().get();
        ui.on_close(|| Some(Msg::Quit));
        ui.on_timer(|_| Some(Msg::Tick));
        {
            let shared = Rc::clone(&shared);
            ui.on_key(move |key, modifiers| shortcut(&shared, key, modifiers));
        }
        let export_picker = FileDialog::save_file(ui, "Export conversation")?
            .filter("Markdown", &["md"])
            .filter("All files", &[])
            .on_accept(|path| Some(Msg::ExportPicked(path)))
            .on_cancel(|| None);
        export_picker.set_initial_dir(platform.app_dir());
        let (core, events) = Core::start(Arc::clone(&platform));
        // The shaper is taken once, here, and lives on the layout thread.
        let layouts = Layouts::start(ui.text_shaper());
        let mut app = LazyAgentApp {
            core,
            events,
            layouts,
            platform,
            _consent: consent,
            key,
            chat,
            settings_page,
            panels,
            shared,
            page: Page::Consent,
            settings: Settings::default(),
            settings_dirty: false,
            have_key: false,
            conversations: Vec::new(),
            shown: Vec::new(),
            hits: None,
            current: None,
            live: Live::default(),
            pending_user: None,
            failure: None,
            opened: HashSet::new(),
            rows_dirty: true,
            busy: false,
            turn_started: None,
            first_chunk: false,
            submitted: HashMap::new(),
            carried: Vec::new(),
            settings_known: false,
            rev: 0,
            reported_rows: usize::MAX,
            reported_following: true,
            export_picker,
            link_dialog: None,
            pending_link: None,
            system_theme,
            timer: None,
            report: Box::new(report),
        };
        app.chat.view.state.borrow_mut().note =
            "Ask anything. Enter sends, Shift+Enter starts a new line.".to_string();
        app.show(ui, Page::Consent);
        app.set_interval(ui, drain::BUSY_MS);
        Ok(app)
    }

    /// The transcript view (tests drive it).
    pub fn view(&self) -> Rc<crate::view::TranscriptView> {
        Rc::clone(&self.chat.view)
    }

    fn say(&self, report: Report) {
        (self.report)(&Msg::Report(report));
    }

    /// Shows `page` and puts the cursor where it belongs.
    fn show(&mut self, ui: &Ui<Msg>, page: Page) {
        self.page = page;
        self.shared.page.set(page);
        for (i, panel) in self.panels.iter().enumerate() {
            ui.set_visible(panel.id(), i == page.index());
        }
        match page {
            Page::Key => self.key.focus(),
            Page::Chat => self.chat.focus_composer(),
            Page::Settings => self.settings_page.load(&self.settings),
            Page::Consent => {}
        }
    }

    pub(crate) fn apply_theme(&self, ui: &Ui<Msg>) {
        let theme = match self.settings.theme {
            ThemeChoice::System => self.system_theme,
            ThemeChoice::Light => Theme::light(),
            ThemeChoice::Dark => Theme::dark(),
        };
        ui.set_theme(theme);
    }

    /// Runs the timer at `millis` (restarting it only when that changed).
    fn set_interval(&mut self, ui: &mut Ui<Msg>, millis: u32) {
        if self.timer.is_some_and(|(_, current)| current == millis) {
            return;
        }
        if let Some((timer, _)) = self.timer.take() {
            ui.kill_timer(timer);
        }
        self.timer = Some((ui.set_timer(millis), millis));
    }

    fn set_busy(&mut self, busy: bool) {
        self.busy = busy;
        self.shared.busy.set(busy);
        self.chat.set_busy(busy);
    }

    fn save_settings(&mut self) {
        if self.settings_dirty {
            self.settings_dirty = false;
            self.core.send(Request::SaveSettings(self.settings.clone()));
        }
    }

    /// The current conversation's id, if one is open.
    fn current_id(&self) -> Option<String> {
        self.current.as_ref().map(|c| c.id.clone())
    }

    fn send_text(&mut self, ui: &mut Ui<Msg>, text: String, edit: bool) {
        if self.page != Page::Chat || self.busy {
            return;
        }
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        if !self.have_key {
            // Keep what was typed and ask for the key first.
            self.chat.set_draft(&text);
            self.key.say("Enter your Fireworks API key to chat.");
            self.show(ui, Page::Key);
            return;
        }
        self.save_settings();
        let turns = self.current.as_ref().map_or(0, |c| c.turns()) + usize::from(!edit);
        if edit {
            self.core.send(Request::EditLast(text.clone()));
        } else {
            self.core.send(Request::Send(text.clone()));
        }
        self.begin_turn(Some(text));
        self.say(Report::Sent(turns));
        self.set_interval(ui, drain::BUSY_MS);
    }

    fn begin_turn(&mut self, user_text: Option<String>) {
        self.pending_user = user_text;
        self.live = Live {
            active: true,
            ..Live::default()
        };
        self.failure = None;
        self.chat.set_banner("");
        self.set_busy(true);
        self.turn_started = Some(Instant::now());
        self.first_chunk = false;
        self.rows_dirty = true;
        self.chat.view.state.borrow_mut().transcript.scroll_to_end();
    }
}

impl App for LazyAgentApp {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            Msg::Tick => self.tick(ui),
            Msg::Accept => {
                self.settings.consented = true;
                self.core.send(Request::SaveSettings(self.settings.clone()));
                self.say(Report::Consented);
                let next = if self.have_key { Page::Chat } else { Page::Key };
                self.show(ui, next);
            }
            Msg::KeySubmit => {
                let key = self.key.take_key();
                self.update(Msg::KeyEntered(Secret::new(key)), ui);
            }
            Msg::KeyEntered(key) => {
                if key.is_empty() {
                    self.key.say("Paste your key first.");
                    return;
                }
                self.core.send(Request::SetKey(key));
                self.have_key = true;
                self.key.say("");
                self.say(Report::KeySet);
                self.show(ui, Page::Chat);
            }
            Msg::Send => {
                if self.page == Page::Chat && !self.busy {
                    let text = self.chat.take_draft();
                    self.send_text(ui, text, false);
                }
            }
            Msg::SendText(text) => self.send_text(ui, text, false),
            Msg::EditLast => {
                if self.page == Page::Chat && !self.busy {
                    let text = self.chat.take_draft();
                    self.send_text(ui, text, true);
                }
            }
            Msg::ComposerText(text) => {
                self.shared.typing.set(Typing::Composer);
                // The Enter that sent a message (or submitted the key) can land
                // in the composer as a stray new line: a blank draft is no draft.
                if !text.is_empty() && text.trim().is_empty() {
                    self.chat.set_draft("");
                }
            }
            Msg::Stop => self.core.stop(),
            Msg::Regenerate => {
                if self.page == Page::Chat && !self.busy && self.current.is_some() && self.have_key
                {
                    self.core.send(Request::Regenerate);
                    self.begin_turn(None);
                    self.set_interval(ui, drain::BUSY_MS);
                }
            }
            Msg::NewChat => {
                if !self.busy {
                    self.core.send(Request::New);
                    self.chat.focus_composer();
                    self.shared.typing.set(Typing::Composer);
                }
            }
            Msg::Select(i) => {
                if !self.busy {
                    if let Some(id) = self.shown.get(i) {
                        self.core.send(Request::Open(id.clone()));
                    }
                }
            }
            Msg::DeleteChat => {
                if let (false, Some(id)) = (self.busy, self.current_id()) {
                    self.core.send(Request::Delete(id));
                }
            }
            Msg::ExportChat => {
                if self.current.is_some() {
                    self.export_picker.open();
                }
            }
            Msg::ExportPicked(path) => {
                if let Some(id) = self.current_id() {
                    self.core.send(Request::ExportTo { id, path });
                }
            }
            Msg::SearchText(text) => {
                self.shared.typing.set(Typing::Other);
                if text.trim().is_empty() {
                    self.hits = None;
                    self.refresh_list();
                } else {
                    self.core.send(Request::Search(text));
                }
            }
            Msg::OpenSettings => self.show(ui, Page::Settings),
            Msg::Back => {
                self.save_settings();
                self.show(ui, Page::Chat);
            }
            Msg::SettingsModelPreset(i) => {
                if let Some(preset) = PRESETS.get(i) {
                    self.settings.model = preset.id.to_string();
                    self.settings_dirty = true;
                    self.settings_page.load(&self.settings);
                }
            }
            Msg::SettingsModelText(text) => {
                self.settings.model = text;
                self.settings_dirty = true;
            }
            Msg::SettingsEffort(i) => {
                self.settings.effort = effort_of(i);
                self.settings_dirty = true;
            }
            Msg::SettingsTheme(i) => {
                self.settings.theme = match i {
                    1 => ThemeChoice::Light,
                    2 => ThemeChoice::Dark,
                    _ => ThemeChoice::System,
                };
                self.apply_theme(ui);
                self.settings_dirty = true;
            }
            Msg::SettingsSystem(text) => {
                self.settings.system = text;
                self.settings_dirty = true;
            }
            Msg::ForgetKey => {
                self.core.send(Request::ForgetKey);
                self.have_key = false;
                self.settings_page.say("The key was forgotten.");
                self.key.say("");
                self.save_settings();
                self.show(ui, Page::Key);
            }
            Msg::Transcript(action) => self.clicked(ui, action),
            Msg::LinkChoice(action) => {
                if let Some(url) = self.pending_link.take() {
                    if action == TaskDialogAction::Command(0) {
                        match self.platform.open_url(&url) {
                            Ok(()) => self.say(Report::LinkOpened(url)),
                            Err(why) => self.chat.set_banner(&why),
                        }
                    }
                }
                self.link_dialog = None;
            }
            Msg::Report(_) => (self.report)(&msg),
            Msg::Quit => {
                self.save_settings();
                (self.report)(&Msg::Quit);
                ui.quit();
            }
        }
    }
}
