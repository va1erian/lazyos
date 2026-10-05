//! The account page: the address and password, and the servers guessed from
//! the address (esMail's presets) or typed in. It also asks for the password of
//! a saved account at the start of a session (`secrets.rs`).

use std::rc::Rc;

use esmail::config::{AccountConfig, AuthKind, TlsMode};
use esmail_glue::account_form::{self, AccountForm, Field};

use xui_core::app::Ui;
use xui_core::arrange::{
    Align, Handle, LayoutExt, Track, button, column, edit, grid, label, panel, row,
};
use xui_core::icon::Lucide;
use xui_core::widget::{Edit, HasText, Label, Panel};

use super::Msg;

/// The handles [`page`] fills.
#[derive(Default)]
pub struct AccountWidgets {
    panel: Handle<Panel<Msg>>,
    title: Handle<Label<Msg>>,
    name: Handle<Edit<Msg>>,
    email: Handle<Edit<Msg>>,
    password: Handle<Edit<Msg>>,
    imap_host: Handle<Edit<Msg>>,
    imap_port: Handle<Edit<Msg>>,
    smtp_host: Handle<Edit<Msg>>,
    smtp_port: Handle<Edit<Msg>>,
    error: Handle<Label<Msg>>,
}

/// The page: a line saying what to do, the fields in a two-column grid, a
/// line for what was wrong and the buttons at the bottom right.
pub fn page(w: &AccountWidgets) -> impl LayoutExt<Msg> {
    let field = |caption: &str, cue: &str, handle: &Handle<Edit<Msg>>| {
        (
            label(caption).align_y(Align::Center),
            edit().placeholder(cue).bind(handle),
        )
    };
    let (name_l, name) = field("Name", "shown in the folder pane (optional)", &w.name);
    let (email_l, email) = field("Email address", "you@example.com", &w.email);
    let (password_l, password) = field(
        "Password",
        "an app password for Gmail, Outlook and iCloud",
        &w.password,
    );
    let (imap_l, imap) = field("IMAP server", "imap.example.com", &w.imap_host);
    let (imap_port_l, imap_port) = field("IMAP port", "993 (TLS)", &w.imap_port);
    let (smtp_l, smtp) = field("SMTP server", "smtp.example.com", &w.smtp_host);
    let (smtp_port_l, smtp_port) = field("SMTP port", "465 (TLS) or 587 (STARTTLS)", &w.smtp_port);
    panel(
        column().padding(16).gap(12).children((
            label("").bind(&w.title),
            grid([Track::Auto, Track::Fill(1)]).gap(8).children((
                name_l,
                name,
                email_l,
                email.on_change(Msg::EmailChanged),
                password_l,
                password.password(),
                imap_l,
                imap,
                imap_port_l,
                imap_port,
                smtp_l,
                smtp,
                smtp_port_l,
                smtp_port,
            )),
            label("").bind(&w.error),
            row().gap(8).justify(Align::End).children((
                button("Cancel").icon(Lucide::X).on_click(Msg::CloseAccount),
                button("Connect")
                    .icon(Lucide::Check)
                    .primary()
                    .on_click(Msg::SaveAccount),
            )),
        )),
    )
    .plain()
    .bind(&w.panel)
}

/// The form's widgets, hidden while the mail view is up.
pub struct AccountPage {
    panel: Rc<Panel<Msg>>,
    title: Rc<Label<Msg>>,
    name: Rc<Edit<Msg>>,
    email: Rc<Edit<Msg>>,
    password: Rc<Edit<Msg>>,
    imap_host: Rc<Edit<Msg>>,
    imap_port: Rc<Edit<Msg>>,
    smtp_host: Rc<Edit<Msg>>,
    smtp_port: Rc<Edit<Msg>>,
    error: Rc<Label<Msg>>,
    /// The saved account whose password is being asked for, if any.
    pub editing: Option<AccountConfig>,
    /// The servers the address last suggested. A changed address replaces
    /// the server fields only while they still hold these (or nothing), so
    /// servers the user typed are kept.
    suggested: (String, String),
}

impl AccountPage {
    /// The page [`page`] mounted.
    pub fn new(w: &AccountWidgets) -> AccountPage {
        AccountPage {
            panel: w.panel.get(),
            title: w.title.get(),
            name: w.name.get(),
            email: w.email.get(),
            password: w.password.get(),
            imap_host: w.imap_host.get(),
            imap_port: w.imap_port.get(),
            smtp_host: w.smtp_host.get(),
            smtp_port: w.smtp_port.get(),
            error: w.error.get(),
            editing: None,
            suggested: Default::default(),
        }
    }

    pub fn set_shown(&self, ui: &Ui<Msg>, shown: bool) {
        ui.set_visible(self.panel.id(), shown);
    }

    /// Opens the page empty, for a new account.
    pub fn start_new(&mut self, ui: &Ui<Msg>) {
        self.editing = None;
        self.fill(
            &AccountForm::default(),
            "Add a mail account. LazyOS connects over TLS and checks the server's certificate.",
        );
        self.suggested = Default::default();
        self.email.focus();
        self.set_shown(ui, true);
    }

    /// Opens the page for `account`, asking for its password.
    pub fn ask_password(&mut self, ui: &Ui<Msg>, account: AccountConfig, why: &str) {
        let form = AccountForm::from_account(&account, String::new());
        self.fill(
            &form,
            &format!(
                "{}: enter the password for this session. {why}",
                account.display_name
            ),
        );
        self.suggested = Default::default();
        self.editing = Some(account);
        self.password.focus();
        self.set_shown(ui, true);
    }

    fn fill(&self, form: &AccountForm, title: &str) {
        self.title.set_text(title);
        self.error.set_text("");
        self.name.set_text(&form.display_name);
        self.email.set_text(&form.email);
        self.password.set_text("");
        self.set_servers(form);
    }

    fn set_servers(&self, form: &AccountForm) {
        self.imap_host.set_text(&form.imap_host);
        self.imap_port.set_text(&form.imap_port);
        self.smtp_host.set_text(&form.smtp_host);
        self.smtp_port.set_text(&form.smtp_port);
    }

    /// The address changed: suggest its provider's servers, unless the user
    /// has typed their own.
    pub fn email_changed(&mut self, email: &str) {
        let current = (self.imap_host.text(), self.smtp_host.text());
        let untouched = current == self.suggested || (current.0.is_empty() && current.1.is_empty());
        if !untouched {
            return;
        }
        if let Some(detection) = account_form::detect(email) {
            let mut form = AccountForm::default();
            form.apply(&detection.preset);
            self.set_servers(&form);
            self.suggested = (form.imap_host, form.smtp_host);
        }
    }

    /// The form as typed. Sign-in is always by password: Google's OAuth flow
    /// needs a browser, so Gmail takes an app password.
    pub fn form(&self) -> AccountForm {
        let smtp_port = self.smtp_port.text();
        let smtp_tls = smtp_port
            .trim()
            .parse()
            .ok()
            .and_then(account_form::security_for_smtp_port)
            .unwrap_or(TlsMode::Ssl);
        AccountForm {
            display_name: self.name.text(),
            email: self.email.text(),
            auth: AuthKind::Password,
            password: self.password.text(),
            imap_host: self.imap_host.text(),
            imap_port: self.imap_port.text(),
            smtp_host: self.smtp_host.text(),
            smtp_port,
            smtp_tls,
        }
    }

    /// Shows why the form was refused and puts the cursor in the field to fix.
    pub fn show_error(&self, field: Option<Field>, message: &str) {
        self.error.set_text(message);
        let edit = match field {
            Some(Field::Email) => &self.email,
            Some(Field::Password) => &self.password,
            Some(Field::ImapHost) => &self.imap_host,
            Some(Field::ImapPort) => &self.imap_port,
            Some(Field::SmtpHost) => &self.smtp_host,
            Some(Field::SmtpPort) => &self.smtp_port,
            None => return,
        };
        edit.focus();
    }
}
