//! Who may change accounts (docs/accounts-plan.md U1, U2).
//!
//! `accountsd` turns the kernel-stamped identity of a request into a [`Who`]
//! and asks [`authorize`] before it touches the database. The rules:
//!
//! * **`elevd`** may create, delete and promote accounts and set any
//!   password: it does so only after an administrator approved that very
//!   operation on the trusted prompt (U2). Nothing else may, root included:
//!   there is no uid that bypasses this table.
//! * **the login screen** may create exactly one account, an administrator,
//!   and only while the database has none (the first-boot setup).
//! * **a user** may change their own password, with the old one.
//! * everyone else is refused.
//!
//! Lookups, `ListUsers` and `Authenticate` are open to every caller (the
//! last one rate-limited, [`ratelimit`](crate::ratelimit)).

use crate::Db;

/// The caller of an account request, from its kernel-stamped credentials.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Who {
    /// `elevd` itself (its system uid, unlabelled, no session).
    Elevd,
    /// The login screen (the `_greeter` system uid, unlabelled, no session).
    Greeter,
    /// A task with this uid: a session's program or any other caller.
    User { uid: u32 },
}

/// An account change, as far as authorization cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change<'a> {
    Create {
        admin: bool,
    },
    Delete,
    SetAdmin,
    /// Set `target`'s password; `with_old` when the request carries the
    /// current one.
    SetPassword {
        target: &'a str,
        with_old: bool,
    },
}

/// What the caller must still prove once a change is allowed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Allowed {
    /// Nothing more.
    Now,
    /// The target's current password (checked, and rate-limited, like a
    /// login).
    WithOldPassword,
}

/// A refusal: always `EPERM`, with a friendly reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Denied(pub &'static str);

impl Denied {
    /// The errno-style code (`EPERM`).
    pub const ERRNO: i64 = 1;
}

/// Whether `who` may make `change` to `db`.
pub fn authorize(db: &Db, who: Who, change: Change<'_>) -> Result<Allowed, Denied> {
    match (who, change) {
        (Who::Elevd, _) => Ok(Allowed::Now),
        (Who::Greeter, Change::Create { admin: true }) if db.needs_setup() => Ok(Allowed::Now),
        (Who::Greeter, _) => Err(Denied(
            "the login screen may only create the owner account of a new machine",
        )),
        (Who::User { uid }, Change::SetPassword { target, with_old }) => match db.user(target) {
            Some(account) if account.uid == uid && with_old => Ok(Allowed::WithOldPassword),
            Some(account) if account.uid == uid => {
                Err(Denied("changing your password needs your current password"))
            }
            _ => Err(Denied(
                "only an administrator, through elevd, may change another user's password",
            )),
        },
        (Who::User { .. }, _) => Err(Denied(
            "only an administrator, through elevd, may create, delete or promote accounts",
        )),
    }
}
