//! The command line of `lrplay`.
//!
//! Three callers produce it, and all are treated as untrusted input:
//!
//! * `init`'s launcher: `<app> [--client] [<path>] attempt=N`, where `<path>` is
//!   a project directory or `.lrp` (from `mimed` open-with or another app);
//! * a package manifest's `[entry] args`, for example
//!   `["--project", "resources/project"]`;
//! * a developer at a shell.
//!
//! # Where a produced app finds its project (decision D1a)
//!
//! An installed `.lzp` lives at `/data/apps/<system_name>/<version>-<hash>/`
//! with the player at `bin/lrplay.elf` and the project at `resources/project/`.
//! The install directory name contains a per-version hash, so a manifest cannot
//! hard-code an absolute path. Rather than depend on the installer setting a
//! working directory, `lrplay` resolves paths against **its own install
//! directory** (the parent of the `bin/` folder holding the running binary):
//!
//! * `--project <relative>` is joined to the install directory
//!   (`resources/project` -> `<install>/resources/project`); a relative path may
//!   not contain `..`;
//! * `--project <absolute>` and a bare positional path are used as given (a
//!   developer's `lrplay /system/share/lazyrad/hello`, or `mimed` opening a `.lrp`);
//! * with neither, `<install>/resources/project` is used when it exists, so the
//!   packager writes `args = []`.
//!
//! # Finding: `current_exe()` is unusable on LazyOS today
//!
//! The kernel hard-codes `/proc/self/exe` to `/busybox` (`readlink` in
//! `kernel/src/process/linux/pathops.rs`), so `std::env::current_exe()` answers
//! `/busybox` for every program (verified in the P1 session:
//! `LRPLAY:PROJECT:PASS:... exe=/busybox`). `lrplay` therefore locates itself
//! from `argv[0]` ([`exe_from_argv0`]): an absolute path is used as is, a
//! relative one is joined to the working directory, and a bare name (looked up
//! through `PATH`, or no `argv[0]` at all) is assumed to sit in the working
//! directory. **Integration requirement for `pkgd`/`init`**: start an installed
//! app with its absolute path as `argv[0]`, or with the install directory as the
//! working directory, or give `--project` an absolute path.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

/// The longest accepted path argument, in bytes (same bound as
/// `xui_app::platform::argv::MAX_PATH_BYTES`: a hostile argument must not force
/// a large allocation).
pub const MAX_PATH_BYTES: usize = 4096;

/// The project directory inside an install directory.
pub const DEFAULT_PROJECT: &str = "resources/project";

/// What the command line asked for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlayerArgs {
    /// `--client` was present (the backend also detects the mode itself).
    pub client: bool,
    /// The project named by `--project` or positionally, unresolved.
    pub project: Option<PathBuf>,
}

/// Why the command line was refused. `Display` is one line, shown on serial.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArgError {
    /// A flag this program does not know.
    UnknownFlag(String),
    /// `--project` with nothing after it.
    MissingValue,
    /// A project path given twice (flag and positional, or two positionals).
    DuplicateProject,
    /// The path is empty, too long, or contains NUL or control characters.
    BadPath,
    /// A relative path that would leave the install directory.
    Escapes(String),
    /// The project directory the install layout implies does not exist.
    NoProject(PathBuf),
}

impl std::fmt::Display for ArgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArgError::UnknownFlag(flag) => write!(f, "unknown option `{flag}`"),
            ArgError::MissingValue => write!(f, "--project needs a path"),
            ArgError::DuplicateProject => write!(f, "more than one project path"),
            ArgError::BadPath => write!(f, "the project path is not valid"),
            ArgError::Escapes(path) => write!(f, "`{path}` leaves the install directory"),
            ArgError::NoProject(path) => write!(f, "no project at `{}`", path.display()),
        }
    }
}

/// Parses `args` (the command line without the program name).
pub fn parse_player<I>(args: I) -> Result<PlayerArgs, ArgError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut parsed = PlayerArgs::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let text = arg.to_string_lossy();
        if text == "--client" {
            parsed.client = true;
        } else if text.starts_with("attempt=") {
            // `init`'s retry counter; meaningless here.
        } else if text == "--project" {
            let value = args.next().ok_or(ArgError::MissingValue)?;
            set_project(&mut parsed, value)?;
        } else if let Some(value) = text.strip_prefix("--project=") {
            set_project(&mut parsed, OsString::from(value))?;
        } else if text.starts_with('-') {
            return Err(ArgError::UnknownFlag(text.into_owned()));
        } else {
            set_project(&mut parsed, arg)?;
        }
    }
    Ok(parsed)
}

/// Records the project path, refusing a second one or a malformed one.
fn set_project(parsed: &mut PlayerArgs, value: OsString) -> Result<(), ArgError> {
    if parsed.project.is_some() {
        return Err(ArgError::DuplicateProject);
    }
    let path = PathBuf::from(value);
    let text = path.to_string_lossy();
    if text.is_empty() || text.len() > MAX_PATH_BYTES || text.chars().any(char::is_control) {
        return Err(ArgError::BadPath);
    }
    parsed.project = Some(path);
    Ok(())
}

/// The path of the running program, from its `argv[0]` and the working
/// directory (see the module documentation for why not `current_exe`).
pub fn exe_from_argv0(argv0: Option<&std::ffi::OsStr>, cwd: &Path) -> PathBuf {
    let Some(argv0) = argv0.filter(|arg| !arg.is_empty()) else {
        return cwd.join("lrplay.elf");
    };
    let path = Path::new(argv0);
    if is_absolute(path) {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

/// The install directory of the program at `exe`: the parent of its `bin/`
/// folder, or the program's own folder when it does not sit in one.
pub fn install_dir(exe: &Path) -> PathBuf {
    let Some(dir) = exe.parent() else {
        return PathBuf::from("/");
    };
    let in_bin = dir.file_name().is_some_and(|name| name == "bin");
    match (in_bin, dir.parent()) {
        (true, Some(parent)) => parent.to_path_buf(),
        _ => dir.to_path_buf(),
    }
}

/// Whether `path` is absolute on LazyOS (a leading `/`). `Path::is_absolute`
/// would say no on a Windows host running the unit tests.
fn is_absolute(path: &Path) -> bool {
    path.is_absolute() || path.to_string_lossy().starts_with('/')
}

/// The project directory to run: see the module documentation for the rules.
///
/// `project_exists` answers whether the implied default directory exists (so the
/// function stays pure and testable).
pub fn resolve_project(
    requested: Option<&Path>,
    exe: &Path,
    project_exists: impl Fn(&Path) -> bool,
) -> Result<PathBuf, ArgError> {
    let install = install_dir(exe);
    match requested {
        Some(path) if is_absolute(path) => Ok(path.to_path_buf()),
        Some(path) => {
            let clean = path
                .components()
                .all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
            if clean {
                Ok(install.join(path))
            } else {
                Err(ArgError::Escapes(path.display().to_string()))
            }
        }
        None => {
            let default = install.join(DEFAULT_PROJECT);
            if project_exists(&default) {
                Ok(default)
            } else {
                Err(ArgError::NoProject(default))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<PlayerArgs, ArgError> {
        parse_player(args.iter().map(OsString::from))
    }

    #[test]
    fn an_empty_command_line_asks_for_nothing() {
        assert_eq!(parse(&[]), Ok(PlayerArgs::default()));
    }

    #[test]
    fn the_launcher_line_is_understood() {
        let args = parse(&["--client", "/system/share/lazyrad/hello", "attempt=1"]).unwrap();
        assert!(args.client);
        assert_eq!(
            args.project,
            Some(PathBuf::from("/system/share/lazyrad/hello"))
        );
    }

    #[test]
    fn a_manifest_entry_is_understood_in_both_spellings() {
        for form in [
            &["--project", "resources/project"][..],
            &["--project=resources/project"][..],
        ] {
            let args = parse(form).unwrap();
            assert_eq!(args.project, Some(PathBuf::from("resources/project")));
        }
    }

    #[test]
    fn malformed_command_lines_are_refused() {
        assert_eq!(parse(&["--project"]), Err(ArgError::MissingValue));
        assert_eq!(parse(&["a", "b"]), Err(ArgError::DuplicateProject));
        assert_eq!(
            parse(&["--project", "a", "b"]),
            Err(ArgError::DuplicateProject)
        );
        assert_eq!(parse(&["--project="]), Err(ArgError::BadPath));
        assert_eq!(parse(&["--project", "a\nb"]), Err(ArgError::BadPath));
        assert!(matches!(parse(&["--wat"]), Err(ArgError::UnknownFlag(_))));
        let long = "x".repeat(MAX_PATH_BYTES + 1);
        assert_eq!(parse(&[&long]), Err(ArgError::BadPath));
    }

    #[test]
    fn the_install_directory_is_the_parent_of_bin() {
        let exe = Path::new("/data/apps/user.me.todo/1.0.0-abcd1234/bin/lrplay.elf");
        assert_eq!(
            install_dir(exe),
            Path::new("/data/apps/user.me.todo/1.0.0-abcd1234")
        );
        assert_eq!(
            install_dir(Path::new(fhs::bin::LRPLAY)),
            Path::new(fhs::SYSTEM)
        );
    }

    #[test]
    fn the_program_is_located_from_argv0_and_the_working_directory() {
        use std::ffi::OsStr;
        let cwd = Path::new("/data/apps/a.b.c/1.0.0-ff");
        let abs = exe_from_argv0(
            Some(OsStr::new("/data/apps/a.b.c/1.0.0-ff/bin/lrplay.elf")),
            cwd,
        );
        assert_eq!(install_dir(&abs), cwd);
        let rel = exe_from_argv0(Some(OsStr::new("bin/lrplay.elf")), cwd);
        assert_eq!(install_dir(&rel), cwd);
        // A bare name or nothing at all: the working directory is the install.
        assert_eq!(
            install_dir(&exe_from_argv0(Some(OsStr::new("lrplay.elf")), cwd)),
            cwd
        );
        assert_eq!(install_dir(&exe_from_argv0(None, cwd)), cwd);
        assert_eq!(install_dir(&exe_from_argv0(Some(OsStr::new("")), cwd)), cwd);
    }

    #[test]
    fn a_relative_project_resolves_against_the_install_directory() {
        let exe = Path::new("/data/apps/a.b.c/1.0.0-ff/bin/lrplay.elf");
        let got = resolve_project(Some(Path::new("resources/project")), exe, |_| false).unwrap();
        assert_eq!(
            got,
            Path::new("/data/apps/a.b.c/1.0.0-ff/resources/project")
        );
    }

    #[test]
    fn a_relative_project_cannot_climb_out_of_the_install_directory() {
        let exe = Path::new("/data/apps/a.b.c/1.0.0-ff/bin/lrplay.elf");
        for bad in ["../other", "resources/../../x"] {
            assert!(matches!(
                resolve_project(Some(Path::new(bad)), exe, |_| true),
                Err(ArgError::Escapes(_))
            ));
        }
    }

    #[test]
    fn an_absolute_project_is_used_as_given() {
        let exe = Path::new("/system/bin/lrplay");
        let got = resolve_project(Some(Path::new("/system/share/lazyrad/hello")), exe, |_| {
            false
        })
        .unwrap();
        assert_eq!(got, Path::new("/system/share/lazyrad/hello"));
    }

    #[test]
    fn no_argument_uses_the_packaged_project_when_it_exists() {
        let exe = Path::new("/data/apps/a.b.c/1.0.0-ff/bin/lrplay.elf");
        let want = Path::new("/data/apps/a.b.c/1.0.0-ff/resources/project");
        assert_eq!(resolve_project(None, exe, |p| p == want).unwrap(), want);
        assert_eq!(
            resolve_project(None, exe, |_| false),
            Err(ArgError::NoProject(want.to_path_buf()))
        );
    }
}
