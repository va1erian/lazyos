//! The Accounts page's rules and flows, apart from its widgets so they are
//! tested on their own: what may be asked of [`Accounts`] and the status text
//! for each outcome.
//!
//! The page refuses ahead of time what the services would refuse anyway (a
//! name `accountsd` rejects, a password outside `accountdb::secret`'s rule,
//! two passwords that differ) and what would lock people out: removing the
//! account you are using, and removing or demoting the last administrator
//! (`accountsd` also refuses the last one, and the prompt would be wasted).

use accountdb::secret::check_secret;

use crate::accounts::{Account, Accounts};

/// A refusal or an outcome, as the status line words it.
pub type Outcome = Result<String, String>;

/// The new password typed twice, checked against the one rule.
pub fn new_password(password: &str, again: &str) -> Result<(), String> {
    check_secret(password).map_err(String::from)?;
    if password != again {
        return Err(String::from("the two passwords differ"));
    }
    Ok(())
}

/// How many administrators `list` has.
fn admins(list: &[Account]) -> usize {
    list.iter().filter(|account| account.admin).count()
}

/// Whether `target` is the only administrator left.
fn last_admin(list: &[Account], target: &Account) -> bool {
    target.admin && admins(list) <= 1
}

/// Remove `target`, unless it is `me` or the last administrator.
pub fn remove(accounts: &dyn Accounts, list: &[Account], target: &Account) -> Outcome {
    if accounts.me().as_deref() == Some(target.name.as_str()) {
        return Err(String::from("you cannot remove the account you are using"));
    }
    if last_admin(list, target) {
        return Err(String::from("the last administrator cannot be removed"));
    }
    accounts.remove(&target.name)?;
    Ok(format!(
        "{} was removed; its home is archived.",
        target.name
    ))
}

/// Make `target` an administrator or not; the last one stays one.
pub fn set_admin(
    accounts: &dyn Accounts,
    list: &[Account],
    target: &Account,
    admin: bool,
) -> Outcome {
    if target.admin == admin {
        return Ok(format!(
            "{} already is {}.",
            target.name,
            if admin {
                "an administrator"
            } else {
                "a standard user"
            }
        ));
    }
    if !admin && last_admin(list, target) {
        return Err(String::from("the last administrator cannot stop being one"));
    }
    accounts.set_admin(&target.name, admin)?;
    Ok(if admin {
        format!("{} is an administrator.", target.name)
    } else {
        format!("{} is no longer an administrator.", target.name)
    })
}

/// Set another account's password (an administrator approves); your own
/// goes through [`change_own`], which needs the current one.
pub fn set_password(
    accounts: &dyn Accounts,
    target: &Account,
    password: &str,
    again: &str,
) -> Outcome {
    if accounts.me().as_deref() == Some(target.name.as_str()) {
        return Err(String::from(
            "change your own password below, with your current one",
        ));
    }
    new_password(password, again)?;
    accounts.set_password(&target.name, password)?;
    Ok(format!("{}'s password was set.", target.name))
}

/// Add an account named `name` (trimmed).
pub fn add(
    accounts: &dyn Accounts,
    name: &str,
    password: &str,
    again: &str,
    admin: bool,
) -> Outcome {
    let name = name.trim();
    if !accountdb::valid_account_name(name) {
        return Err(String::from(
            "a name is lowercase letters, digits, '-' and '_', not 'root'",
        ));
    }
    new_password(password, again)?;
    accounts.create(name, password, admin)?;
    Ok(format!("{name} was added."))
}

/// Change your own password from `old`.
pub fn change_own(accounts: &dyn Accounts, old: &str, password: &str, again: &str) -> Outcome {
    let me = accounts.me().ok_or("your account is unknown")?;
    new_password(password, again)?;
    accounts.change_password(&me, old, password)?;
    Ok(String::from("Your password was changed."))
}

#[cfg(test)]
mod tests;
