//! The account database on disk: load it at start, write it back after a
//! change, and keep its two public views in step.
//!
//! A change is written whole to `db.new` (made private), flushed, and renamed
//! over `db`, so a crash leaves either the old database or the new one, never
//! half of each. The views (`/system/etc/passwd`, `/system/etc/group`) are
//! files `_accounts` owns in root's directory: rewritten in place, and
//! regenerated from the database at every start, so a damaged or outdated
//! view lasts at most until then.

use alloc::format;
use alloc::string::{String, ToString};

use accountdb::{Db, LoadError};
use user::files;
use user::sys;

/// `ENOENT` and `EFBIG` from `user::files`.
const ENOENT: i64 = 2;
const EFBIG: i64 = 27;

/// Read and parse the database. `Err` carries the `reason=` text.
pub(crate) fn load() -> Result<Db, String> {
    let path = fhs::state::ACCOUNTS_DB;
    let bytes = match files::read_up_to(path, accountdb::DB_MAX) {
        Ok(bytes) => bytes,
        Err(ENOENT) => return Err(LoadError::Missing.to_string()),
        Err(EFBIG) => {
            let size = files::stat(path)
                .map(|(size, _)| size as usize)
                .unwrap_or(0);
            return Err(LoadError::Oversize(size).to_string());
        }
        Err(code) => return Err(LoadError::Unreadable(code).to_string()),
    };
    accountdb::parse(&bytes).map_err(|error| error.to_string())
}

/// Write `db` back atomically, then its views. `Err` is the text the caller
/// reports; the database on disk is then the old one.
pub(crate) fn persist(db: &Db) -> Result<(), String> {
    let text = db.to_text();
    let fresh = fhs::state::ACCOUNTS_DB_NEW;
    files::write_large(fresh, text.as_bytes())
        .and_then(|()| files::chmod(fresh, 0o600))
        .and_then(|()| files::fsync(fresh))
        .and_then(|()| files::rename(fresh, fhs::state::ACCOUNTS_DB))
        .map_err(|code| {
            let _ = files::remove(fresh);
            format!("the account database could not be written (errno {code})")
        })?;
    write_views(db);
    Ok(())
}

/// Rewrite the views that differ from `db`'s. A failure is reported, not
/// fatal: the database is the truth and the next start tries again.
pub(crate) fn write_views(db: &Db) {
    for (path, text) in [
        (fhs::etc::PASSWD, db.passwd_view()),
        (fhs::etc::GROUP, db.group_view()),
    ] {
        let current = files::read_up_to(path, accountdb::DB_MAX).ok();
        if current.as_deref() == Some(text.as_bytes()) {
            continue;
        }
        match files::write_file(path, text.as_bytes()) {
            Ok(()) => sys::write_str(&format!("ACCOUNTS:VIEW:PASS file={path}\n")),
            Err(code) => sys::write_str(&format!("ACCOUNTS:VIEW:FAIL file={path} errno={code}\n")),
        }
    }
}
