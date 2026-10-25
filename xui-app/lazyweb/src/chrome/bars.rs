//! The bars over the page: a site asking to notify, a page that does not answer (or a tab that
//! stopped), and the offer of the last session back after a crash. Each is the browser's own
//! chrome, hidden until the window shows it.

use xui_core::arrange::{button, group, label, row, Align, Handle, LayoutExt};
use xui_core::layout::Insets;
use xui_core::widget::Button;
use xui_core::Dip;

use super::Widgets;
use crate::app::Msg;
use crate::hang::Answer as HangAnswer;
use crate::notify::BarAnswer;

/// The bar a site's request to notify shows in, between the toolbar and the page.
pub(super) const NOTIFY_HEIGHT: Dip = Dip(64.0);
const NOTIFY_BUTTON: Dip = Dip(84.0);
/// The bar a page that stopped answering shows, in the same place.
pub(super) const HANG_HEIGHT: Dip = Dip(64.0);
/// The bar offering the last page back after a crash, in the same place.
pub(super) const RESTORE_HEIGHT: Dip = Dip(64.0);

/// The question a page's request to show notifications asks: whose it is, and Allow, Block or
/// "Not now" (the bar closes and the site may ask again). It is the browser's own bar, not a toast:
/// a toast never takes the keyboard and an answer must never need one.
pub(super) fn notify_bar(widgets: &Widgets) -> impl LayoutExt<Msg> {
    let answer = |text: &str, which, handle: &Handle<Button<Msg>>| {
        button(text)
            .on_click(Msg::NotifyAnswer(which))
            .bind(handle)
            .width(NOTIFY_BUTTON)
    };
    group(
        "Notifications",
        row()
            .gap(8)
            .align(Align::Center)
            .padding(Insets::symmetric(Dip(6.0), Dip(2.0)))
            .children((
                label("").bind(&widgets.notify_text).fill(1),
                answer("Allow", BarAnswer::Allow, &widgets.notify_allow),
                answer("Block", BarAnswer::Block, &widgets.notify_block),
                answer("Not now", BarAnswer::Later, &widgets.notify_later),
            )),
    )
    .bind(&widgets.notify_bar)
}

/// This page is not responding: Wait, Reload (ends the stuck process and loads the page again) or
/// Stop (ends it). The browser's own chrome, so a page that has stopped cannot take it with it.
pub(super) fn hang_bar(widgets: &Widgets) -> impl LayoutExt<Msg> {
    let answer = |text: &str, which, handle: &Handle<Button<Msg>>| {
        button(text)
            .on_click(Msg::Hang(which))
            .bind(handle)
            .width(NOTIFY_BUTTON)
    };
    group(
        "Page not responding",
        row()
            .gap(8)
            .align(Align::Center)
            .padding(Insets::symmetric(Dip(6.0), Dip(2.0)))
            .children((
                label("This page is not responding.")
                    .bind(&widgets.hang_text)
                    .fill(1),
                answer("Wait", HangAnswer::Wait, &widgets.hang_wait),
                answer("Reload", HangAnswer::Reload, &widgets.hang_reload),
                answer("Stop", HangAnswer::Stop, &widgets.hang_stop),
            )),
    )
    .bind(&widgets.hang_bar)
}

/// LazyWeb did not close properly: Restore brings the last page back, Dismiss keeps the start page.
pub(super) fn restore_bar(widgets: &Widgets) -> impl LayoutExt<Msg> {
    let answer = |text: &str, yes, handle: &Handle<Button<Msg>>| {
        button(text)
            .on_click(Msg::Restore(yes))
            .bind(handle)
            .width(NOTIFY_BUTTON)
    };
    group(
        "Restore",
        row()
            .gap(8)
            .align(Align::Center)
            .padding(Insets::symmetric(Dip(6.0), Dip(2.0)))
            .children((
                label("LazyWeb did not close properly. Restore the last page?")
                    .bind(&widgets.restore_text)
                    .fill(1),
                answer("Restore", true, &widgets.restore_yes),
                answer("Dismiss", false, &widgets.restore_no),
            )),
    )
    .bind(&widgets.restore_bar)
}
