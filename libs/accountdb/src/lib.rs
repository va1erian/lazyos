//! The account database, `/accounts/db` (docs/accounts-plan.md U1,
//! issue #624).
//!
//! One file holds every account and group, owned by the `_accounts` service
//! account ([`ACCOUNTS_UID`], mode 0600 in a 0700 directory) and written only
//! by `accountsd`. `/system/etc/passwd` and `/system/etc/group` are views
//! generated from it ([`Db::passwd_view`], [`Db::group_view`]); the password
//! verifiers stay here, read by `keyd` and nobody else. The image build seeds
//! the file with the same code, so a database `accountsd` would refuse never
//! ships.
//!
//! # Format
//!
//! Text, one record per line; blank lines and `#` comments are allowed and a
//! trailing `\r` is ignored:
//!
//! ```text
//! next:1002
//! group:admin:10
//! user:admin:1001:1001:/home/admin:sh:admin:argon2id:19456:2:1:<salt>:<hash>
//! user:user:1000:1000:/home/user:sh::!
//! ```
//!
//! * `next:<uid>`: the uid the next new account gets (at most once). A uid is
//!   never handed out twice, so a new account cannot inherit files a deleted
//!   one left behind;
//! * `group:<name>:<gid>`: a group; [`ADMIN_GROUP`] makes its members admins;
//! * `user:<name>:<uid>:<gid>:<home>:<shell>:<groups>:<secret>`: an account.
//!   `<groups>` is a comma-separated list of declared groups (possibly empty),
//!   `<secret>` is `!` (no password: nobody can log in as it) or the Argon2id
//!   verifier in the shadow file's form (`argon2id:<m_kib>:<t>:<p>:<salt>:<hash>`).
//!
//! The parser is strict and fails closed, like `libs/passwd`'s: a file that is
//! too large, not text or has any malformed or conflicting record yields a
//! [`LoadError`] and no account at all. An account's uid and primary gid lie
//! in [`FIRST_UID`]`..=`[`LAST_UID`] and its name passes
//! [`valid_account_name`]: no account logs in as root or as a system service
//! (the `admin` account is an ordinary uid in the `admin` group). An
//! empty database (no user) is valid: it is a machine waiting for its owner
//! (the first-boot setup, [`Db::needs_setup`]).
#![no_std]

extern crate alloc;
#[cfg(any(test, feature = "fuzz"))]
extern crate std;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

pub use passwd::shadow::Cost;
pub use passwd::valid_name;

#[cfg(any(test, feature = "fuzz"))]
pub mod fuzz;
pub mod ops;
pub mod policy;
pub mod ratelimit;
#[cfg(test)]
mod ratelimit_tests;
pub mod secret;
#[cfg(test)]
mod tests;

/// Largest database accepted, in bytes.
pub const DB_MAX: usize = 32 * 1024;
/// Most accounts the database holds.
pub const MAX_USERS: usize = 64;
/// Most groups the database holds.
pub const MAX_GROUPS: usize = 32;
/// Most supplementary groups one account belongs to.
pub const MAX_MEMBERSHIPS: usize = 8;
/// The first uid an account gets; below it are the system services' uids.
pub const FIRST_UID: u32 = 1000;
/// The last uid an account may get.
pub const LAST_UID: u32 = 59_999;
/// The group whose members are administrators: they approve `elevd`'s
/// privileged operations with their password (U2). Nobody is root.
pub const ADMIN_GROUP: &str = "admin";
/// The `admin` group's gid.
pub const ADMIN_GID: u32 = 10;
/// The `_accounts` system uid: `accountsd` runs as it and owns the database.
pub const ACCOUNTS_UID: u32 = 908;
/// The `_elev` system uid `elevd` runs as (docs/accounts-plan.md U2): the
/// one identity `accountsd` (and every other service with a privileged path)
/// takes privileged requests from, after an administrator approved them on
/// the trusted prompt.
pub const ELEVD_UID: u32 = 909;
/// The login shell of a new account (BusyBox `sh`, the applet alias the
/// kernel's Linux loader resolves).
pub const LOGIN_SHELL: &str = "sh";
/// The `<secret>` of an account without a password.
pub const LOCKED: &str = "!";
/// Names no account may take: the superuser's, which programs take for uid 0.
pub const RESERVED_NAMES: &[&str] = &["root"];

/// A name an account may have: a login name ([`valid_name`]) that is not a
/// system service's (`_`-prefixed, `_accounts`, `_elev`, ...) nor reserved
/// ([`RESERVED_NAMES`]). The parser, `Create`, `init`'s homes and the login
/// screen's setup all apply it, so they agree by construction.
pub fn valid_account_name(name: &str) -> bool {
    valid_name(name) && !name.starts_with('_') && !RESERVED_NAMES.contains(&name)
}

/// An Argon2id password verifier, as `keyd` derives and checks it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verifier {
    pub cost: Cost,
    pub salt: Vec<u8>,
    pub hash: [u8; passwd::shadow::VERIFIER_LEN],
}

impl Verifier {
    /// Parse `argon2id:<m_kib>:<t>:<p>:<salt>:<hash>`, the shadow row
    /// without its name; `None` when any field is wrong.
    pub fn parse(text: &str) -> Option<Verifier> {
        // The shadow row parser checks every field; the name is a stand-in.
        let entry = passwd::shadow::parse_row(&format!("x:{text}")).ok()?;
        Some(Verifier {
            cost: entry.cost,
            salt: entry.salt,
            hash: entry.verifier,
        })
    }

    /// The text [`Verifier::parse`] reads back.
    pub fn to_text(&self) -> String {
        let row = passwd::shadow::format_row("x", self.cost, &self.salt, &self.hash);
        row[2..].to_string()
    }
}

/// One group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub name: String,
    pub gid: u32,
}

/// One account. `secret` is `None` for a locked account (no password).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct User {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
    pub home: String,
    pub shell: String,
    /// Supplementary groups, by name, each declared in the database.
    pub groups: Vec<String>,
    pub secret: Option<Verifier>,
}

impl User {
    /// Whether the account is an administrator (a member of [`ADMIN_GROUP`]).
    pub fn is_admin(&self) -> bool {
        self.groups.iter().any(|group| group == ADMIN_GROUP)
    }
}

/// The whole database.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Db {
    pub groups: Vec<Group>,
    pub users: Vec<User>,
    /// The uid the next new account gets (see [`ops`]).
    pub next_uid: u32,
}

/// Why there is no database. [`fmt::Display`] is the `reason=` text of
/// `ACCOUNTS:LOAD:FAIL`: one word, then `key=value` details.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadError {
    /// The file does not exist.
    Missing,
    /// The file exists but could not be read (the errno).
    Unreadable(i64),
    /// Larger than [`DB_MAX`].
    Oversize(usize),
    /// Not UTF-8.
    NotText,
    /// Line `line` (1-based) is malformed in `field`.
    BadRecord { line: usize, field: &'static str },
    /// Line `line` repeats a name, uid or gid already used (`what`).
    Duplicate { line: usize, what: &'static str },
    /// Line `line` names a group no `group:` record declares.
    UnknownGroup { line: usize },
    /// More than [`MAX_USERS`] accounts or [`MAX_GROUPS`] groups.
    TooMany,
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::Missing => f.write_str("missing"),
            LoadError::Unreadable(errno) => write!(f, "unreadable errno={errno}"),
            LoadError::Oversize(size) => write!(f, "oversize bytes={size} max={DB_MAX}"),
            LoadError::NotText => f.write_str("not-utf8"),
            LoadError::BadRecord { line, field } => {
                write!(f, "bad-record line={line} field={field}")
            }
            LoadError::Duplicate { line, what } => write!(f, "duplicate-{what} line={line}"),
            LoadError::UnknownGroup { line } => write!(f, "unknown-group line={line}"),
            LoadError::TooMany => f.write_str("too-many"),
        }
    }
}

/// Parse a whole database (see the module docs for the rules).
pub fn parse(bytes: &[u8]) -> Result<Db, LoadError> {
    if bytes.len() > DB_MAX {
        return Err(LoadError::Oversize(bytes.len()));
    }
    let text = core::str::from_utf8(bytes).map_err(|_| LoadError::NotText)?;
    let mut db = Db::default();
    let mut next: Option<u32> = None;
    // Users are checked against the groups once every group is known.
    let mut users: Vec<(usize, User)> = Vec::new();
    for (index, raw) in text.split('\n').enumerate() {
        let line = index + 1;
        let row = raw.strip_suffix('\r').unwrap_or(raw);
        if row.trim().is_empty() || row.starts_with('#') {
            continue;
        }
        let bad = |field| LoadError::BadRecord { line, field };
        let (kind, rest) = row.split_once(':').ok_or(bad("kind"))?;
        match kind {
            "next" => {
                if next.is_some() {
                    return Err(LoadError::Duplicate { line, what: "next" });
                }
                let uid =
                    passwd::parse_id(rest).filter(|uid| (FIRST_UID..=LAST_UID + 1).contains(uid));
                next = Some(uid.ok_or(bad("next"))?);
            }
            "group" => {
                let group = parse_group(rest).map_err(bad)?;
                if db.groups.iter().any(|g| g.name == group.name) {
                    return Err(LoadError::Duplicate {
                        line,
                        what: "group",
                    });
                }
                if db.groups.iter().any(|g| g.gid == group.gid) {
                    return Err(LoadError::Duplicate { line, what: "gid" });
                }
                db.groups.push(group);
            }
            "user" => {
                let user = parse_user(rest).map_err(bad)?;
                if users.iter().any(|(_, u)| u.name == user.name) {
                    return Err(LoadError::Duplicate { line, what: "name" });
                }
                if users.iter().any(|(_, u)| u.uid == user.uid) {
                    return Err(LoadError::Duplicate { line, what: "uid" });
                }
                users.push((line, user));
            }
            _ => return Err(bad("kind")),
        }
        if db.groups.len() > MAX_GROUPS || users.len() > MAX_USERS {
            return Err(LoadError::TooMany);
        }
    }
    for (line, user) in users {
        if user
            .groups
            .iter()
            .any(|name| !db.groups.iter().any(|g| &g.name == name))
        {
            return Err(LoadError::UnknownGroup { line });
        }
        db.users.push(user);
    }
    db.next_uid = next.unwrap_or_else(|| db.lowest_free_uid());
    Ok(db)
}

/// `<name>:<gid>`.
fn parse_group(rest: &str) -> Result<Group, &'static str> {
    let (name, gid) = rest.split_once(':').ok_or("count")?;
    if !valid_name(name) {
        return Err("name");
    }
    let gid = passwd::parse_id(gid).ok_or("gid")?;
    Ok(Group {
        name: name.to_string(),
        gid,
    })
}

/// `<name>:<uid>:<gid>:<home>:<shell>:<groups>:<secret>`.
fn parse_user(rest: &str) -> Result<User, &'static str> {
    let fields: Vec<&str> = rest.splitn(7, ':').collect();
    let [name, uid, gid, home, shell, groups, secret] = fields[..] else {
        return Err("count");
    };
    if !valid_account_name(name) {
        return Err("name");
    }
    // Nobody logs in as root (docs/accounts-plan.md, decisions), nor as a
    // system service, nor with a system group as its own: an account's uid
    // and primary gid are in the accounts' range (review of #659).
    let accounts = FIRST_UID..=LAST_UID;
    let uid = passwd::parse_id(uid)
        .filter(|uid| accounts.contains(uid))
        .ok_or("uid")?;
    let gid = passwd::parse_id(gid)
        .filter(|gid| accounts.contains(gid))
        .ok_or("gid")?;
    if !passwd::valid_home(home) {
        return Err("home");
    }
    if !passwd::valid_shell(shell) {
        return Err("shell");
    }
    let groups: Vec<String> = if groups.is_empty() {
        Vec::new()
    } else {
        groups.split(',').map(str::to_string).collect()
    };
    let mut seen: Vec<&str> = Vec::new();
    for group in &groups {
        if !valid_name(group) || seen.contains(&group.as_str()) {
            return Err("groups");
        }
        seen.push(group);
    }
    if groups.len() > MAX_MEMBERSHIPS {
        return Err("groups");
    }
    let secret = match secret {
        LOCKED => None,
        text => Some(Verifier::parse(text).ok_or("secret")?),
    };
    Ok(User {
        name: name.to_string(),
        uid,
        gid,
        home: home.to_string(),
        shell: shell.to_string(),
        groups,
        secret,
    })
}

impl Db {
    /// The database as text [`parse`] reads back.
    pub fn to_text(&self) -> String {
        let mut out = String::from(
            "# LazyOS account database (docs/accounts-plan.md); written by accountsd\n",
        );
        out.push_str(&format!("next:{}\n", self.next_uid));
        for group in &self.groups {
            out.push_str(&format!("group:{}:{}\n", group.name, group.gid));
        }
        for user in &self.users {
            let secret = user
                .secret
                .as_ref()
                .map_or_else(|| LOCKED.to_string(), Verifier::to_text);
            out.push_str(&format!(
                "user:{}:{}:{}:{}:{}:{}:{}\n",
                user.name,
                user.uid,
                user.gid,
                user.home,
                user.shell,
                user.groups.join(","),
                secret
            ));
        }
        out
    }

    /// `/system/etc/passwd`: `name:uid:gid:x:home:shell` per account (the
    /// verifiers never leave the database).
    pub fn passwd_view(&self) -> String {
        let mut out = String::new();
        for user in &self.users {
            out.push_str(&format!(
                "{}:{}:{}:{}:{}:{}\n",
                user.name,
                user.uid,
                user.gid,
                passwd::IN_SHADOW,
                user.home,
                user.shell
            ));
        }
        out
    }

    /// `/system/etc/group`: `name:gid:member,member` per group, members in
    /// account order. The kernel adds it to the Linux `/etc/group`.
    pub fn group_view(&self) -> String {
        let mut out = String::new();
        for group in &self.groups {
            let members: Vec<&str> = self
                .users
                .iter()
                .filter(|user| user.groups.contains(&group.name))
                .map(|user| user.name.as_str())
                .collect();
            out.push_str(&format!(
                "{}:{}:{}\n",
                group.name,
                group.gid,
                members.join(",")
            ));
        }
        out
    }

    /// The account called `name`.
    pub fn user(&self, name: &str) -> Option<&User> {
        self.users.iter().find(|user| user.name == name)
    }

    /// The account with `uid`.
    pub fn by_uid(&self, uid: u32) -> Option<&User> {
        self.users.iter().find(|user| user.uid == uid)
    }

    /// How many administrators there are.
    pub fn admins(&self) -> usize {
        self.users.iter().filter(|user| user.is_admin()).count()
    }

    /// Whether the machine still needs its owner: no account at all. The
    /// first-boot setup (the login screen) may then create one admin.
    pub fn needs_setup(&self) -> bool {
        self.users.is_empty()
    }

    /// The smallest uid at or above [`FIRST_UID`] above every account's.
    pub fn lowest_free_uid(&self) -> u32 {
        self.users
            .iter()
            .map(|user| user.uid.saturating_add(1))
            .fold(FIRST_UID, u32::max)
    }
}
