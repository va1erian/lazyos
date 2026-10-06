//! `/system/etc/shadow` (issue #447): the build hashes the default accounts'
//! passwords with Argon2id and ships only the verifiers, root-only (0600).
//!
//! The passwords themselves come from `build_support/passwords`
//! (`name:password`, one per account of `build_support/passwd`): the image's
//! documented development defaults (`docs/security-model.md` section 3), never
//! written to the volume. The cost is `keyd`'s own
//! ([`kdf::Params::INTERACTIVE`]), so `keyd` verifies a typed password against
//! a row with the same arena it uses for everything else.
//!
//! The salt is derived from the name and the password (a SHA-256 prefix), so
//! the same inputs give the same file and an unchanged image stays unchanged.
//! It still differs per account and per password; the passwords are public
//! development defaults, and accounts made at runtime (U1) get random salts
//! from `keyd`.

use lazyos_crypto::{kdf, sha256};
use passwd::shadow::{self, Cost};

/// The default passwords, `name:password`.
pub const PASSWORDS: &str = include_str!("passwords");
/// Salt bytes per row.
const SALT_LEN: usize = 16;

/// The shadow file for the accounts of `passwd` (the account file's text).
/// Every account must have exactly one password line and every password line
/// an account; anything else fails the build, as an image whose accounts
/// cannot log in would.
pub fn build(passwd: &[u8]) -> Vec<u8> {
    let accounts = passwd::parse(passwd).expect("build_support/passwd parses");
    let mut out = String::from("# Argon2id verifiers (issue #447); written by the image build.\n");
    let mut seen = Vec::new();
    for line in PASSWORDS
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
    {
        let (name, password) = line
            .split_once(':')
            .unwrap_or_else(|| panic!("build_support/passwords: not name:password: {line:?}"));
        assert!(
            accounts.iter().any(|account| account.name == name),
            "build_support/passwords: {name} has no account in build_support/passwd"
        );
        assert!(
            !seen.contains(&name),
            "build_support/passwords: {name} twice"
        );
        seen.push(name);
        out.push_str(&row(name, password));
        out.push('\n');
    }
    for account in &accounts {
        assert!(
            seen.contains(&account.name.as_str()),
            "build_support/passwords: no password for {}",
            account.name
        );
    }
    let bytes = out.into_bytes();
    shadow::parse(&bytes).expect("the generated shadow file parses");
    bytes
}

/// One verifier row.
fn row(name: &str, password: &str) -> String {
    let digest = sha256::sha256_parts(&[
        b"lazyos-shadow-salt\0",
        name.as_bytes(),
        b"\0",
        password.as_bytes(),
    ]);
    let salt = &digest[..SALT_LEN];
    let params = kdf::Params::INTERACTIVE;
    let mut verifier = [0u8; shadow::VERIFIER_LEN];
    kdf::argon2id(password.as_bytes(), salt, params, &mut verifier).expect("argon2id");
    let cost = Cost {
        m_kib: params.m_cost_kib,
        t: params.t_cost,
        p: params.p_cost,
    };
    shadow::format_row(name, cost, salt, &verifier)
}
