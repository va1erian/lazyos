//! The one-time move of the IDE's data folder.
//!
//! Before LazyRAD was a package its settings and the data of projects run from
//! it lived in `<home>/.apps/lazyrad`. A package keeps its data in
//! `<home>/.apps/<system_name>` (`docs/packages.md`), so the IDE moves the old
//! folder to `<home>/.apps/os.lazy.lazyrad` the first time it starts.

use std::io;
use std::path::Path;

use crate::platform::Home;

/// What [`migrate_legacy_data`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Migration {
    /// There is no old folder, or everything in it already has a counterpart
    /// in the new one: nothing moved.
    NotNeeded,
    /// The old folder's contents are now in the new one.
    Moved,
}

/// Moves `<home>/.apps/lazyrad` to `<home>/.apps/os.lazy.lazyrad`.
///
/// With no new folder yet this is one rename, which is atomic: an interrupted
/// start leaves one folder or the other, never half of each. If the new folder
/// exists already (a player run from a shell creates its `data` folder there
/// before the IDE ever starts) each entry of the old folder that the new one
/// lacks is moved over, and an entry both have stays in the old folder, never
/// overwritten, for the user to merge.
pub fn migrate_legacy_data(home: &Home) -> io::Result<Migration> {
    let old = home.app_data(fhs::state::LAZYRAD_LEGACY_APP);
    let new = home.app_data(fhs::state::LAZYRAD_APP);
    move_dir(&old, &new)
}

fn move_dir(old: &Path, new: &Path) -> io::Result<Migration> {
    if !old.is_dir() {
        return Ok(Migration::NotNeeded);
    }
    if !new.exists() {
        std::fs::rename(old, new)?;
        return Ok(Migration::Moved);
    }
    let mut moved = false;
    for entry in std::fs::read_dir(old)? {
        let entry = entry?;
        let target = new.join(entry.file_name());
        if !target.exists() {
            std::fs::rename(entry.path(), target)?;
            moved = true;
        }
    }
    // Only an emptied folder goes; anything left is the user's to merge.
    let _ = std::fs::remove_dir(old);
    Ok(if moved {
        Migration::Moved
    } else {
        Migration::NotNeeded
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("lazyrad-os-migrate-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_old_folder_moves_with_its_contents() {
        let dir = scratch("moves");
        let old = dir.join("old");
        let new = dir.join("new");
        fs::create_dir_all(old.join("config")).unwrap();
        fs::write(old.join("config").join("settings"), "dark").unwrap();
        assert_eq!(move_dir(&old, &new).unwrap(), Migration::Moved);
        assert!(!old.exists());
        assert_eq!(
            fs::read_to_string(new.join("config/settings")).unwrap(),
            "dark"
        );
        // A second start finds nothing to move.
        assert_eq!(move_dir(&old, &new).unwrap(), Migration::NotNeeded);
    }

    #[test]
    fn an_existing_new_folder_gets_what_it_lacks_and_is_never_overwritten() {
        let dir = scratch("merges");
        let old = dir.join("old");
        let new = dir.join("new");
        // A shell-run player made `new/data`; the old folder has its own
        // `data` and the IDE's `config`.
        fs::create_dir_all(old.join("data")).unwrap();
        fs::create_dir_all(old.join("config")).unwrap();
        fs::write(old.join("data").join("a"), "old data").unwrap();
        fs::write(old.join("config").join("settings"), "dark").unwrap();
        fs::create_dir_all(new.join("data")).unwrap();
        fs::write(new.join("data").join("b"), "new data").unwrap();
        assert_eq!(move_dir(&old, &new).unwrap(), Migration::Moved);
        assert_eq!(
            fs::read_to_string(new.join("config/settings")).unwrap(),
            "dark"
        );
        assert_eq!(fs::read_to_string(new.join("data/b")).unwrap(), "new data");
        assert!(
            !new.join("data/a").exists(),
            "the new data folder is untouched"
        );
        // What both had stays behind in the old folder.
        assert_eq!(fs::read_to_string(old.join("data/a")).unwrap(), "old data");
        assert!(!old.join("config").exists());
        // A second start finds nothing more to move.
        assert_eq!(move_dir(&old, &new).unwrap(), Migration::NotNeeded);
    }

    #[test]
    fn a_fully_merged_old_folder_is_removed() {
        let dir = scratch("empties");
        let old = dir.join("old");
        let new = dir.join("new");
        fs::create_dir_all(old.join("config")).unwrap();
        fs::create_dir_all(&new).unwrap();
        assert_eq!(move_dir(&old, &new).unwrap(), Migration::Moved);
        assert!(!old.exists());
        assert!(new.join("config").is_dir());
    }

    #[test]
    fn no_old_folder_is_nothing_to_do() {
        let dir = scratch("none");
        assert_eq!(
            move_dir(&dir.join("absent"), &dir.join("new")).unwrap(),
            Migration::NotNeeded
        );
    }

    #[test]
    fn the_folders_are_the_legacy_and_the_package_name_in_the_apps_dir() {
        let home = Home::from_var(Some(std::ffi::OsStr::new("/home/user")));
        assert_eq!(
            home.app_data(fhs::state::LAZYRAD_LEGACY_APP),
            Path::new("/home/user/.apps/lazyrad")
        );
        assert_eq!(
            home.app_data(fhs::state::LAZYRAD_APP),
            Path::new("/home/user/.apps/os.lazy.lazyrad")
        );
    }
}
