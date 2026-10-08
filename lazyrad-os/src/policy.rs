//! Which files a player's scripts may touch (decision D5): the app's private
//! data directory, its own project read-only, the documents it was started to
//! open (and their folders) read-only, nothing else.
//!
//! The runtime asks the platform for an [`FsPolicy`] each time it starts a
//! project. A [`Sandbox`] keeps the grants the user makes while the program
//! runs (a file picked in `open_file_dialog`) in an `Rc`, so it is not `Send`
//! and cannot live in the platform, which must be. [`PolicySpec`] holds the
//! same rules as plain data and builds a fresh sandbox on request.

use std::path::{Path, PathBuf};

use lazyrad_runtime::{Access, FsPolicy, Sandbox};

use crate::platform::{Home, APPS_ROOT};

/// The rules a sandbox is built from: a read/write root and extra grants.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PolicySpec {
    root: PathBuf,
    grants: Vec<Grant>,
}

/// One extra grant: a path, its access, and whether a directory is granted
/// whole or only its listing and the files directly in it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Grant {
    path: PathBuf,
    access: Access,
    children_only: bool,
}

impl PolicySpec {
    /// Read and write below `root`, nothing else yet.
    pub fn new(root: PathBuf) -> PolicySpec {
        PolicySpec {
            root,
            grants: Vec::new(),
        }
    }

    /// Also grants `path` (a file, or a directory and its contents).
    pub fn allow(mut self, path: PathBuf, access: Access) -> PolicySpec {
        self.grants.push(Grant {
            path,
            access,
            children_only: false,
        });
        self
    }

    /// Also grants the directory `dir`'s listing and the files directly in
    /// it, never its subfolders ([`Sandbox::allow_children`]; nothing for `/`).
    pub fn allow_children(mut self, dir: PathBuf, access: Access) -> PolicySpec {
        self.grants.push(Grant {
            path: dir,
            access,
            children_only: true,
        });
        self
    }

    /// A fresh sandbox with these rules.
    pub fn build(&self) -> FsPolicy {
        let sandbox = self
            .grants
            .iter()
            .fold(Sandbox::new(self.root.clone()), |sandbox, grant| {
                let path = grant.path.clone();
                if grant.children_only {
                    sandbox.allow_children(path, grant.access)
                } else {
                    sandbox.allow(path, grant.access)
                }
            });
        FsPolicy::Sandboxed(sandbox)
    }
}

/// The app id of `exe` when it runs from an installed package
/// (`/apps/<id>/<version>-<hash>/bin/<elf>`), else `None`.
pub fn installed_app_id(exe: &Path) -> Option<String> {
    let text = exe.to_string_lossy().replace('\\', "/");
    let rest = text.strip_prefix(APPS_ROOT)?.strip_prefix('/')?;
    let id = rest.split('/').next()?;
    let valid = !id.is_empty() && id != "." && id != "..";
    valid.then(|| id.to_owned())
}

/// The read/write root for a script: an installed app's
/// `<home>/.apps/<system_name>`, else `<home>/.apps/os.lazy.lazyrad/data`.
///
/// A player that runs from the IDE's own install directory (Play) is not an
/// installed app of its own: it gets the IDE's `data` folder, so a project's
/// files never mix with the IDE's `config`.
pub fn data_root(exe: &Path, home: &Home) -> PathBuf {
    match installed_app_id(exe) {
        Some(id) if id != fhs::state::LAZYRAD_APP => home.app_data(&id),
        _ => home.dev_data_dir(),
    }
}

/// The policy for a player running `project` from `exe`: read/write under
/// [`data_root`], plus read-only access to the project itself and to each of
/// `documents` and the files beside it.
///
/// A document is a file the user asked this app to open (a picture
/// double-clicked in Files); its folder's listing and the files directly in
/// it are granted so a viewer can page through the file's neighbours, as the
/// user expects of one. Subfolders stay out, and a document in `/` brings no
/// folder at all. A grant widens nothing the kernel would refuse: the player
/// still runs as the user.
pub fn player_policy(exe: &Path, project: &Path, home: &Home, documents: &[PathBuf]) -> PolicySpec {
    let mut spec = PolicySpec::new(data_root(exe, home)).allow(project.to_path_buf(), Access::Read);
    for document in documents {
        spec = spec.allow(document.clone(), Access::Read);
        if let Some(folder) = document.parent().filter(|p| !p.as_os_str().is_empty()) {
            spec = spec.allow_children(folder.to_path_buf(), Access::Read);
        }
    }
    spec
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::*;
    use crate::platform::player_beside;

    const APP_EXE: &str = "/apps/user.me.todo/1.0.0-abcd1234/bin/lrplay.elf";
    const IDE_EXE: &str = "/apps/os.lazy.lazyrad/0.1.0-abcd1234/bin/lazyrad.elf";

    fn home(path: &str) -> Home {
        Home::from_var(Some(OsStr::new(path)))
    }

    /// A scratch folder (a directory grant needs the directory to exist).
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lazyrad-os-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn an_installed_app_writes_its_own_folder_in_the_home() {
        let exe = Path::new(APP_EXE);
        assert_eq!(installed_app_id(exe).as_deref(), Some("user.me.todo"));
        assert_eq!(
            data_root(exe, &home("/home/user")),
            Path::new("/home/user/.apps/user.me.todo")
        );
        assert!(!data_root(exe, &home("/home/user")).starts_with(APPS_ROOT));
    }

    #[test]
    fn a_dev_run_uses_lazyrads_own_data_folder() {
        // A shell run, and Play: the player beside the installed IDE.
        let play = player_beside(Path::new(IDE_EXE));
        assert_eq!(
            play,
            Path::new("/apps/os.lazy.lazyrad/0.1.0-abcd1234/bin/lrplay.elf")
        );
        assert_eq!(
            data_root(&play, &home("/home/admin")),
            Path::new("/home/admin/.apps/os.lazy.lazyrad/data")
        );
        let exe = Path::new("/transient/lrplay.elf");
        assert_eq!(installed_app_id(exe), None);
        assert_eq!(
            data_root(exe, &home("/home/admin")),
            Path::new("/home/admin/.apps/os.lazy.lazyrad/data")
        );
        assert_eq!(
            data_root(exe, &Home::from_var(None)),
            Path::new("/transient/lazyrad/.apps/os.lazy.lazyrad/data")
        );
    }

    #[test]
    fn odd_paths_are_not_app_ids() {
        for bad in [
            "/apps/",
            "/apps//x/bin/a",
            "/apps/../bin/a",
            "/appsx/y/bin/a",
        ] {
            assert_eq!(installed_app_id(Path::new(bad)), None, "{bad}");
        }
    }

    #[test]
    fn the_player_policy_is_private_data_plus_a_read_only_project() {
        let project = scratch("policy");
        let file = project.join("main.lfm");
        std::fs::write(&file, "x").unwrap();
        let file = file.to_string_lossy().into_owned();
        let user = home("/home/user");

        let policy = player_policy(Path::new(APP_EXE), &project, &user, &[]).build();
        let own = policy
            .resolve("notes.txt", Access::Write)
            .expect("own data");
        let own = own.to_string_lossy().replace('\\', "/");
        assert!(
            own.ends_with("/home/user/.apps/user.me.todo/notes.txt"),
            "{own}"
        );
        assert!(
            policy.resolve(&file, Access::Read).is_ok(),
            "the project is readable"
        );
        assert!(
            policy.resolve(&file, Access::Write).is_err(),
            "the project is read-only"
        );
        assert!(policy.resolve("/system/etc/passwd", Access::Read).is_err());
        assert!(policy.resolve("../other/x", Access::Read).is_err());
        let _ = std::fs::remove_dir_all(&project);
    }

    #[test]
    fn a_document_and_its_folder_are_readable_but_not_writable() {
        let project = scratch("doc-project");
        let pictures = scratch("doc-pictures");
        let opened = pictures.join("a.png");
        let neighbour = pictures.join("b.png");
        std::fs::write(&opened, "a").unwrap();
        std::fs::write(&neighbour, "b").unwrap();
        std::fs::create_dir_all(pictures.join("private")).unwrap();
        let nested = pictures.join("private/d.png");
        std::fs::write(&nested, "d").unwrap();
        let elsewhere = scratch("doc-elsewhere").join("c.png");
        std::fs::write(&elsewhere, "c").unwrap();

        let policy = player_policy(
            Path::new(APP_EXE),
            &project,
            &home("/home/user"),
            std::slice::from_ref(&opened),
        )
        .build();
        let text = |p: &Path| p.to_string_lossy().into_owned();
        assert!(policy.resolve(&text(&opened), Access::Read).is_ok());
        assert!(policy.resolve(&text(&neighbour), Access::Read).is_ok());
        assert!(
            policy.resolve(&text(&pictures), Access::Read).is_ok(),
            "listable"
        );
        assert!(policy.resolve(&text(&opened), Access::Write).is_err());
        assert!(policy.resolve(&text(&elsewhere), Access::Read).is_err());
        assert!(
            policy.resolve(&text(&nested), Access::Read).is_err(),
            "a subfolder of the document's folder stays out"
        );
        for dir in [project, pictures] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn each_build_is_a_fresh_sandbox() {
        let spec = PolicySpec::new(PathBuf::from("/home/user/.apps/x"));
        let FsPolicy::Sandboxed(first) = spec.build() else {
            panic!("a sandbox");
        };
        first.allow_runtime(PathBuf::from("/picked.png"), Access::Read);
        let second = spec.build();
        assert!(
            second.resolve("/picked.png", Access::Read).is_err(),
            "a runtime grant stays with the run that made it"
        );
    }
}
