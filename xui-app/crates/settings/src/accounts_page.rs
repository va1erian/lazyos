//! The Accounts page (docs/accounts-plan.md U1): the accounts of this
//! computer, adding and removing one, making one an administrator, setting
//! another account's password, and changing your own.
//!
//! Changing your own password needs only your current one. Adding, removing,
//! promoting and setting someone else's password are privileged: they go
//! through [`Accounts`] to `elevd`, which shows the trusted prompt where an
//! administrator approves (U2). A removed account's home is archived, never
//! deleted outright. Every new password is typed twice and follows
//! `accountdb::secret`'s rule; the rules live in [`accounts_ops`].

use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::arrange::{
    button, checkbox, column, edit, label, row, Build, Handle, LayoutExt, Mounted,
};
use xui_core::backend::{Result, WidgetId};
use xui_core::layout::Align;
use xui_core::widget::{Button, CheckBox, Edit, ListView};
use xui_core::HasText;

use crate::accounts::{Account, Accounts};
use crate::accounts_ops::{self, Outcome};
use crate::app::{choice_list, Msg};

/// Width of a name or password field: three and a button fit a row.
const FIELD_W: i32 = 112;

/// Messages the Accounts page's widgets raise.
#[derive(Clone, Debug, PartialEq)]
pub enum AccountsMsg {
    /// An account row was selected.
    Select(usize),
    /// Make the selected account an administrator (`true`) or not.
    Admin(bool),
    Remove,
    /// Set the selected account's password (an administrator approves).
    SetPassword,
    Add,
    /// Change your own password.
    Password,
}

/// A button raising `msg`.
fn command(text: &str, msg: AccountsMsg) -> Build<Button<Msg>, Msg> {
    button(text).on_click(Msg::Accounts(msg))
}

/// The page's password fields, each typed twice where it is new.
struct Secrets {
    /// The selected account's new password.
    set: Rc<Edit<Msg>>,
    set_again: Rc<Edit<Msg>>,
    /// A new account's.
    add: Rc<Edit<Msg>>,
    add_again: Rc<Edit<Msg>>,
    /// Yours: current, new, new again.
    old: Rc<Edit<Msg>>,
    new: Rc<Edit<Msg>>,
    confirm: Rc<Edit<Msg>>,
}

/// The page's widgets and the accounts listed.
pub struct AccountsPage {
    list: Rc<ListView<Msg>>,
    name: Rc<Edit<Msg>>,
    admin: Rc<CheckBox<Msg>>,
    secrets: Secrets,
    _mounted: Mounted<Msg>,
    accounts: Vec<Account>,
}

impl AccountsPage {
    /// Lays the page out in the container `page`.
    pub fn build(ui: &Ui<Msg>, page: WidgetId) -> Result<AccountsPage> {
        let (list, name, admin) = (Handle::new(), Handle::new(), Handle::new());
        let h: [Handle<Edit<Msg>>; 7] = std::array::from_fn(|_| Handle::new());
        let secret = |hint: &str, handle: &Handle<Edit<Msg>>| {
            edit()
                .password()
                .placeholder(hint)
                .bind(handle)
                .width(FIELD_W)
        };
        let mounted = ui.mount_in(
            page,
            column().padding(12).gap(6).children((
                label("Accounts on this computer"),
                choice_list(&[])
                    .on_select(|i| Msg::Accounts(AccountsMsg::Select(i)))
                    .bind(&list)
                    .size(440, 96)
                    .align(Align::Start),
                row().gap(6).children((
                    command("Make administrator", AccountsMsg::Admin(true)),
                    command("Remove administrator", AccountsMsg::Admin(false)),
                    command("Remove", AccountsMsg::Remove),
                )),
                label("Set the selected account's password"),
                row().gap(6).children((
                    secret("new password", &h[0]),
                    secret("again", &h[1]),
                    command("Set password", AccountsMsg::SetPassword),
                )),
                label("Add an account"),
                row().gap(6).children((
                    edit().placeholder("name").bind(&name).width(FIELD_W),
                    secret("password", &h[2]),
                    secret("again", &h[3]),
                )),
                row().gap(6).children((
                    checkbox("Administrator").bind(&admin),
                    command("Add", AccountsMsg::Add),
                )),
                label("Change your password"),
                row().gap(6).children((
                    secret("current", &h[4]),
                    secret("new", &h[5]),
                    secret("new again", &h[6]),
                    command("Change", AccountsMsg::Password),
                )),
                label("All but your own password ask an administrator."),
            )),
        )?;
        let [set, set_again, add, add_again, old, new, confirm] = h.map(|handle| handle.get());
        Ok(AccountsPage {
            list: list.get(),
            name: name.get(),
            admin: admin.get(),
            secrets: Secrets {
                set,
                set_again,
                add,
                add_again,
                old,
                new,
                confirm,
            },
            _mounted: mounted,
            accounts: Vec::new(),
        })
    }

    /// Re-read the accounts; returns a status text when they cannot be read.
    pub fn load(&mut self, accounts: &dyn Accounts) -> String {
        let selected = self.selected().map(|account| account.name.clone());
        let (rows, text) = match accounts.list() {
            Ok(list) => (list, String::new()),
            Err(error) => (Vec::new(), format!("Accounts are unavailable: {error}")),
        };
        self.accounts = rows;
        let texts: Vec<String> = self.accounts.iter().map(Account::row).collect();
        let texts: Vec<&str> = texts.iter().map(String::as_str).collect();
        self.list.set_items(&texts);
        let again = selected.and_then(|name| self.accounts.iter().position(|a| a.name == name));
        self.list
            .select(again.or(if texts.is_empty() { None } else { Some(0) }));
        text
    }

    fn selected(&self) -> Option<&Account> {
        self.list
            .selected()
            .and_then(|index| self.accounts.get(index))
    }

    /// Handle one message; returns the status line text.
    pub fn update(&mut self, msg: AccountsMsg, accounts: &dyn Accounts) -> String {
        let outcome = match msg {
            AccountsMsg::Select(row) => {
                // The row the message names (a session or a test may raise it).
                self.list
                    .select(Some(row).filter(|row| *row < self.accounts.len()));
                return String::new();
            }
            AccountsMsg::Add => self.add(accounts),
            AccountsMsg::Password => self.change_password(accounts),
            AccountsMsg::Admin(_) | AccountsMsg::Remove | AccountsMsg::SetPassword => {
                self.on_selected(msg, accounts)
            }
        };
        let text = match outcome {
            Ok(text) => text,
            Err(error) => format!("Not done: {error}"),
        };
        let unavailable = self.load(accounts);
        if unavailable.is_empty() {
            text
        } else {
            unavailable
        }
    }

    /// One of the actions on the selected account.
    fn on_selected(&self, msg: AccountsMsg, accounts: &dyn Accounts) -> Outcome {
        let target = self.selected().ok_or("select an account first")?;
        let list = &self.accounts;
        match msg {
            AccountsMsg::Admin(admin) => accounts_ops::set_admin(accounts, list, target, admin),
            AccountsMsg::Remove => accounts_ops::remove(accounts, list, target),
            _ => {
                let s = &self.secrets;
                let outcome = accounts_ops::set_password(
                    accounts,
                    target,
                    &s.set.text(),
                    &s.set_again.text(),
                );
                clear(&[&s.set, &s.set_again]);
                if outcome.is_ok() {
                    println!("SETTINGS:ACCOUNTS:SETPASSWORD:PASS user={}", target.name);
                }
                outcome
            }
        }
    }

    fn add(&self, accounts: &dyn Accounts) -> Outcome {
        let s = &self.secrets;
        let name = self.name.text().trim().to_string();
        let admin = self.admin.is_checked();
        let outcome = accounts_ops::add(accounts, &name, &s.add.text(), &s.add_again.text(), admin);
        clear(&[&s.add, &s.add_again]);
        if outcome.is_ok() {
            self.name.set_text("");
            self.admin.set_checked(false);
            println!(
                "SETTINGS:ACCOUNTS:ADD:PASS user={name} admin={}",
                u8::from(admin)
            );
        }
        outcome
    }

    fn change_password(&self, accounts: &dyn Accounts) -> Outcome {
        let s = &self.secrets;
        let outcome =
            accounts_ops::change_own(accounts, &s.old.text(), &s.new.text(), &s.confirm.text());
        clear(&[&s.old, &s.new, &s.confirm]);
        if outcome.is_ok() {
            let me = accounts.me().unwrap_or_default();
            println!("SETTINGS:ACCOUNTS:PASSWORD:PASS user={me}");
        }
        outcome
    }
}

/// Empty password fields once their value was used, refused or not.
fn clear(fields: &[&Rc<Edit<Msg>>]) {
    for field in fields {
        field.set_text("");
    }
}
