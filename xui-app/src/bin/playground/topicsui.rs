//! The Topic monitor window's widgets and layout
//! (`docs/messenger-playground-plan.md` section 3): menu bar, the
//! developer-tool strip, the filter and Pause/Clear/Pin row, one tab per
//! subscription with its decoded events, Copy/Save/Stop and the counters, the
//! selected event's details, the Publish form and a status bar.

use std::rc::Rc;

use xui_app::devtool::{strip, Notice, Strip};
use xui_core::arrange::{
    button, checkbox, column, edit, label, list, menu_bar, row, status_bar, tabs, Build, Handle,
    Layout, LayoutExt,
};
use xui_core::widget::{Button, CheckBox, Edit, Label, ListView, Menu, MenuId, StatusBar, Tabs};
use xui_core::Fill;

use crate::tmsg::TMsg;

/// The menu bar's design height.
const MENU_HEIGHT: i32 = 30;

// Events menu.
pub const PAUSE: MenuId = MenuId::new(1);
pub const CLEAR: MenuId = MenuId::new(2);
pub const PIN: MenuId = MenuId::new(3);
pub const COPY: MenuId = MenuId::new(4);
pub const SAVE: MenuId = MenuId::new(5);
pub const STOP: MenuId = MenuId::new(6);
pub const CLOSE: MenuId = MenuId::new(7);
// View menu.
pub const NOTICE: MenuId = MenuId::new(11);

fn menu_msg(id: MenuId) -> Option<TMsg> {
    Some(match id {
        PAUSE => TMsg::Pause,
        CLEAR => TMsg::ClearEvents,
        PIN => TMsg::PinEvent,
        COPY => TMsg::CopyEvent,
        SAVE => TMsg::SaveEvents,
        STOP => TMsg::CloseSub,
        CLOSE => TMsg::Close,
        NOTICE => TMsg::NoticeShow,
        _ => return None,
    })
}

fn menus() -> Build<Menu<TMsg>, TMsg> {
    menu_bar(|bar| {
        bar.submenu(MenuId::new(100), "&Events", |m| {
            m.item(PAUSE, "&Pause or resume");
            m.item(CLEAR, "&Clear");
            m.item(PIN, "P&in the selected event");
            m.item(COPY, "Copy as &Rhai");
            m.item(SAVE, "&Save events");
            m.separator();
            m.item(STOP, "S&top listening");
            m.separator();
            m.item(CLOSE, "Close &window  Ctrl+W");
        });
        bar.submenu(MenuId::new(200), "&View", |m| {
            m.item(NOTICE, "Show developer-tool &notice  Ctrl+Shift+D");
        });
    })
    .on_select_with(menu_msg)
}

/// Every widget the monitor changes after start-up.
#[derive(Default)]
pub struct TopicsWidgets {
    pub strip: Handle<Strip<TMsg>>,
    pub subs: Handle<Tabs<TMsg>>,
    pub sub_filter: Handle<Edit<TMsg>>,
    pub pause: Handle<Button<TMsg>>,
    pub clear: Handle<Button<TMsg>>,
    pub pin: Handle<Button<TMsg>>,
    pub copy: Handle<Button<TMsg>>,
    pub save: Handle<Button<TMsg>>,
    pub close_sub: Handle<Button<TMsg>>,
    pub sub_stats: Handle<Label<TMsg>>,
    pub details: Handle<ListView<TMsg>>,
    pub pub_topic: Handle<Edit<TMsg>>,
    pub pub_payload: Handle<Edit<TMsg>>,
    pub pub_retained: Handle<CheckBox<TMsg>>,
    pub pub_go: Handle<Button<TMsg>>,
    pub pub_listen: Handle<Button<TMsg>>,
    pub status: Handle<StatusBar<TMsg>>,
}

/// What the tab strip says before anything is listened to.
pub const WELCOME_TAB: &str = "(none)";
pub const WELCOME_TEXT: &str =
    "Press Listen on a topic in the main window, or type a topic below and press Listen.";

impl TopicsWidgets {
    /// The whole window.
    pub fn layout(&self, notice: &Rc<Notice>) -> Layout<TMsg> {
        column().gap(0).children((
            menus().height(MENU_HEIGHT),
            strip(Rc::clone(notice), TMsg::NoticeDismiss).bind(&self.strip),
            column()
                .padding(6)
                .gap(4)
                .children((
                    self.filter_row(),
                    tabs()
                        .page(WELCOME_TAB, column().child(label(WELCOME_TEXT)))
                        .on_change(TMsg::SubTab)
                        .bind(&self.subs)
                        .fill(4),
                    self.control_row(),
                    label("Selected event").caption(),
                    list().column("details", Fill).bind(&self.details).fill(4),
                    label("Publish").caption(),
                    self.publish_form(),
                ))
                .fill(1),
            status_bar(&["Ready", ""]).bind(&self.status),
        ))
    }

    fn filter_row(&self) -> Layout<TMsg> {
        row().gap(4).children((
            edit()
                .placeholder("filter events")
                .on_change(TMsg::EventFilter)
                .bind(&self.sub_filter)
                .fill(1),
            button("Pause").on_click(TMsg::Pause).bind(&self.pause),
            button("Clear")
                .on_click(TMsg::ClearEvents)
                .bind(&self.clear),
            button("Pin").on_click(TMsg::PinEvent).bind(&self.pin),
        ))
    }

    fn control_row(&self) -> Layout<TMsg> {
        row().gap(4).children((
            button("Copy as Rhai")
                .on_click(TMsg::CopyEvent)
                .bind(&self.copy),
            button("Save").on_click(TMsg::SaveEvents).bind(&self.save),
            button("Stop listening")
                .on_click(TMsg::CloseSub)
                .bind(&self.close_sub),
            label("").bind(&self.sub_stats).fill(1),
        ))
    }

    fn publish_form(&self) -> Layout<TMsg> {
        column().gap(4).children((
            edit()
                .placeholder(
                    "topic, e.g. demo/pg/x (Listen takes a filter: + one level, # the rest)",
                )
                .on_change(TMsg::PubTopic)
                .bind(&self.pub_topic),
            row().gap(4).children((
                edit()
                    .placeholder("value: \"text\" or #{ field: 1 }")
                    .on_change(TMsg::PubPayload)
                    .bind(&self.pub_payload)
                    .fill(1),
                checkbox("retained")
                    .on_toggle(TMsg::PubRetained)
                    .bind(&self.pub_retained),
                button("Publish").on_click(TMsg::PubGo).bind(&self.pub_go),
                button("Listen")
                    .on_click(TMsg::PubListen)
                    .bind(&self.pub_listen),
            )),
        ))
    }
}
