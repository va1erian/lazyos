//! The window's chrome: the menu bar, the toolbar of icon buttons with the
//! address field and the throbber, the page, and the status bar with its
//! security badge and download progress.

use xui_core::app::Ui;
use xui_core::arrange::{
    build, button, column, edit, label, menu_bar, progress, row, Align, Handle, Layout, LayoutExt,
};
use xui_core::icon::Lucide;
use xui_core::layout::Insets;
use xui_core::widget::{Button, Edit, Label, Menu, MenuId, ProgressBar};
use xui_core::Dip;

use crate::app::Msg;
use crate::indicators::{Badge, Throbber};
use crate::page::Page;

/// Heights of the menu bar, the toolbar and the status bar.
const MENU_HEIGHT: Dip = Dip(28.0);
const TOOLBAR_HEIGHT: Dip = Dip(38.0);
const STATUS_HEIGHT: Dip = Dip(24.0);
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
    /// The context menu's: act on what was right-clicked.
    OpenLink,
    CopyLink,
    SaveImage,
}

/// The menu ids, one per command (and one per menu title).
const COMMANDS: [Command; 15] = [
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
    Command::OpenLink,
    Command::CopyLink,
    Command::SaveImage,
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

/// The widgets the window changes after building.
#[derive(Default)]
pub struct Widgets {
    pub menu: Handle<xui_core::widget::Menu<Msg>>,
    pub page: Handle<Page>,
    pub address: Handle<Edit<Msg>>,
    pub back: Handle<Button<Msg>>,
    pub forward: Handle<Button<Msg>>,
    /// Reload while idle, Stop while loading.
    pub reload: Handle<Button<Msg>>,
    pub throbber: Handle<Throbber>,
    pub status: Handle<Label<Msg>>,
    pub badge: Handle<Badge>,
    pub download_label: Handle<Label<Msg>>,
    pub download_bar: Handle<ProgressBar<Msg>>,
}

/// The whole window; the page opens `url` first.
pub fn window(widgets: &Widgets, url: String) -> Layout<Msg> {
    column().children((
        menus(widgets).fixed(MENU_HEIGHT),
        toolbar(widgets).fixed(TOOLBAR_HEIGHT),
        build(move |ui| Page::new(ui, &url))
            .bind(&widgets.page)
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
            item(m, Command::OpenLocation, "Open &Location...", Lucide::Link);
            item(m, Command::SavePage, "&Save Page As Download", Lucide::Save);
            m.separator();
            item(m, Command::Close, "&Close Window", Lucide::X);
        });
        bar.submenu(VIEW_MENU, "&View", |m| {
            item(m, Command::Reload, "&Reload", Lucide::RefreshCw);
            item(m, Command::Stop, "&Stop", Lucide::CircleX);
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
        bar.submenu(DOWNLOADS_MENU, "&Downloads", |m| {
            item(
                m,
                Command::ShowDownloads,
                "Show &Downloads",
                Lucide::Download,
            );
        });
        bar.submenu(HELP_MENU, "&Help", |m| {
            item(m, Command::About, "&About LazyWeb", Lucide::Info);
        });
    })
    .on_select_with(|id| Command::from_id(id).map(Msg::Menu))
    .bind(&widgets.menu)
}

/// The popup a right click on the page opens: the page's own commands, then
/// the ones for the link or picture under the pointer (`app.rs` enables
/// those only when there is one).
pub fn context_menu(ui: &Ui<Msg>) -> Menu<Msg> {
    Menu::context(ui)
        .build(|m| {
            m.item(Command::Back.id(), "&Back")
                .icon(Lucide::ChevronLeft);
            m.item(Command::Forward.id(), "&Forward")
                .icon(Lucide::ChevronRight);
            m.item(Command::Reload.id(), "&Reload")
                .icon(Lucide::RefreshCw);
            m.separator();
            m.item(Command::OpenLink.id(), "&Open Link")
                .icon(Lucide::ExternalLink);
            m.item(Command::CopyLink.id(), "Copy Link &Address")
                .icon(Lucide::Link);
            m.item(Command::SaveImage.id(), "&Save Image As...")
                .icon(Lucide::Download);
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
            tool(Lucide::ExternalLink, "Go (Enter)", Msg::Go).width(TOOL_SIDE),
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
