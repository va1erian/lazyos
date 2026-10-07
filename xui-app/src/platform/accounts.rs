//! A typed client of the account service (`os.lazy.accounts.v1`,
//! docs/accounts-plan.md U1): the account list, a user changing their own
//! password, and the first-boot setup's owner account. The privileged
//! changes (another user's account) go through `elevd` instead
//! ([`super::elevd`]); `accountsd` refuses them from anyone else.
//!
//! Every call is bounded: a stalled service costs one refusal, never a frozen
//! window. Replies are untrusted, so names and homes are checked before they
//! reach the painter.

use messenger_generated::errors::ERROR_FIELD;
use messenger_generated::os_lazy_accounts_v1 as wire;

use super::messenger::{CallError, Service};
use crate::sys::errno;

/// `accountsd`'s registered name.
const NAME: &str = "os.lazy.accountsd";
/// How long a lookup may take (PIT ticks, 100 Hz).
const LIST_TICKS: u64 = 300;
/// How long a password change or account creation may take: Argon2id in
/// `keyd`, the brake's delay and, for an account, its home (under TCG).
const CHANGE_TICKS: u64 = 6000;
/// The most accounts shown (the database holds at most 64).
const MAX_USERS: usize = 64;

/// One account as the list shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub name: String,
    pub uid: u32,
    pub admin: bool,
}

/// What a refused call says: the errno and the service's friendly text.
pub type Refusal = CallError;

fn service() -> Result<Service, Refusal> {
    Service::try_connect(NAME).ok_or_else(|| CallError {
        code: -errno::ENOENT,
        message: String::from("the account service is not running"),
    })
}

fn encode_error(_: impl core::fmt::Debug) -> Refusal {
    CallError {
        code: -errno::EINVAL,
        message: String::from("the request could not be encoded"),
    }
}

/// The accounts, and whether the machine still waits for its owner (the
/// first-boot setup).
pub fn list() -> Result<(Vec<Account>, bool), Refusal> {
    let reply = service()?.call_detailed_within(
        wire::INTERFACE_ID,
        wire::METHOD_LISTUSERS,
        ERROR_FIELD,
        Vec::new(),
        LIST_TICKS,
    )?;
    let reply = wire::decode_list_users_reply(&reply.body).map_err(encode_error)?;
    let users = reply
        .users
        .into_iter()
        .filter(|user| valid_name(&user.name))
        .take(MAX_USERS)
        .map(|user| Account {
            name: user.name,
            uid: user.uid,
            admin: user.admin,
        })
        .collect();
    Ok((users, reply.setup))
}

/// Create the machine's first account, an administrator (the login screen
/// during the first-boot setup; refused once any account exists).
pub fn create_owner(name: &str, secret: &str) -> Result<(), Refusal> {
    let body = wire::encode_create_args(&wire::CreateArgs {
        name: name.to_string(),
        secret: secret.to_string(),
        admin: true,
    })
    .map_err(encode_error)?;
    service()?
        .call_detailed_within(
            wire::INTERFACE_ID,
            wire::METHOD_CREATE,
            ERROR_FIELD,
            body,
            CHANGE_TICKS,
        )
        .map(|_| ())
}

/// Change `name`'s own password from `old` to `secret`.
pub fn change_own_password(name: &str, old: &str, secret: &str) -> Result<(), Refusal> {
    let body = wire::encode_set_password_args(&wire::SetPasswordArgs {
        name: name.to_string(),
        old: Some(old.to_string()),
        secret: secret.to_string(),
    })
    .map_err(encode_error)?;
    service()?
        .call_detailed_within(
            wire::INTERFACE_ID,
            wire::METHOD_SETPASSWORD,
            ERROR_FIELD,
            body,
            CHANGE_TICKS,
        )
        .map(|_| ())
}

/// The Settings app's [`xui_settings::Accounts`]: the list and the user's
/// own password from `accountsd`, everything an administrator must approve
/// through `elevd` ([`super::elevd`]).
#[derive(Clone, Copy, Debug, Default)]
pub struct OsAccounts;

impl xui_settings::Accounts for OsAccounts {
    fn me(&self) -> Option<String> {
        std::env::var("USER").ok().filter(|name| valid_name(name))
    }

    fn list(&self) -> Result<Vec<xui_settings::Account>, String> {
        let (users, _) = list().map_err(|refusal| refusal.message)?;
        Ok(users
            .into_iter()
            .map(|user| xui_settings::Account {
                name: user.name,
                uid: user.uid,
                admin: user.admin,
            })
            .collect())
    }

    fn change_password(&self, name: &str, old: &str, new: &str) -> Result<(), String> {
        change_own_password(name, old, new).map_err(|refusal| refusal.message)
    }

    fn create(&self, name: &str, password: &str, admin: bool) -> Result<(), String> {
        let kind = if admin { "admin" } else { "user" };
        elevated("account.create", &[name, password, kind])
    }

    fn remove(&self, name: &str) -> Result<(), String> {
        elevated("account.delete", &[name, "archive"])
    }

    fn set_admin(&self, name: &str, admin: bool) -> Result<(), String> {
        elevated("account.admin", &[name, if admin { "1" } else { "0" }])
    }

    fn set_password(&self, name: &str, new: &str) -> Result<(), String> {
        elevated("account.password", &[name, new])
    }
}

/// One `elevd` request, its refusal as text.
fn elevated(operation: &str, args: &[&str]) -> Result<(), String> {
    super::elevd::request(operation, args)
        .map(|_| ())
        .map_err(|error| super::elevd::describe(&error))
}

/// A login name as `accountsd` accepts one (`accountdb::valid_name`:
/// `[a-z_][a-z0-9_-]*`, 1 to 32 bytes). Used to vet replies.
pub fn valid_name(name: &str) -> bool {
    accountdb::valid_name(name)
}

/// A name a new account may take: a login name that is neither a system
/// service's (`_`-prefixed) nor reserved (`root`), the rule `accountsd`
/// applies to `Create` (`accountdb::valid_account_name`).
pub fn valid_new_name(name: &str) -> bool {
    accountdb::valid_account_name(name)
}

#[cfg(test)]
mod tests {
    use super::{valid_name, valid_new_name};

    #[test]
    fn names_follow_the_database_rule() {
        assert!(valid_name("owner"));
        assert!(valid_name("a-b_1"));
        assert!(!valid_name("Owner"));
        assert!(!valid_name("1a"));
        assert!(!valid_name(""));
        assert!(!valid_name(&"a".repeat(33)));
        // A new account is never a system service's nor root.
        assert!(valid_name("_accounts") && !valid_new_name("_accounts"));
        assert!(!valid_new_name("root"));
        assert!(valid_new_name("owner"));
    }
}
