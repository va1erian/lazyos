//! The state the running system owns on the OS volume, which an update must
//! respect or repair instead of overwriting (docs/accounts-plan.md U1):
//!
//! * **the account database's home.** Images built while the database lived
//!   in `/conf/accounts` (and `/conf` was 0711 so `_accounts` could reach it)
//!   get it moved to its own top-level directory, and the old directory
//!   removed, so `/conf` can be root's alone again;
//! * **confd's store files** are made private (0600): `confd` wrote them
//!   0644, which only the directory's mode kept from other users;
//! * **the accounts' homes** are seed directories ([`DirSpec::seed`]): made
//!   with the database, when it is seeded (a fresh volume, or one that never
//!   had a database), and never created, chmodded or chowned by an update,
//!   so a deleted account's home does not come back and a home keeps the
//!   owner the running system gave it.

use ext2fs::{AttrChange, Ext2, Ext2Error, FileKind};

use super::volume_error;
use crate::os_layout::DirSpec;

/// Move a database kept in `/conf/accounts` to [`fhs::state::ACCOUNTS_DB`]
/// (unless one is there already, which then wins), then remove the old
/// directory with whatever else it held (a `db.new` an interrupted write
/// left). Nothing to do on a volume without the old directory.
pub(super) fn move_legacy_accounts(volume: &Ext2) -> Result<(), String> {
    let legacy = fhs::state::LEGACY_ACCOUNTS_DB;
    if volume.lookup(fhs::state::LEGACY_ACCOUNTS_DIR).is_err() {
        return Ok(());
    }
    if volume.lookup(legacy).is_ok() && volume.lookup(fhs::state::ACCOUNTS_DB).is_err() {
        volume
            .rename(legacy, fhs::state::ACCOUNTS_DB)
            .map_err(|e| volume_error(&format!("move {legacy}"), e))?;
        println!(
            "cargo:warning=moved the account database {legacy} to {}",
            fhs::state::ACCOUNTS_DB
        );
    }
    volume
        .remove_tree(fhs::state::LEGACY_ACCOUNTS_DIR)
        .map_err(|e| volume_error(&format!("remove {}", fhs::state::LEGACY_ACCOUNTS_DIR), e))
}

/// Make every file `confd` keeps directly in `/conf` private to its owner
/// (0600): the store, its temporary, corrupt and migrated copies, and the
/// seed marker. Directories (`/conf/svc`) keep the table's mode.
pub(super) fn private_conf_files(volume: &Ext2) -> Result<(), String> {
    let dir = fhs::state::CONF_ROOT;
    let entries = match volume.readdir(dir) {
        Ok(entries) => entries,
        Err(Ext2Error::NotFound) => return Ok(()),
        Err(error) => return Err(volume_error(&format!("list {dir}"), error)),
    };
    for entry in entries {
        if entry.kind != FileKind::File {
            continue;
        }
        let path = format!("{dir}/{}", entry.name);
        let meta = volume
            .lookup(&path)
            .map_err(|e| volume_error(&format!("stat {path}"), e))?;
        let mode = meta.mode & 0o7777;
        if mode & 0o077 == 0 {
            continue;
        }
        let change = AttrChange {
            mode: Some(mode & 0o700),
            ..AttrChange::default()
        };
        volume
            .setattr(&path, &change)
            .map_err(|e| volume_error(&format!("chmod {path}"), e))?;
    }
    Ok(())
}

/// Whether this write seeds the account database: the volume has none yet.
/// Asked after [`move_legacy_accounts`], so a moved database counts.
pub(super) fn seeding_accounts(volume: &Ext2) -> bool {
    volume.lookup(fhs::state::ACCOUNTS_DB).is_err()
}

/// Create the seed directories of `dirs` that do not exist, when `seeding`;
/// an existing one is never touched, and nothing happens otherwise.
pub(super) fn seed_dirs(volume: &Ext2, dirs: &[DirSpec], seeding: bool) -> Result<(), String> {
    if !seeding {
        return Ok(());
    }
    for dir in dirs.iter().filter(|dir| dir.seed) {
        if volume.lookup(&dir.path).is_ok() {
            continue;
        }
        volume
            .mkdir_p(&dir.path, dir.mode, dir.uid, dir.gid)
            .map_err(|e| volume_error(&format!("mkdir {}", dir.path), e))?;
        // `mkdir_p` applies the umask-free mode, but say it outright: a home
        // is private from its first block.
        let change = AttrChange {
            mode: Some(dir.mode),
            uid: Some(dir.uid),
            gid: Some(dir.gid),
            ..AttrChange::default()
        };
        volume
            .setattr(&dir.path, &change)
            .map_err(|e| volume_error(&format!("chmod {}", dir.path), e))?;
    }
    Ok(())
}
