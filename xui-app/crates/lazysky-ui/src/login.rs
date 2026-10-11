//! The sign-in page: a handle, an app password and a button.

use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::arrange::{button, column, edit, label, Handle, LayoutExt, Mounted};
use xui_core::backend::{Result, WidgetId};
use xui_core::layout::Align;
use xui_core::widget::{Button, Edit, Label};
use xui_core::HasText;

use crate::Msg;

/// Width of the two fields.
const FIELD_W: i32 = 300;

pub struct LoginPage {
    handle: Rc<Edit<Msg>>,
    password: Rc<Edit<Msg>>,
    submit: Rc<Button<Msg>>,
    message: Rc<Label<Msg>>,
    _mounted: Mounted<Msg>,
}

impl LoginPage {
    /// Lays the page out in the container `page`.
    pub fn build(ui: &Ui<Msg>, page: WidgetId) -> Result<LoginPage> {
        let (handle, password, submit, message) =
            (Handle::new(), Handle::new(), Handle::new(), Handle::new());
        let mounted = ui.mount_in(
            page,
            column().padding(24).gap(10).align(Align::Center).children((
                label("LazySky").title(),
                label("Sign in with your handle and an app password."),
                edit()
                    .placeholder("handle.bsky.social")
                    .bind(&handle)
                    .width(FIELD_W),
                edit()
                    .password()
                    .placeholder("app password")
                    .bind(&password)
                    .width(FIELD_W),
                button("Sign in").on_click(Msg::SignInClicked).bind(&submit),
                label("").bind(&message),
                label("Create an app password in Bluesky: Settings, Privacy and security."),
            )),
        )?;
        Ok(LoginPage {
            handle: handle.get(),
            password: password.get(),
            submit: submit.get(),
            message: message.get(),
            _mounted: mounted,
        })
    }

    /// The typed handle and password. The password field is emptied: the
    /// secret lives in the request and nowhere else.
    pub fn take_credentials(&self) -> (String, String) {
        let handle = self.handle.text();
        let password = self.password.text();
        self.password.set_text("");
        (handle, password)
    }

    /// Puts `handle` in the handle field (a remembered one).
    pub fn set_handle(&self, handle: &str) {
        self.handle.set_text(handle);
    }

    /// Shows a line under the button.
    pub fn say(&self, line: &str) {
        self.message.set_text(line);
    }

    /// Disables the button while a sign-in is in flight.
    pub fn set_busy(&self, busy: bool) {
        self.submit.set_enabled(!busy);
    }

    /// Puts the cursor in the handle field.
    pub fn focus(&self) {
        self.handle.focus();
    }
}
