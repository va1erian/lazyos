//! The Mail window: a toolbar, the folder pane, the message list and the
//! reading pane, with a status line below. The compose form covers the list
//! and the reading pane; the account page covers everything under the toolbar.
//!
//! All network and disk work runs on esMail's tokio runtime (`esmail_glue::Core`).
//! Its sessions wake the window through a `Proxy` (`Msg::Wake`); the window
//! then drains what they reported (`mailbox.rs`) and never blocks.
//!
//! Serial evidence for the sessions (`tools/screenshot/examples/mail_*.json`):
//! `MAIL:UP:PASS` (main.rs), `MAIL:CORE:ACCOUNTS=<n>`, `MAIL:CONNECTED:<account>`,
//! `MAIL:FOLDERS:<account>:<n>`, `MAIL:HEADERS:<mailbox>:<n>`,
//! `MAIL:BODY:PASS:<uid>`, `MAIL:RENDER:PASS`, `MAIL:SEND:PASS|FAIL`,
//! `MAIL:ACCOUNT:SAVED|FAIL` and `MAIL:ERROR:<text>`. None carries a password.

mod account;
mod compose;
mod layout;
mod mailbox;
mod models;
mod reader;
#[cfg(test)]
mod window_tests;

use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Arc;

use esmail::auth::Auth;
use esmail::config::Config;
use esmail::imap::MailHeader;
use esmail_glue::account_setup;
use esmail_glue::compose::{self as compose_kind, Kind};
use esmail_glue::mailbox::OpenFolder;
use esmail_glue::{BodyLoads, Core, FolderRef, FolderTree};
use secrecy::SecretString;
use xui_core::app::{App, Ui};
use xui_core::backend::Result;
use xui_core::widget::{HasText, Label, ListView, Panel};

use account::AccountPage;
use compose::ComposePage;
use layout::Widgets;
use models::{FolderRows, MessageRows};
use reader::Reader;

/// `(account, mailbox, uid)`: which message a body fetch is for.
type BodyKey = (usize, String, u32);

/// Everything the window reacts to.
#[derive(Clone)]
pub enum Msg {
    /// An account session or the cache has something to apply.
    Wake,
    /// The reading pane finished a layout pass.
    Frame,
    /// The poll for the reading pane's first paint of a message.
    Tick,
    Link(String),
    Copy(String),
    /// A row of the folder pane was picked.
    Folder(usize),
    /// A row of the message list was picked.
    Message(usize),
    GetMail,
    Compose,
    Reply,
    Accounts,
    Send,
    CloseCompose,
    SaveAccount,
    CloseAccount,
    EmailChanged(String),
    /// New mail arrived in a watched folder (the text of the notification).
    NewMail(String),
}

pub struct Mail {
    config: Config,
    /// `None` until an account is configured.
    core: Option<Core>,
    folders: FolderTree,
    folder_rows: FolderRows,
    folder_list: Rc<ListView<Msg>>,
    message_list: Rc<ListView<Msg>>,
    reader: Rc<Reader>,
    account_page: AccountPage,
    compose_page: ComposePage,
    status: Rc<Label<Msg>>,
    /// The folder pane and everything beside it; the account page takes its
    /// place.
    mail_view: Rc<Panel<Msg>>,
    /// The message list and the reading pane; the compose form takes their
    /// place.
    panes: Rc<Panel<Msg>>,
    /// The folder on show, its pages and the requests in flight.
    open: Option<OpenFolder>,
    /// The message on show and the folder it is in.
    selected: Option<(FolderRef, MailHeader)>,
    /// The selected message's body, quoted by Reply.
    selected_body: String,
    bodies: BodyLoads<BodyKey>,
    /// Accounts whose password this session still has to ask for.
    password_queue: VecDeque<(usize, String)>,
    /// Whether the account page covers the panes (see `cover`).
    covered: bool,
    next_compose: u64,
}

impl Mail {
    pub fn build(ui: &mut Ui<Msg>, config: Config) -> Result<Mail> {
        let widgets = Widgets::default();
        ui.root(layout::window(&widgets))?;
        let mounted = widgets.mounted();
        let account_page = AccountPage::new(&widgets.account);
        account_page.set_shown(ui, false);
        let mut compose_page = ComposePage::new(&widgets.compose);
        compose_page.close(ui);
        let tick = ui.set_timer(100);
        ui.on_timer(move |id| (id == tick).then_some(Msg::Tick));
        let mut mail = Mail {
            config,
            core: None,
            folders: FolderTree::default(),
            folder_rows: FolderRows::default(),
            folder_list: mounted.folder_list,
            message_list: mounted.message_list,
            reader: mounted.reader,
            account_page,
            compose_page,
            status: mounted.status,
            mail_view: mounted.mail_view,
            panes: mounted.panes,
            open: None,
            selected: None,
            selected_body: String::new(),
            bodies: BodyLoads::default(),
            password_queue: VecDeque::new(),
            covered: false,
            next_compose: 0,
        };
        mail.start_core(ui);
        Ok(mail)
    }

    /// (Re)starts the mail core for the configured accounts, forgetting what
    /// the previous one showed.
    fn start_core(&mut self, ui: &Ui<Msg>) {
        self.core = None;
        self.open = None;
        self.selected = None;
        self.bodies.cancel();
        self.message_list.set_model(MessageRows::new(&[]));
        self.folders = FolderTree::new(
            self.config
                .accounts
                .iter()
                .map(|account| account.display_name.clone()),
        );
        self.refresh_folders();
        println!("MAIL:CORE:ACCOUNTS={}", self.config.accounts.len());
        if self.config.accounts.is_empty() {
            self.reader.show_notice("No mail account is set up yet.");
            self.account_page.start_new(ui);
            self.cover(ui, true);
            return self.set_status("Add an account to start.");
        }
        let proxy = ui.proxy();
        let waker: esmail::waker::Waker = Arc::new(move || {
            let _ = proxy.send(Msg::Wake);
        });
        let proxy = ui.proxy();
        let notify: esmail::session::NotifyFn =
            Arc::new(move |title: &str, _body: &str, _account: &str| {
                let _ = proxy.send(Msg::NewMail(title.to_owned()));
            });
        match Core::start(&self.config, waker, notify) {
            Ok((core, issues)) => {
                for account in 0..core.accounts().len() {
                    core.cache().load_mailboxes(account);
                }
                self.core = Some(core);
                self.password_queue = issues
                    .into_iter()
                    .map(|issue| (issue.account, issue.message))
                    .collect();
                self.reader.show_notice("Select a folder, then a message.");
                self.set_status("Connecting...");
                self.ask_next_password(ui);
            }
            Err(error) => self.error(&format!("Could not start the mail core: {error}")),
        }
    }

    /// Asks for the next account's password, if any is still missing.
    fn ask_next_password(&mut self, ui: &Ui<Msg>) {
        let Some((account, why)) = self.password_queue.pop_front() else {
            return;
        };
        let Some(config) = self.config.accounts.get(account).cloned() else {
            return;
        };
        self.account_page
            .ask_password(ui, config, &format!("({why})"));
        self.cover(ui, true);
    }

    /// Shows the account page in place of the mail view, or the mail view
    /// again; the compose form keeps the list and the reader hidden while it
    /// is open.
    fn cover(&mut self, ui: &Ui<Msg>, covered: bool) {
        self.covered = covered;
        ui.set_visible(self.mail_view.id(), !covered);
        ui.set_visible(self.panes.id(), !self.compose_page.is_open());
    }

    fn set_status(&self, text: &str) {
        self.status.set_text(text);
    }

    fn error(&self, text: &str) {
        println!("MAIL:ERROR:{text}");
        self.set_status(&format!("Error: {text}"));
    }

    fn refresh_folders(&mut self) {
        self.folder_rows = FolderRows::from_tree(&self.folders);
        let selected = self
            .open
            .as_ref()
            .and_then(|open| self.folder_rows.row_of(open.folder()));
        self.folder_list.set_model(self.folder_rows.clone());
        self.folder_list.select(selected);
    }

    /// The account page's Connect: saves the account (its password only for
    /// this session) and restarts the core with it.
    fn save_account(&mut self, ui: &Ui<Msg>) {
        let form = self.account_page.form();
        let account = match form.to_account(self.account_page.editing.as_ref()) {
            Ok(account) => account,
            Err(error) => {
                return self
                    .account_page
                    .show_error(Some(error.field), error.message);
            }
        };
        let auth = Auth::Password(SecretString::from(form.password));
        match account_setup::save_account(&self.config, account, &auth) {
            Ok(config) => {
                println!("MAIL:ACCOUNT:SAVED");
                self.config = config;
                self.account_page.set_shown(ui, false);
                self.cover(ui, false);
                if self.password_queue.is_empty() {
                    self.start_core(ui);
                } else {
                    self.ask_next_password(ui);
                }
            }
            Err(error) => {
                println!("MAIL:ACCOUNT:FAIL");
                self.account_page.show_error(None, &error);
            }
        }
    }

    fn close_account(&mut self, ui: &Ui<Msg>) {
        self.account_page.set_shown(ui, false);
        self.cover(ui, false);
        if self.password_queue.is_empty() {
            self.set_status("Ready");
        } else {
            self.ask_next_password(ui);
        }
    }

    fn open_compose(&mut self, ui: &Ui<Msg>, kind: Kind) {
        let account = match (&self.selected, kind) {
            (Some((folder, _)), _) => folder.account,
            (None, Kind::New) => self.open.as_ref().map_or(0, |open| open.folder().account),
            (None, _) => return self.set_status("Select a message to reply to."),
        };
        let Some(config) = self.config.accounts.get(account) else {
            return self.set_status("Add an account first.");
        };
        let original = self
            .selected
            .as_ref()
            .map(|(_, header)| (header, self.selected_body.as_str()));
        let state =
            compose_kind::initial_state(kind, original.filter(|_| kind.needs_original()), config);
        ui.set_visible(self.panes.id(), false);
        self.compose_page.open(ui, account, &config.username, state);
    }

    /// Closes the form and shows the panes again, unless the account page
    /// covers them; a password asked for while composing is asked now.
    fn close_compose(&mut self, ui: &Ui<Msg>) {
        self.compose_page.close(ui);
        if self.covered {
            return;
        }
        if self.password_queue.is_empty() {
            self.cover(ui, false);
        } else {
            self.ask_next_password(ui);
        }
    }

    fn send(&mut self, ui: &Ui<Msg>) {
        let (Some(core), Some((account, state))) =
            (self.core.as_ref(), self.compose_page.message())
        else {
            return;
        };
        self.next_compose += 1;
        match core.send_mail(self.next_compose, account, state) {
            Ok(()) => {
                self.compose_page.set_sending(ui, true);
                self.set_status("Sending...");
            }
            Err(error) => {
                println!("MAIL:SEND:FAIL");
                self.set_status(&error);
            }
        }
    }
}

impl App for Mail {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            Msg::Wake => self.drain(ui),
            Msg::Frame => self.reader.frame(),
            Msg::Tick => {
                if self.reader.first_paint() {
                    println!("MAIL:RENDER:PASS");
                }
            }
            // Links are not followed: there is no browser to hand them to yet.
            Msg::Link(href) => self.set_status(&format!("Link: {href}")),
            Msg::Copy(text) => ui.set_clipboard_text(&text),
            Msg::Folder(row) => {
                if let Some(Some(folder)) = self.folder_rows.targets.get(row).cloned() {
                    self.open_folder(folder);
                }
            }
            Msg::Message(row) => self.select_message(row),
            Msg::GetMail => self.refresh(),
            Msg::Compose => self.open_compose(ui, Kind::New),
            Msg::Reply => self.open_compose(ui, Kind::Reply),
            Msg::Accounts => {
                self.account_page.start_new(ui);
                self.cover(ui, true);
            }
            Msg::Send => self.send(ui),
            Msg::CloseCompose => self.close_compose(ui),
            Msg::SaveAccount => self.save_account(ui),
            Msg::CloseAccount => self.close_account(ui),
            Msg::EmailChanged(email) => self.account_page.email_changed(&email),
            Msg::NewMail(text) => self.set_status(&text),
        }
    }
}
