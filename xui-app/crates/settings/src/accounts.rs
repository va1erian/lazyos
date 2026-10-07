//! The accounts seam (docs/accounts-plan.md U1): what the Accounts page asks
//! of the system. On LazyOS the binary implements [`Accounts`] over
//! `accountsd` (the list, a user changing their own password) and `elevd`
//! (creating, removing and promoting accounts, after an administrator
//! approved on the trusted prompt); [`MemAccounts`] stands in for tests and
//! previews, the same split as [`ConfigStore`](crate::store::ConfigStore).

use std::cell::RefCell;

/// One account as the page lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub name: String,
    pub uid: u32,
    pub admin: bool,
}

impl Account {
    /// The list row: name, uid and whether it is an administrator.
    pub fn row(&self) -> String {
        let role = if self.admin { "administrator" } else { "user" };
        format!("{}   (uid {}, {role})", self.name, self.uid)
    }
}

/// Account management. Every failure is the text the status line shows.
pub trait Accounts {
    /// The account running Settings, when known.
    fn me(&self) -> Option<String>;
    /// Every account.
    fn list(&self) -> Result<Vec<Account>, String>;
    /// Change `name`'s own password from `old` to `new`.
    fn change_password(&self, name: &str, old: &str, new: &str) -> Result<(), String>;
    /// Create `name` (an administrator when `admin`): asks an administrator.
    fn create(&self, name: &str, password: &str, admin: bool) -> Result<(), String>;
    /// Remove `name`, archiving its home: asks an administrator.
    fn remove(&self, name: &str) -> Result<(), String>;
    /// Make `name` an administrator or not: asks an administrator.
    fn set_admin(&self, name: &str, admin: bool) -> Result<(), String>;
    /// Set another account's password without the old one (recovery):
    /// asks an administrator.
    fn set_password(&self, name: &str, new: &str) -> Result<(), String>;
}

/// An in-memory [`Accounts`] for tests and previews: `admin` and `user`, the
/// latter running Settings; passwords are kept in the clear (tests only).
pub struct MemAccounts {
    pub me: Option<String>,
    pub accounts: RefCell<Vec<(Account, String)>>,
    /// When set, every privileged call fails with this text (a cancelled
    /// prompt).
    pub refuse: RefCell<Option<String>>,
}

impl Default for MemAccounts {
    fn default() -> MemAccounts {
        let account = |name: &str, uid, admin| Account {
            name: name.to_string(),
            uid,
            admin,
        };
        MemAccounts {
            me: Some(String::from("user")),
            accounts: RefCell::new(vec![
                (account("admin", 1001, true), String::from("nimda")),
                (account("user", 1000, false), String::from("lazy")),
            ]),
            refuse: RefCell::new(None),
        }
    }
}

impl MemAccounts {
    fn approved(&self) -> Result<(), String> {
        match self.refuse.borrow().as_ref() {
            Some(text) => Err(text.clone()),
            None => Ok(()),
        }
    }
}

impl Accounts for MemAccounts {
    fn me(&self) -> Option<String> {
        self.me.clone()
    }

    fn list(&self) -> Result<Vec<Account>, String> {
        Ok(self
            .accounts
            .borrow()
            .iter()
            .map(|(a, _)| a.clone())
            .collect())
    }

    fn change_password(&self, name: &str, old: &str, new: &str) -> Result<(), String> {
        let mut accounts = self.accounts.borrow_mut();
        let entry = accounts
            .iter_mut()
            .find(|(account, _)| account.name == name)
            .ok_or_else(|| String::from("there is no such user"))?;
        if entry.1 != old {
            return Err(String::from("the current password is wrong"));
        }
        entry.1 = new.to_string();
        Ok(())
    }

    fn create(&self, name: &str, password: &str, admin: bool) -> Result<(), String> {
        self.approved()?;
        let mut accounts = self.accounts.borrow_mut();
        if accounts.iter().any(|(account, _)| account.name == name) {
            return Err(String::from("that name is taken"));
        }
        let uid = accounts
            .iter()
            .map(|(a, _)| a.uid + 1)
            .max()
            .unwrap_or(1000);
        let account = Account {
            name: name.to_string(),
            uid,
            admin,
        };
        accounts.push((account, password.to_string()));
        Ok(())
    }

    fn remove(&self, name: &str) -> Result<(), String> {
        self.approved()?;
        let mut accounts = self.accounts.borrow_mut();
        let before = accounts.len();
        accounts.retain(|(account, _)| account.name != name);
        if accounts.len() == before {
            return Err(String::from("there is no such user"));
        }
        Ok(())
    }

    fn set_admin(&self, name: &str, admin: bool) -> Result<(), String> {
        self.approved()?;
        let mut accounts = self.accounts.borrow_mut();
        let entry = accounts
            .iter_mut()
            .find(|(account, _)| account.name == name)
            .ok_or_else(|| String::from("there is no such user"))?;
        entry.0.admin = admin;
        Ok(())
    }

    fn set_password(&self, name: &str, new: &str) -> Result<(), String> {
        self.approved()?;
        let mut accounts = self.accounts.borrow_mut();
        let entry = accounts
            .iter_mut()
            .find(|(account, _)| account.name == name)
            .ok_or_else(|| String::from("there is no such user"))?;
        entry.1 = new.to_string();
        Ok(())
    }
}

impl MemAccounts {
    /// `name`'s password (tests only).
    pub fn password(&self, name: &str) -> Option<String> {
        self.accounts
            .borrow()
            .iter()
            .find(|(account, _)| account.name == name)
            .map(|(_, password)| password.clone())
    }
}
