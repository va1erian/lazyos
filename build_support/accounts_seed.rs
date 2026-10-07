//! The accounts an image starts with (docs/accounts-plan.md U1, issue #624):
//! the account database `/accounts/db` and its views.
//!
//! The seed comes from three files beside this one: `passwd` (the accounts,
//! `name:uid:gid:x:home:shell`), `groups` (`name:gid:members`; `admin` makes
//! administrators) and `passwords` (`name:password`, the documented
//! development defaults, `docs/security-model.md` section 3, never written to
//! the volume). The build hashes each password with Argon2id at `keyd`'s own
//! cost ([`kdf::Params::INTERACTIVE`]) into the database, which is written
//! with `libs/accountdb`, the code `accountsd` and `keyd` read it with, so a
//! database they would refuse never ships.
//!
//! The database is a **seed** ([`Placement::seed`]): written when the volume
//! has none, then the running system's (accounts created, deleted and
//! passwords changed survive every update). `/system/etc/passwd` and
//! `/system/etc/group` are views of it, owned by `_accounts` so `accountsd`
//! can rewrite them; the build writes them from the seed and `accountsd`
//! regenerates them from the live database at every start.
//!
//! The salt is derived from the name and the password (a SHA-256 prefix), so
//! the same inputs give the same file and an unchanged image stays unchanged.
//! Accounts made at runtime get random salts from `keyd`.

use accountdb::{Cost, Db, Group, User, Verifier, ACCOUNTS_UID};
use lazyos_crypto::{kdf, sha256};

use crate::os_image::{Placement, Sink, Source};
use crate::os_layout::Account;

/// The default accounts, `name:uid:gid:x:home:shell`.
pub const PASSWD: &[u8] = include_bytes!("passwd");
/// The default groups, `name:gid:members`.
pub const GROUPS: &str = include_str!("groups");
/// The default passwords, `name:password`.
pub const PASSWORDS: &str = include_str!("passwords");
/// Salt bytes per verifier.
const SALT_LEN: usize = 16;
/// The new account skeleton's only file: a home is otherwise empty until
/// LazyShell seeds the desktop.
const SKEL_PROFILE: &str =
    "# ~/.profile, copied from /system/etc/skel when the account was created.\n";

/// Which accounts an image carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Seed {
    /// The default accounts (`admin`, `user`) with their passwords.
    Accounts,
    /// No account: the login screen asks for the owner (first-boot setup).
    Setup,
    /// No database at all: `accountsd` fails closed (the recovery check).
    Omit,
}

/// The seed this build asks for: `LAZYOS_OMIT_PASSWD=1` omits it,
/// `LAZYOS_SETUP=1` empties it. A setup image never logs anyone in
/// (`user/build.rs` drops its autologin), so a `LAZYOS_AUTOLOGIN` name is
/// ignored with a warning rather than keeping accounts the setup would hide.
pub fn from_env() -> Seed {
    for name in ["LAZYOS_OMIT_PASSWD", "LAZYOS_SETUP", "LAZYOS_AUTOLOGIN"] {
        println!("cargo:rerun-if-env-changed={name}");
    }
    for file in ["passwd", "groups", "passwords", "accounts_seed.rs"] {
        println!("cargo:rerun-if-changed=build_support/{file}");
    }
    let on = |name: &str| std::env::var_os(name).as_deref() == Some(std::ffi::OsStr::new("1"));
    if on("LAZYOS_OMIT_PASSWD") {
        println!("cargo:warning=LAZYOS_OMIT_PASSWD=1: no account database; no login will succeed");
        return Seed::Omit;
    }
    if !on("LAZYOS_SETUP") {
        return Seed::Accounts;
    }
    let autologin = std::env::var("LAZYOS_AUTOLOGIN").unwrap_or_default();
    if !matches!(autologin.trim(), "" | "none") {
        println!(
            "cargo:warning=LAZYOS_SETUP=1 ignores LAZYOS_AUTOLOGIN={}: a first-boot setup \
             image has no account until its owner is created, and logs nobody in",
            autologin.trim()
        );
    }
    Seed::Setup
}

/// The seed database. Every account of `passwd` needs exactly one password
/// and every password an account; a group may list only known accounts.
/// Anything else fails the build, as an image whose accounts cannot log in
/// would.
pub fn database(seed: Seed) -> Db {
    let accounts = passwd::parse(PASSWD).expect("build_support/passwd parses");
    let groups = parse_groups(&accounts);
    let passwords = parse_passwords(&accounts);
    let mut db = Db {
        groups: groups.iter().map(|(group, _)| group.clone()).collect(),
        users: Vec::new(),
        next_uid: 0,
    };
    if seed == Seed::Accounts {
        for account in &accounts {
            let password = passwords
                .iter()
                .find(|(name, _)| *name == account.name)
                .map(|(_, password)| *password)
                .unwrap_or_else(|| {
                    panic!("build_support/passwords: no password for {}", account.name)
                });
            db.users.push(User {
                name: account.name.clone(),
                uid: account.uid,
                gid: account.gid,
                home: account.home.clone(),
                shell: account.shell.clone(),
                groups: groups
                    .iter()
                    .filter(|(_, members)| members.contains(&account.name))
                    .map(|(group, _)| group.name.clone())
                    .collect(),
                secret: Some(verifier(&account.name, password)),
            });
        }
    }
    db.next_uid = db.lowest_free_uid();
    let again = accountdb::parse(db.to_text().as_bytes()).expect("the seed database parses");
    assert_eq!(again, db, "the seed database round-trips");
    db
}

/// `groups`: each group and the account names it lists.
fn parse_groups(accounts: &[passwd::Entry]) -> Vec<(Group, Vec<String>)> {
    let mut out: Vec<(Group, Vec<String>)> = Vec::new();
    for line in lines(GROUPS) {
        let fields: Vec<&str> = line.split(':').collect();
        let [name, gid, members] = fields[..] else {
            panic!("build_support/groups: not name:gid:members: {line:?}");
        };
        let gid = gid
            .parse()
            .unwrap_or_else(|_| panic!("build_support/groups: bad gid in {line:?}"));
        let members: Vec<String> = members
            .split(',')
            .filter(|member| !member.is_empty())
            .map(str::to_string)
            .collect();
        for member in &members {
            assert!(
                accounts.iter().any(|account| &account.name == member),
                "build_support/groups: {member} has no account in build_support/passwd"
            );
        }
        out.push((
            Group {
                name: name.to_string(),
                gid,
            },
            members,
        ));
    }
    out
}

/// `passwords`: one `(name, password)` per account, checked against it.
fn parse_passwords(accounts: &[passwd::Entry]) -> Vec<(&'static str, &'static str)> {
    let mut out: Vec<(&str, &str)> = Vec::new();
    for line in lines(PASSWORDS) {
        let (name, password) = line
            .split_once(':')
            .unwrap_or_else(|| panic!("build_support/passwords: not name:password: {line:?}"));
        assert!(
            accounts.iter().any(|account| account.name == name),
            "build_support/passwords: {name} has no account in build_support/passwd"
        );
        assert!(
            !out.iter().any(|(seen, _)| *seen == name),
            "build_support/passwords: {name} twice"
        );
        out.push((name, password));
    }
    out
}

/// The lines of a seed file that are neither blank nor comments.
fn lines(text: &'static str) -> impl Iterator<Item = &'static str> {
    text.lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
}

/// The Argon2id verifier of `password` for `name`.
pub fn verifier(name: &str, password: &str) -> Verifier {
    let digest = sha256::sha256_parts(&[
        b"lazyos-shadow-salt\0",
        name.as_bytes(),
        b"\0",
        password.as_bytes(),
    ]);
    let salt = &digest[..SALT_LEN];
    let params = kdf::Params::INTERACTIVE;
    let mut hash = [0u8; passwd::shadow::VERIFIER_LEN];
    kdf::argon2id(password.as_bytes(), salt, params, &mut hash).expect("argon2id");
    Verifier {
        cost: Cost {
            m_kib: params.m_cost_kib,
            t: params.t_cost,
            p: params.p_cost,
        },
        salt: salt.to_vec(),
        hash,
    }
}

/// The accounts whose homes the image creates (`os_layout::dirs`).
pub fn homes(db: &Db) -> Vec<Account> {
    db.users
        .iter()
        .map(|user| Account {
            name: user.name.clone(),
            uid: user.uid,
            gid: user.gid,
            home: user.home.clone(),
        })
        .collect()
}

/// Place the database (a seed), its two views and the home skeleton.
pub fn place(sink: &mut dyn Sink, db: &Db) {
    let owner = Placement::owned(ACCOUNTS_UID, ACCOUNTS_UID);
    sink.add_placed(
        fhs::state::ACCOUNTS_DB,
        Source::Bytes(db.to_text().into_bytes()),
        0o600,
        Placement::seed(ACCOUNTS_UID, ACCOUNTS_UID),
    );
    let view = |text: String| Source::Bytes(text.into_bytes());
    sink.add_placed(fhs::etc::PASSWD, view(db.passwd_view()), 0o644, owner);
    sink.add_placed(fhs::etc::GROUP, view(db.group_view()), 0o644, owner);
    sink.add_bytes(
        &format!("{}/.profile", fhs::etc::SKEL),
        SKEL_PROFILE.as_bytes().to_vec(),
    );
}
