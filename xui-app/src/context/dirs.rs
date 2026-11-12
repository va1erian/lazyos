//! The well-known folders of the running user, all from `libs/fhs`.

use std::path::PathBuf;

use crate::platform::dirs;

/// Home and what lives under it. Nothing here touches the disk except
/// [`Dirs::start`], which checks that home exists.
pub struct Dirs {
    system_name: Option<String>,
}

impl Dirs {
    pub(super) fn new(system_name: Option<String>) -> Dirs {
        Dirs { system_name }
    }

    /// `$HOME`, when it is an absolute path.
    pub fn home(&self) -> Option<PathBuf> {
        dirs::home()
    }

    /// Where a picker opens: home when it exists, else `/transient`.
    pub fn start(&self) -> PathBuf {
        dirs::default_dir()
    }

    /// `<home>/Documents` (it may not exist yet).
    pub fn documents(&self) -> Option<PathBuf> {
        Some(self.home()?.join("Documents"))
    }

    /// `<home>/Downloads`, where LazyWeb saves.
    pub fn downloads(&self) -> Option<PathBuf> {
        Some(self.home()?.join(lazydownloads::FOLDER))
    }

    /// `<home>/.apps/<system name>`: the app's own files. `None` without a
    /// home or when the builder was not told the app's system name
    /// (`AppBuilder::system_name`).
    pub fn app_data(&self) -> Option<PathBuf> {
        let home = self.home()?;
        let name = self.system_name.as_deref()?;
        Some(PathBuf::from(fhs::app_data_dir(home.to_str()?, name)))
    }

    /// `/transient`: the ramfs every boot has.
    pub fn transient(&self) -> PathBuf {
        PathBuf::from(fhs::mount::TRANSIENT)
    }

    /// `/tmp`.
    pub fn tmp(&self) -> PathBuf {
        PathBuf::from(fhs::mount::TMP)
    }
}
