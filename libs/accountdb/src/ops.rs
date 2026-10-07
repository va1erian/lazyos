//! Account operations on a [`Db`]: what `Create`, `Delete`, `SetAdmin` and
//! `SetPassword` do to the records once [`policy`](crate::policy) said the
//! caller may ask. Each either applies completely or returns an [`OpError`]
//! and leaves the database as it was, so `accountsd` persists only whole
//! changes.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::{
    valid_account_name, Db, Group, User, Verifier, ADMIN_GID, ADMIN_GROUP, FIRST_UID, LAST_UID,
    LOGIN_SHELL, MAX_USERS,
};

/// Why an operation was refused. [`OpError::errno`] is the code `accountsd`
/// answers with, [`OpError::message`] the text the user reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpError {
    /// The name is not a valid login name.
    BadName,
    /// An account (or group) of that name exists.
    Exists,
    /// No account of that name.
    NotFound,
    /// The change would leave the machine without an administrator.
    LastAdmin,
    /// No room for another account, or no uid left.
    Full,
}

impl OpError {
    /// The errno-style code of the refusal.
    pub const fn errno(self) -> i64 {
        match self {
            OpError::BadName => 22,   // EINVAL
            OpError::Exists => 17,    // EEXIST
            OpError::NotFound => 2,   // ENOENT
            OpError::LastAdmin => 16, // EBUSY
            OpError::Full => 28,      // ENOSPC
        }
    }

    /// A short, friendly explanation.
    pub const fn message(self) -> &'static str {
        match self {
            OpError::BadName => {
                "a user name is lowercase letters, digits, '_' and '-', starting with a letter"
            }
            OpError::Exists => "that name is taken",
            OpError::NotFound => "there is no such user",
            OpError::LastAdmin => "the machine must keep at least one administrator",
            OpError::Full => "no room for another account",
        }
    }
}

impl Db {
    /// Create the account `name` (an admin when `admin`) with `secret`
    /// (`None`: locked until a password is set). It gets the next free uid,
    /// a primary group of the same number, `/home/<name>` and the login
    /// shell; the caller makes the home itself. Returns the new record.
    pub fn create(
        &mut self,
        name: &str,
        admin: bool,
        secret: Option<Verifier>,
    ) -> Result<User, OpError> {
        // `_`-prefixed names are the system services' (`_accounts`, ...).
        if !valid_account_name(name) {
            return Err(OpError::BadName);
        }
        if self.user(name).is_some() || self.groups.iter().any(|g| g.name == name) {
            return Err(OpError::Exists);
        }
        if self.users.len() >= MAX_USERS {
            return Err(OpError::Full);
        }
        let uid = self.free_uid().ok_or(OpError::Full)?;
        if admin {
            self.ensure_admin_group();
        }
        let user = User {
            name: name.to_string(),
            uid,
            gid: uid,
            home: fhs::home_of(name),
            shell: LOGIN_SHELL.to_string(),
            groups: if admin {
                alloc::vec![String::from(ADMIN_GROUP)]
            } else {
                Vec::new()
            },
            secret,
        };
        self.users.push(user.clone());
        self.next_uid = uid + 1;
        Ok(user)
    }

    /// Delete the account `name`; returns the removed record (its home is
    /// the caller's to archive or remove). The last administrator stays.
    pub fn delete(&mut self, name: &str) -> Result<User, OpError> {
        let index = self.position(name)?;
        if self.users[index].is_admin() && self.admins() == 1 {
            return Err(OpError::LastAdmin);
        }
        Ok(self.users.remove(index))
    }

    /// Make `name` an administrator or not. Taking it from the last one is
    /// refused; a change to the current state is not an error.
    pub fn set_admin(&mut self, name: &str, admin: bool) -> Result<(), OpError> {
        let index = self.position(name)?;
        let is = self.users[index].is_admin();
        if is == admin {
            return Ok(());
        }
        if !admin && self.admins() == 1 {
            return Err(OpError::LastAdmin);
        }
        if admin {
            self.ensure_admin_group();
            self.users[index].groups.push(String::from(ADMIN_GROUP));
        } else {
            self.users[index]
                .groups
                .retain(|group| group != ADMIN_GROUP);
        }
        Ok(())
    }

    /// Replace `name`'s verifier.
    pub fn set_secret(&mut self, name: &str, secret: Verifier) -> Result<(), OpError> {
        let index = self.position(name)?;
        self.users[index].secret = Some(secret);
        Ok(())
    }

    fn position(&self, name: &str) -> Result<usize, OpError> {
        self.users
            .iter()
            .position(|user| user.name == name)
            .ok_or(OpError::NotFound)
    }

    /// The `admin` group exists (an empty database may lack it).
    fn ensure_admin_group(&mut self) {
        if self.groups.iter().any(|group| group.name == ADMIN_GROUP) {
            return;
        }
        // Its usual gid, unless a hand-edited database gave that one away.
        let gid = (ADMIN_GID..)
            .find(|gid| !self.groups.iter().any(|group| group.gid == *gid))
            .unwrap_or(ADMIN_GID);
        self.groups.push(Group {
            name: String::from(ADMIN_GROUP),
            gid,
        });
    }

    /// The next uid to hand out: [`Db::next_uid`] or the first one after it
    /// that no account and no group uses as an id (a primary group shares
    /// the uid's number).
    fn free_uid(&self) -> Option<u32> {
        (self.next_uid.max(FIRST_UID)..=LAST_UID).find(|uid| {
            !self
                .users
                .iter()
                .any(|user| user.uid == *uid || user.gid == *uid)
                && !self.groups.iter().any(|group| group.gid == *uid)
        })
    }
}
