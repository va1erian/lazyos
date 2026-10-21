//! The main window's menu bar: ids, the message each raises and the menus.
//! The View menu's "Editor and output" is a check item, kept in step with the
//! bottom row by [`crate::panesapp`].

use xui_core::arrange::{menu_bar, Build};
use xui_core::widget::{Menu, MenuId};

use crate::msg::Msg;

/// The menu bar's design height.
pub const MENU_HEIGHT: i32 = 30;

// File menu.
pub const NEW: MenuId = MenuId::new(1);
pub const OPEN: MenuId = MenuId::new(2);
pub const SAVE: MenuId = MenuId::new(3);
pub const SAVE_AS: MenuId = MenuId::new(4);
pub const QUIT: MenuId = MenuId::new(5);
// Edit menu.
pub const UNDO: MenuId = MenuId::new(11);
pub const REDO: MenuId = MenuId::new(12);
pub const CUT: MenuId = MenuId::new(13);
pub const COPY: MenuId = MenuId::new(14);
pub const PASTE: MenuId = MenuId::new(15);
pub const SELECT_ALL: MenuId = MenuId::new(16);
// Run menu.
pub const RUN: MenuId = MenuId::new(21);
pub const RUN_SELECTION: MenuId = MenuId::new(22);
pub const STOP: MenuId = MenuId::new(23);
pub const STOP_ALL: MenuId = MenuId::new(24);
// View menu.
pub const VIEW_INTERFACES: MenuId = MenuId::new(31);
pub const VIEW_LIVE: MenuId = MenuId::new(32);
pub const VIEW_TOPICS: MenuId = MenuId::new(33);
pub const VIEW_GALLERY: MenuId = MenuId::new(34);
pub const REFRESH: MenuId = MenuId::new(35);
pub const VIEW_MONITOR: MenuId = MenuId::new(36);
pub const VIEW_NOTICE: MenuId = MenuId::new(37);
pub const VIEW_BOTTOM: MenuId = MenuId::new(38);
// Help menu.
pub const HELP: MenuId = MenuId::new(41);

fn menu_msg(id: MenuId) -> Option<Msg> {
    Some(match id {
        NEW => Msg::FileNew,
        OPEN => Msg::FileOpen,
        SAVE => Msg::FileSave,
        SAVE_AS => Msg::FileSaveAs,
        QUIT => Msg::Quit,
        UNDO => Msg::Undo,
        REDO => Msg::Redo,
        CUT => Msg::Cut,
        COPY => Msg::Copy,
        PASTE => Msg::Paste,
        SELECT_ALL => Msg::SelectAll,
        RUN => Msg::Run,
        RUN_SELECTION => Msg::RunSelection,
        STOP => Msg::Stop,
        STOP_ALL => Msg::StopAll,
        VIEW_INTERFACES => Msg::ExplorerTab(0),
        VIEW_LIVE => Msg::ExplorerTab(1),
        VIEW_TOPICS => Msg::ExplorerTab(2),
        VIEW_GALLERY => Msg::ExplorerTab(3),
        REFRESH => Msg::TopicsRefresh,
        VIEW_MONITOR => Msg::ShowTopics,
        VIEW_NOTICE => Msg::NoticeShow,
        VIEW_BOTTOM => Msg::ToggleBottom,
        HELP => Msg::ShowHelp,
        _ => return None,
    })
}

pub fn menus() -> Build<Menu<Msg>, Msg> {
    menu_bar(|bar| {
        bar.submenu(MenuId::new(100), "&File", |m| {
            m.item(NEW, "&New");
            m.item(OPEN, "&Open...");
            m.separator();
            m.item(SAVE, "&Save");
            m.item(SAVE_AS, "Save &As...");
            m.separator();
            m.item(QUIT, "&Quit");
        });
        bar.submenu(MenuId::new(200), "&Edit", |m| {
            m.item(UNDO, "&Undo");
            m.item(REDO, "&Redo");
            m.separator();
            m.item(CUT, "Cu&t");
            m.item(COPY, "&Copy");
            m.item(PASTE, "&Paste");
            m.item(SELECT_ALL, "Select &All");
        });
        bar.submenu(MenuId::new(300), "&Run", |m| {
            m.item(RUN, "&Run  F5");
            m.item(RUN_SELECTION, "Run &selection  Ctrl+Enter");
            m.item(STOP, "S&top  Shift+F5");
            m.item(STOP_ALL, "Stop &all");
        });
        bar.submenu(MenuId::new(400), "&View", |m| {
            m.item(VIEW_INTERFACES, "&Interfaces  Ctrl+1");
            m.item(VIEW_LIVE, "&Live  Ctrl+2");
            m.item(VIEW_TOPICS, "&Topics  Ctrl+3");
            m.item(VIEW_GALLERY, "&Gallery  Ctrl+4");
            m.separator();
            m.item(REFRESH, "&Refresh topics");
            m.separator();
            m.check(VIEW_BOTTOM, "Editor and &output  Ctrl+J", true);
            m.separator();
            m.item(VIEW_MONITOR, "Topic &monitor  Ctrl+T");
            m.item(VIEW_NOTICE, "Show developer-tool &notice  Ctrl+Shift+D");
        });
        bar.submenu(MenuId::new(500), "&Help", |m| {
            m.item(HELP, "&Help");
        });
    })
    .on_select_with(menu_msg)
}

