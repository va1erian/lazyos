//! Which paths `fs.read` may open.
//!
//! `dbgd` has its own uid, so the filesystem already refuses what that uid
//! may not read; this list is the second lock. A path is accepted only as an
//! absolute, normalised name under one of [`ROOTS`], and never one of
//! [`SECRETS`] or anything under a secret directory.

/// Directories (and the files in them) a client may read.
pub const ROOTS: &[&str] = &[
    fhs::mount::TRANSIENT,
    fhs::mount::TMP,
    fhs::state::LOGS_ROOT,
    fhs::system::SYSTEM_ETC,
    fhs::system::SYSTEM_SHARE,
    fhs::docs::DOCS_ROOT,
];

/// Names refused even under a root: the password verifiers, `confd`'s raw
/// store, the account database and the boot config (which carries `dbgd`'s
/// own key).
pub const SECRETS: &[&str] = &[
    fhs::etc::SHADOW,
    fhs::boot::LAZYOS_CFG_PATH,
    fhs::state::ACCOUNTS_DIR,
    fhs::state::CONF_ROOT,
];

/// Why a path was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Not absolute, or holds `..`, `.`, an empty component or a control
    /// character.
    Malformed,
    /// Outside every root.
    Outside,
    /// A secret.
    Secret,
}

impl Refusal {
    pub fn text(self) -> &'static str {
        match self {
            Refusal::Malformed => "path is not absolute and normalised",
            Refusal::Outside => "path is outside the readable roots",
            Refusal::Secret => "path names a secret",
        }
    }
}

/// Whether `path` is `root` or lives under it.
fn under(path: &str, root: &str) -> bool {
    path == root
        || path
            .strip_prefix(root)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Check `path`.
pub fn check(path: &str) -> Result<(), Refusal> {
    if !path.starts_with('/') || path.len() > 256 {
        return Err(Refusal::Malformed);
    }
    if path.chars().any(|c| c.is_control()) {
        return Err(Refusal::Malformed);
    }
    if path.len() > 1 {
        let body = path[1..].strip_suffix('/').unwrap_or(&path[1..]);
        if body
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        {
            return Err(Refusal::Malformed);
        }
    }
    if SECRETS.iter().any(|secret| under(path, secret)) {
        return Err(Refusal::Secret);
    }
    if ROOTS.iter().any(|root| under(path, root)) {
        Ok(())
    } else {
        Err(Refusal::Outside)
    }
}
