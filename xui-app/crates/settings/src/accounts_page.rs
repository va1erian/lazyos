//! The Accounts page (docs/accounts-plan.md U1): the accounts of this
//! computer, adding and removing one, making one an administrator, and
//! changing your own password.
//!
//! Changing your own password needs only your current one. Adding, removing
//! and promoting accounts are privileged: they go through [`Accounts`] to
//! `elevd`, which shows the trusted prompt where an administrator approves
//! (U2). A removed account's home is archived, never deleted outright.

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
use crate::app::{choice_list, Msg};

/// Messages the Accounts page's widgets raise.
#[derive(Clone, Debug, PartialEq)]
pub enum AccountsMsg {
    /// An account row was selected.
    Select(usize),
    /// Make the selected account an administrator (`true`) or not.
    Admin(bool),
    Remove,
    Add,
    Password,
}

/// A button raising `msg`.
fn command(text: &str, msg: AccountsMsg) -> Build<Button<Msg>, Msg> {
    button(text).on_click(Msg::Accounts(msg))
}

/// The page's widgets and the accounts listed.
pub struct AccountsPage {
    list: Rc<ListView<Msg>>,
    name: Rc<Edit<Msg>>,
    password: Rc<Edit<Msg>>,
    admin: Rc<CheckBox<Msg>>,
    old: Rc<Edit<Msg>>,
    new: Rc<Edit<Msg>>,
    confirm: Rc<Edit<Msg>>,
    _mounted: Mounted<Msg>,
    accounts: Vec<Account>,
}

impl AccountsPage {
    /// Lays the page out in the container `page`.
    pub fn build(ui: &Ui<Msg>, page: WidgetId) -> Result<AccountsPage> {
        let (list, name, password, admin) =
            (Handle::new(), Handle::new(), Handle::new(), Handle::new());
        let (old, new, confirm) = (Handle::new(), Handle::new(), Handle::new());
        let secret = |hint: &str, handle: &Handle<Edit<Msg>>| {
            edit().password().placeholder(hint).bind(handle).width(130)
        };
        let mounted = ui.mount_in(
            page,
            column().padding(16).gap(8).children((
                label("Accounts on this computer"),
                choice_list(&[])
                    .on_select(|i| Msg::Accounts(AccountsMsg::Select(i)))
                    .bind(&list)
                    .size(440, 110)
                    .align(Align::Start),
                row().gap(6).children((
                    command("Make administrator", AccountsMsg::Admin(true)),
                    command("Remove administrator", AccountsMsg::Admin(false)),
                    command("Remove", AccountsMsg::Remove),
                )),
                label("Add an account"),
                row().gap(6).children((
                    edit().placeholder("name").bind(&name).width(130),
                    secret("password", &password),
                    checkbox("Administrator").bind(&admin),
                    command("Add", AccountsMsg::Add),
                )),
                label("Change your password"),
                row().gap(6).children((
                    secret("current", &old),
                    secret("new", &new),
                    secret("new again", &confirm),
                )),
                command("Change password", AccountsMsg::Password).align(Align::Start),
                label("Adding, removing and promoting accounts asks an administrator."),
            )),
        )?;
        Ok(AccountsPage {
            list: list.get(),
            name: name.get(),
            password: password.get(),
            admin: admin.get(),
            old: old.get(),
            new: new.get(),
            confirm: confirm.get(),
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
            AccountsMsg::Select(_) => return String::new(),
            AccountsMsg::Admin(admin) => self.set_admin(accounts, admin),
            AccountsMsg::Remove => self.remove(accounts),
            AccountsMsg::Add => self.add(accounts),
            AccountsMsg::Password => self.change_password(accounts),
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

    fn set_admin(
        &self,
        accounts: &dyn Accounts,
        admin: bool,
    ) -> std::result::Result<String, String> {
        let account = self.selected().ok_or("select an account first")?;
        accounts.set_admin(&account.name, admin)?;
        Ok(if admin {
            format!("{} is an administrator.", account.name)
        } else {
            format!("{} is no longer an administrator.", account.name)
        })
    }

    fn remove(&self, accounts: &dyn Accounts) -> std::result::Result<String, String> {
        let account = self.selected().ok_or("select an account first")?;
        if accounts.me().as_deref() == Some(account.name.as_str()) {
            return Err(String::from("you cannot remove the account you are using"));
        }
        accounts.remove(&account.name)?;
        Ok(format!(
            "{} was removed; its home is archived.",
            account.name
        ))
    }

    fn add(&self, accounts: &dyn Accounts) -> std::result::Result<String, String> {
        let name = self.name.text().trim().to_string();
        let password = self.password.text();
        if name.is_empty() || password.is_empty() {
            return Err(String::from("type a name and a password"));
        }
        let admin = self.admin.is_checked();
        accounts.create(&name, &password, admin)?;
        self.name.set_text("");
        self.password.set_text("");
        self.admin.set_checked(false);
        println!(
            "SETTINGS:ACCOUNTS:ADD:PASS user={name} admin={}",
            u8::from(admin)
        );
        Ok(format!("{name} was added."))
    }

    fn change_password(&self, accounts: &dyn Accounts) -> std::result::Result<String, String> {
        let me = accounts.me().ok_or("your account is unknown")?;
        let (old, new) = (self.old.text(), self.new.text());
        if new.is_empty() {
            return Err(String::from("type a new password"));
        }
        if new != self.confirm.text() {
            return Err(String::from("the two new passwords differ"));
        }
        let result = accounts.change_password(&me, &old, &new);
        for field in [&self.old, &self.new, &self.confirm] {
            field.set_text("");
        }
        result?;
        println!("SETTINGS:ACCOUNTS:PASSWORD:PASS user={me}");
        Ok(String::from("Your password was changed."))
    }
}
