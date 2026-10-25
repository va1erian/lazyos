//! The window's chrome: the menu bar, the toolbar of icon buttons with the
//! address field and the throbber, the page, and the status bar with its
//! security badge and download progress.

use std::cell::Cell;
use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::arrange::{
    build, button, column, edit, label, menu_bar, progress, row, stack, toggle_button, Align,
    Handle, Layout, LayoutExt,
};
use xui_core::icon::Lucide;
use xui_core::layout::Insets;
use xui_core::widget::{Button, Edit, GroupBox, Label, Menu, MenuId, ProgressBar};
use xui_core::Dip;

use xui_webkit::engine::Engine;
use xui_webkit::view::{Hooks, PageView};

use crate::app::Msg;
use crate::bookmarks::{self, BookmarkMsg};
use crate::indicators::{Badge, Throbber};
use crate::keys;
use crate::strip::{self, TabMsg, TabStrip};
use crate::suggest::widget::SuggestBox;

mod bars;
use bars::{hang_bar, notify_bar, restore_bar, HANG_HEIGHT, NOTIFY_HEIGHT, RESTORE_HEIGHT};

/// Heights of the menu bar, the toolbar and the status bar.
const MENU_HEIGHT: Dip = Dip(28.0);
const TOOLBAR_HEIGHT: Dip = Dip(38.0);
const STATUS_HEIGHT: Dip = Dip(24.0);
/// Where the suggestions start: under the address field, past the four buttons.
const SUGGEST_LEFT: Dip = Dip(138.0);
/// The square icon buttons of the toolbar, and the download progress bar.
const TOOL_SIDE: Dip = Dip(30.0);
const PROGRESS_WIDTH: Dip = Dip(120.0);
const PROGRESS_HEIGHT: Dip = Dip(12.0);

/// Every menu command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    OpenLocation,
    SavePage,
    Close,
    Back,
    Forward,
    Reload,
    Stop,
    Home,
    ShowHistory,
    ClearHistory,
    ShowDownloads,
    About,
    /// `about:blocker` (the toolbar's shield and the View menu).
    Blocker,
    /// Reload the page once with the ad blocker off for it.
    ReloadUnblocked,
    /// Keep the page's data in memory from the next start, or stop (`memory.rs`).
    PageDataInMemory,
    /// The link menu's: act on the link that was right-clicked.
    OpenLinkInApp,
    CopyLink,
    /// The Bookmarks menu's.
    BookmarkPage,
    EditBookmark,
    ShowBookmarks,
    /// Help > Report an issue.
    ReportIssue,
    /// The tab commands of the File menu and the link menu.
    NewTab,
    CloseTab,
    ReopenTab,
    OpenLinkInTab,
}

/// The menu ids, one per command (and one per menu title).
const COMMANDS: [Command; 25] = [
    Command::OpenLocation,
    Command::SavePage,
    Command::Close,
    Command::Back,
    Command::Forward,
    Command::Reload,
    Command::Stop,
    Command::Home,
    Command::ShowHistory,
    Command::ClearHistory,
    Command::ShowDownloads,
    Command::About,
    Command::Blocker,
    Command::ReloadUnblocked,
    Command::PageDataInMemory,
    Command::OpenLinkInApp,
    Command::CopyLink,
    Command::BookmarkPage,
    Command::EditBookmark,
    Command::ShowBookmarks,
    Command::ReportIssue,
    Command::NewTab,
    Command::CloseTab,
    Command::ReopenTab,
    Command::OpenLinkInTab,
];

impl Command {
    pub fn id(self) -> MenuId {
        let index = COMMANDS.iter().position(|c| *c == self).unwrap_or(0);
        MenuId::new(index + 1)
    }

    pub fn from_id(id: MenuId) -> Option<Command> {
        COMMANDS.iter().copied().find(|c| c.id() == id)
    }
}

/// Menu titles' ids, outside the commands' range.
const FILE_MENU: MenuId = MenuId::new(100);
const VIEW_MENU: MenuId = MenuId::new(101);
const HISTORY_MENU: MenuId = MenuId::new(102);
const DOWNLOADS_MENU: MenuId = MenuId::new(103);
const HELP_MENU: MenuId = MenuId::new(104);
const BOOKMARKS_MENU: MenuId = MenuId::new(105);
const AGENT_MENU: MenuId = MenuId::new(106);
/// The user agent items' ids: one per preset from here up.
const AGENT_BASE: usize = 200;

/// The View menu's radio item for user agent preset `index`.
pub fn agent_id(index: usize) -> MenuId {
    MenuId::new(AGENT_BASE + index)
}

/// The preset a View menu item picks, if it is one of the user agents'.
fn agent_of(id: MenuId) -> Option<usize> {
    (0..crate::useragent::PRESETS.len()).find(|&index| agent_id(index) == id)
}

/// The widgets the window changes after building.
#[derive(Default)]
pub struct Widgets {
    pub bookmarks: crate::bookmarks::Widgets,
    pub report: crate::report::form::Handles,
    /// Whether a link with a menu of its own is under the pointer: a right click then belongs to
    /// the window's link menu, not the page (`context::link_items`).
    pub link_hover: Rc<Cell<bool>>,
    pub menu: Handle<xui_core::widget::Menu<Msg>>,
    pub page: Handle<PageView<Msg>>,
    pub address: Handle<Edit<Msg>>,
    /// The history suggestions under the address field, drawn over the page.
    pub suggest: Handle<SuggestBox>,
    pub back: Handle<Button<Msg>>,
    pub forward: Handle<Button<Msg>>,
    /// Reload while idle, Stop while loading.
    pub reload: Handle<Button<Msg>>,
    pub throbber: Handle<Throbber>,
    /// The tab strip above the toolbar.
    pub strip: Handle<TabStrip>,
    /// Whether a link a tab can open (a web address) is under the pointer: a middle click or
    /// Ctrl+click on it is the window's (`Hooks::open_link`).
    pub tab_link: Rc<Cell<bool>>,
    /// The ad blocker's button: its icon and tooltip say what it does for this page.
    pub shield: Handle<Button<Msg>>,
    pub status: Handle<Label<Msg>>,
    pub badge: Handle<Badge>,
    pub download_label: Handle<Label<Msg>>,
    pub download_bar: Handle<ProgressBar<Msg>>,
    /// The permission bar (hidden until a page asks to show notifications) and its parts.
    pub notify_bar: Handle<GroupBox<Msg>>,
    pub notify_text: Handle<Label<Msg>>,
    pub notify_allow: Handle<Button<Msg>>,
    pub notify_block: Handle<Button<Msg>>,
    pub notify_later: Handle<Button<Msg>>,
    /// The bar of a page that does not answer (hidden until WebKit says so) and its parts.
    pub hang_bar: Handle<GroupBox<Msg>>,
    pub hang_text: Handle<Label<Msg>>,
    pub hang_wait: Handle<Button<Msg>>,
    pub hang_reload: Handle<Button<Msg>>,
    pub hang_stop: Handle<Button<Msg>>,
    /// The bar that offers the last page back after a crash (hidden unless the last run left the
    /// marker) and its parts.
    pub restore_bar: Handle<GroupBox<Msg>>,
    pub restore_text: Handle<Label<Msg>>,
    pub restore_yes: Handle<Button<Msg>>,
    pub restore_no: Handle<Button<Msg>>,
}

/// The whole window. `loading` is whether a page loads (Escape means Stop only then, so the page
/// otherwise gets it).
pub fn window(widgets: &Widgets, engine: Rc<dyn Engine>, loading: Rc<Cell<bool>>) -> Layout<Msg> {
    let link_hover = Rc::clone(&widgets.link_hover);
    let tab_link = Rc::clone(&widgets.tab_link);
    column().children((
        menus(widgets).fixed(MENU_HEIGHT),
        build(TabStrip::new)
            .bind(&widgets.strip)
            .fixed(strip::HEIGHT),
        toolbar(widgets).fixed(TOOLBAR_HEIGHT),
        notify_bar(widgets).fixed(NOTIFY_HEIGHT),
        hang_bar(widgets).fixed(HANG_HEIGHT),
        restore_bar(widgets).fixed(RESTORE_HEIGHT),
        // The suggestions, the bookmark editor and the report form float over the page.
        stack()
            .children((
                build(move |ui| {
                    let link_hover = Rc::clone(&link_hover);
                    let hooks = Hooks {
                        is_shortcut: Rc::new(move |key, mods| {
                            keys::is_window_key(key, mods, loading.get())
                        }),
                        clicked: Msg::PageClicked,
                        right_click: Some(Rc::new(Msg::PageMenu)),
                        swallow_right: Some(Rc::new(move || link_hover.get())),
                        open_link: Some(Rc::new(move |button, mods| {
                            let opens = button == xui_core::MouseButton::Middle
                                || (button == xui_core::MouseButton::Left && mods.ctrl);
                            (opens && tab_link.get()).then_some(Msg::Tab(TabMsg::OpenHovered))
                        })),
                    };
                    PageView::new(ui, engine, hooks)
                })
                .bind(&widgets.page),
                row()
                    .padding(Insets::new(SUGGEST_LEFT, Dip(0.0), Dip(0.0), Dip(0.0)))
                    .child(build(SuggestBox::new).bind(&widgets.suggest))
                    .align(Align::Start),
                bookmarks::editor::layout(&widgets.bookmarks.editor),
                crate::report::form::layout(&widgets.report),
            ))
            .fill(1),
        status_bar(widgets).fixed(STATUS_HEIGHT),
    ))
}

fn menus(widgets: &Widgets) -> impl LayoutExt<Msg> {
    let item = |menu: &mut xui_core::widget::MenuScope<'_>, command: Command, text, icon| {
        menu.item(command.id(), text).icon(icon);
    };
    menu_bar(move |bar| {
        bar.submenu(FILE_MENU, "&File", |m| {
            item(m, Command::NewTab, "&New Tab", Lucide::Plus);
            item(m, Command::CloseTab, "Close &Tab", Lucide::X);
            item(m, Command::ReopenTab, "&Reopen Closed Tab", Lucide::History);
            m.separator();
            item(m, Command::OpenLocation, "Open &Location...", Lucide::Link);
            item(m, Command::SavePage, "&Save Page As Download", Lucide::Save);
            m.separator();
            item(
                m,
                Command::PageDataInMemory,
                "Keep Page Data in &Memory (on/off)",
                Lucide::Lock,
            );
            m.separator();
            item(m, Command::Close, "&Close Window", Lucide::X);
        });
        bar.submenu(VIEW_MENU, "&View", |m| {
            item(m, Command::Reload, "&Reload", Lucide::RefreshCw);
            item(m, Command::Stop, "&Stop", Lucide::CircleX);
            m.separator();
            item(m, Command::Blocker, "Ad &Blocker...", Lucide::EyeOff);
            item(
                m,
                Command::ReloadUnblocked,
                "Reload Without &Blocking",
                Lucide::Eye,
            );
            m.separator();
            m.submenu(AGENT_MENU, "&User Agent", |m| {
                for (index, preset) in crate::useragent::PRESETS.iter().enumerate() {
                    m.radio(agent_id(index), preset.name, index == 0);
                }
            });
        });
        bar.submenu(HISTORY_MENU, "Hi&story", |m| {
            item(m, Command::Back, "&Back", Lucide::ChevronLeft);
            item(m, Command::Forward, "&Forward", Lucide::ChevronRight);
            item(m, Command::Home, "&Home", Lucide::Home);
            m.separator();
            item(
                m,
                Command::ShowHistory,
                "Show All &History",
                Lucide::History,
            );
            item(m, Command::ClearHistory, "&Clear History", Lucide::Trash2);
        });
        bar.submenu(BOOKMARKS_MENU, "&Bookmarks", |m| {
            item(
                m,
                Command::BookmarkPage,
                "Bookmark This &Page",
                Lucide::Star,
            );
            item(
                m,
                Command::EditBookmark,
                "&Edit This Bookmark...",
                Lucide::Pencil,
            );
            m.separator();
            item(
                m,
                Command::ShowBookmarks,
                "&Show Bookmarks...",
                Lucide::Folder,
            );
        });
        bar.submenu(DOWNLOADS_MENU, "&Downloads", |m| {
            item(
                m,
                Command::ShowDownloads,
                "Show &Downloads",
                Lucide::Download,
            );
        });
        bar.submenu(HELP_MENU, "&Help", |m| {
            item(m, Command::ReportIssue, "&Report an Issue...", Lucide::Bug);
            m.separator();
            item(m, Command::About, "&About LazyWeb", Lucide::Info);
        });
    })
    .on_select_with(|id| Command::from_id(id).map(Msg::Menu))
    .on_toggle(|id, on| Msg::UserAgent(agent_of(id).filter(|_| on)))
    .bind(&widgets.menu)
}

/// The popup a right click on bare page opens: the page's own commands (`context.rs`).
pub fn context_menu(ui: &Ui<Msg>) -> Menu<Msg> {
    Menu::context(ui)
        .build(|m| {
            m.item(Command::Back.id(), "&Back")
                .icon(Lucide::ChevronLeft);
            m.item(Command::Forward.id(), "&Forward")
                .icon(Lucide::ChevronRight);
            m.item(Command::Reload.id(), "&Reload")
                .icon(Lucide::RefreshCw);
        })
        .on_select(|id| Command::from_id(id).map(Msg::Menu))
}

/// The popup a right click on a link opens. Opening a link in a new tab goes first when tabs
/// exist.
pub fn link_menu(ui: &Ui<Msg>) -> Menu<Msg> {
    Menu::context(ui)
        .build(|m| {
            m.item(Command::OpenLinkInTab.id(), "Open Link in New &Tab")
                .icon(Lucide::Plus);
            m.item(Command::CopyLink.id(), "Copy Link &Address")
                .icon(Lucide::Link);
            m.item(Command::OpenLinkInApp.id(), "Open Link in &Another App")
                .icon(Lucide::ExternalLink);
        })
        .on_select(|id| Command::from_id(id).map(Msg::Menu))
}

/// An icon button with a tooltip.
fn tool(icon: Lucide, tip: &str, msg: Msg) -> xui_core::arrange::Build<Button<Msg>, Msg> {
    button("").icon(icon).tooltip(tip).on_click(msg)
}

/// Back, Forward, Reload/Stop and Home, the address field, Go and the
/// throbber.
fn toolbar(widgets: &Widgets) -> Layout<Msg> {
    row()
        .gap(6)
        .align(Align::Center)
        .padding(Insets::symmetric(Dip(6.0), Dip(4.0)))
        .children((
            row().gap(2).align(Align::Center).children((
                tool(
                    Lucide::ChevronLeft,
                    "Back (Alt+Left)",
                    Msg::Menu(Command::Back),
                )
                .bind(&widgets.back)
                .width(TOOL_SIDE),
                tool(
                    Lucide::ChevronRight,
                    "Forward (Alt+Right)",
                    Msg::Menu(Command::Forward),
                )
                .bind(&widgets.forward)
                .width(TOOL_SIDE),
                tool(Lucide::RefreshCw, "Reload (F5)", Msg::ReloadOrStop)
                    .bind(&widgets.reload)
                    .width(TOOL_SIDE),
                tool(Lucide::Home, "Home", Msg::Menu(Command::Home)).width(TOOL_SIDE),
            )),
            edit()
                .bind(&widgets.address)
                .placeholder("Type an address and press Enter")
                .on_change(|_| Msg::AddressEdited)
                .fill(1),
            toggle_button("")
                .icon(Lucide::Star)
                .tooltip("Bookmark this page (Ctrl+D)")
                .on_toggle(|_| Msg::Bookmarks(BookmarkMsg::Star))
                .bind(&widgets.bookmarks.star)
                .width(TOOL_SIDE),
            tool(Lucide::ExternalLink, "Go (Enter)", Msg::Go).width(TOOL_SIDE),
            tool(Lucide::EyeOff, "Ad blocker", Msg::Menu(Command::Blocker))
                .bind(&widgets.shield)
                .width(TOOL_SIDE),
            build(Throbber::new).bind(&widgets.throbber),
        ))
}

/// The status text taking the rest, a download's progress, and the badge.
fn status_bar(widgets: &Widgets) -> Layout<Msg> {
    row()
        .gap(8)
        .align(Align::Center)
        .padding(Insets::new(Dip(8.0), Dip(2.0), Dip(8.0), Dip(2.0)))
        .children((
            label("").bind(&widgets.status).fill(1),
            label("").bind(&widgets.download_label),
            progress(100)
                .bind(&widgets.download_bar)
                .size(PROGRESS_WIDTH, PROGRESS_HEIGHT),
            build(Badge::new).bind(&widgets.badge),
        ))
}
