//! The window's pages: the first-run consent screen, the key prompt, the chat
//! and the settings. Each is built once into its own panel; the shell shows
//! one at a time.

use std::rc::Rc;

use lazyagent_core::model::PRESETS;
use lazyagent_core::settings::{Settings, ThemeChoice};
use xui_core::app::Ui;
use xui_core::arrange::{
    build as create, button, column, combo_box, edit, label, list, multiline_edit, row, Handle,
    LayoutExt, Mounted,
};
use xui_core::backend::{Result, WidgetId};
use xui_core::layout::Align;
use xui_core::widget::{Button, ComboBox, Edit, Label, ListView, MultilineEdit};
use xui_core::HasText;

use crate::view::TranscriptView;
use crate::Msg;

/// What the user must know before anything is sent (docs/ai-agent-plan.md §3).
pub const CONSENT_LINES: [&str; 4] = [
    "Everything you type, and every reply, is sent to Fireworks AI, a company outside this computer.",
    "Your conversations are kept in this app's folder on this computer, readable by you only.",
    "Your API key is kept in memory while the app runs and is never written to disk.",
    "Do not paste passwords or other secrets into a chat.",
];

pub struct ConsentPage {
    _mounted: Mounted<Msg>,
}

impl ConsentPage {
    pub fn build(ui: &Ui<Msg>, page: WidgetId) -> Result<ConsentPage> {
        let mounted = ui.mount_in(
            page,
            column().padding(32).gap(12).align(Align::Center).children((
                label("AI Agent").title(),
                label("A chat with a large language model, hosted by Fireworks AI."),
                label(CONSENT_LINES[0]),
                label(CONSENT_LINES[1]),
                label(CONSENT_LINES[2]),
                label(CONSENT_LINES[3]),
                button("I understand, continue").on_click(Msg::Accept),
            )),
        )?;
        Ok(ConsentPage { _mounted: mounted })
    }
}

pub struct KeyPage {
    key: Rc<Edit<Msg>>,
    message: Rc<Label<Msg>>,
    _mounted: Mounted<Msg>,
}

impl KeyPage {
    pub fn build(ui: &Ui<Msg>, page: WidgetId) -> Result<KeyPage> {
        let (key, message) = (Handle::new(), Handle::new());
        let mounted = ui.mount_in(
            page,
            column().padding(32).gap(10).align(Align::Center).children((
                label("Fireworks API key").title(),
                label("Paste your key. It stays in memory until you close the app."),
                edit()
                    .password()
                    .placeholder("fw_...")
                    .bind(&key)
                    .width(360),
                button("Continue").on_click(Msg::KeySubmit),
                label("").bind(&message),
                label("Create a key at fireworks.ai, in your account's API keys."),
            )),
        )?;
        Ok(KeyPage {
            key: key.get(),
            message: message.get(),
            _mounted: mounted,
        })
    }

    /// The typed key; the field is emptied, so the secret lives in the
    /// request and nowhere else.
    pub fn take_key(&self) -> String {
        let key = self.key.text();
        self.key.set_text("");
        key
    }

    pub fn say(&self, line: &str) {
        self.message.set_text(line);
    }

    pub fn focus(&self) {
        self.key.focus();
    }
}

pub struct ChatPage {
    title: Rc<Label<Msg>>,
    status: Rc<Label<Msg>>,
    banner: Rc<Label<Msg>>,
    list: Rc<ListView<Msg>>,
    composer: Rc<MultilineEdit<Msg>>,
    send: Rc<Button<Msg>>,
    stop: Rc<Button<Msg>>,
    regenerate: Rc<Button<Msg>>,
    pub view: Rc<TranscriptView>,
    _mounted: Mounted<Msg>,
}

impl ChatPage {
    pub fn build(ui: &Ui<Msg>, page: WidgetId) -> Result<ChatPage> {
        let (title, status, banner, list_h, composer, send, stop, regenerate, view) = (
            Handle::new(),
            Handle::new(),
            Handle::new(),
            Handle::new(),
            Handle::new(),
            Handle::new(),
            Handle::new(),
            Handle::new(),
            Handle::new(),
        );
        let mounted = ui.mount_in(
            page,
            column().gap(0).children((
                row().padding(8).gap(8).children((
                    label("New chat").title().bind(&title).fill(1),
                    label("").bind(&status),
                    button("Export").on_click(Msg::ExportChat),
                    button("Settings").on_click(Msg::OpenSettings),
                )),
                row()
                    .gap(0)
                    .children((
                        column()
                            .padding(8)
                            .gap(6)
                            .children((
                                button("New chat (Ctrl+N)").on_click(Msg::NewChat),
                                edit()
                                    .placeholder("Search chats")
                                    .on_change(Msg::SearchText),
                                list()
                                    .then(|l| l.multi_select(false))
                                    .on_select(Msg::Select)
                                    .bind(&list_h)
                                    .fill(1),
                                button("Delete chat").on_click(Msg::DeleteChat),
                            ))
                            .width(240),
                        column()
                            .gap(0)
                            .children((
                                create(|ui: &Ui<Msg>| TranscriptView::new(ui))
                                    .bind(&view)
                                    .fill(1),
                                label("").bind(&banner),
                                row().padding(8).gap(8).children((
                                    multiline_edit()
                                        .on_change(Msg::ComposerText)
                                        .bind(&composer)
                                        .height(76)
                                        .fill(1),
                                    column().gap(6).children((
                                        button("Send").on_click(Msg::Send).bind(&send),
                                        button("Stop").on_click(Msg::Stop).bind(&stop),
                                        button("Redo").on_click(Msg::Regenerate).bind(&regenerate),
                                    )),
                                )),
                            ))
                            .fill(1),
                    ))
                    .fill(1),
            )),
        )?;
        let page = ChatPage {
            title: title.get(),
            status: status.get(),
            banner: banner.get(),
            list: list_h.get(),
            composer: composer.get(),
            send: send.get(),
            stop: stop.get(),
            regenerate: regenerate.get(),
            view: view.get(),
            _mounted: mounted,
        };
        page.set_busy(false);
        Ok(page)
    }

    pub fn set_title(&self, text: &str) {
        self.title.set_text(text);
    }

    pub fn set_status(&self, text: &str) {
        self.status.set_text(text);
    }

    pub fn set_banner(&self, text: &str) {
        self.banner.set_text(text);
    }

    pub fn show_list(&self, titles: &[String]) {
        let items: Vec<&str> = titles.iter().map(String::as_str).collect();
        self.list.set_items(&items);
    }

    /// Takes the composer's text and empties it.
    pub fn take_draft(&self) -> String {
        let text = self.composer.text();
        self.composer.set_text("");
        text
    }

    pub fn set_draft(&self, text: &str) {
        self.composer.set_text(text);
    }

    pub fn focus_composer(&self) {
        self.composer.focus();
    }

    pub fn set_busy(&self, busy: bool) {
        self.send.set_enabled(!busy);
        self.regenerate.set_enabled(!busy);
        self.stop.set_enabled(busy);
    }
}

const EFFORTS: [&str; 5] = ["Model default", "None", "Low", "Medium", "High"];

pub struct SettingsPage {
    preset: Rc<ComboBox<Msg>>,
    model: Rc<Edit<Msg>>,
    effort: Rc<ComboBox<Msg>>,
    theme: Rc<ComboBox<Msg>>,
    system: Rc<Edit<Msg>>,
    note: Rc<Label<Msg>>,
    _mounted: Mounted<Msg>,
}

impl SettingsPage {
    pub fn build(ui: &Ui<Msg>, page: WidgetId) -> Result<SettingsPage> {
        let (preset, model, effort, theme, system, note) = (
            Handle::new(),
            Handle::new(),
            Handle::new(),
            Handle::new(),
            Handle::new(),
            Handle::new(),
        );
        let names: Vec<&str> = PRESETS.iter().map(|m| m.name).collect();
        let mounted = ui.mount_in(
            page,
            column().gap(0).children((
                row().padding(8).gap(8).children((
                    button("Back").on_click(Msg::Back),
                    label("Settings").title().fill(1),
                )),
                column().padding(16).gap(8).children((
                    label("Model"),
                    combo_box(&names)
                        .on_select(Msg::SettingsModelPreset)
                        .bind(&preset)
                        .width(260),
                    edit()
                        .on_change(Msg::SettingsModelText)
                        .bind(&model)
                        .width(460),
                    label("Reasoning effort"),
                    combo_box(&EFFORTS)
                        .on_select(Msg::SettingsEffort)
                        .bind(&effort)
                        .width(200),
                    label("Theme"),
                    combo_box(&["Follow the system", "Light", "Dark"])
                        .on_select(Msg::SettingsTheme)
                        .bind(&theme)
                        .width(200),
                    label("Added to the system prompt (optional)"),
                    edit()
                        .on_change(Msg::SettingsSystem)
                        .bind(&system)
                        .width(460),
                    row().gap(8).children((
                        button("Forget the API key").on_click(Msg::ForgetKey),
                        label("").bind(&note),
                    )),
                    label("Conversations are stored in this app's folder; the key never is."),
                )),
            )),
        )?;
        Ok(SettingsPage {
            preset: preset.get(),
            model: model.get(),
            effort: effort.get(),
            theme: theme.get(),
            system: system.get(),
            note: note.get(),
            _mounted: mounted,
        })
    }

    /// Shows `settings` in the controls.
    pub fn load(&self, settings: &Settings) {
        self.model.set_text(&settings.model);
        if let Some(i) = PRESETS.iter().position(|m| m.id == settings.model) {
            self.preset.select(i);
        }
        let effort = EFFORTS
            .iter()
            .position(|e| e.eq_ignore_ascii_case(&settings.effort))
            .unwrap_or(0);
        self.effort.select(effort);
        self.theme.select(match settings.theme {
            ThemeChoice::System => 0,
            ThemeChoice::Light => 1,
            ThemeChoice::Dark => 2,
        });
        self.system.set_text(&settings.system);
    }

    pub fn say(&self, line: &str) {
        self.note.set_text(line);
    }
}

/// The effort a combo index means (`""` is the model's default).
pub fn effort_of(index: usize) -> String {
    match index {
        1 => "none",
        2 => "low",
        3 => "medium",
        4 => "high",
        _ => "",
    }
    .to_string()
}
