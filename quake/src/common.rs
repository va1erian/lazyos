//! The file system — common.c's `COM_InitFilesystem` on this platform: the
//! base directory (`-basedir`, default `.`), the game directory `id1` under
//! it and id's search path through it ([`quake_rs::common`]: `pak0.pak`,
//! the player's `pak1.pak` if they added it, the loose files), and
//! `COM_WriteFile`/`COM_LoadFile` for the game's own files (the saves,
//! `config.cfg`), which live in the game directory as in id's. All of it is
//! `std::fs`: in the browser the WASI host in `web/wasi.js` serves the calls
//! from the page's storage; natively they are the real disk.
//!
//! **LazyOS change** (the one file the port replaces; this is quake-wasm's
//! `common.rs` at the pinned revision plus the data directory): the pak and
//! the map files are read from the package's read-only install directory
//! (`-basedir`), but the saves and `config.cfg` must be written under the
//! player's home, which `init` hands installed apps as `$HOME`
//! (`tools/pkg`'s write permission `write:$HOME/.apps/...`). So the
//! platform layer can name its data directory once before the first frame
//! ([`set_data_dir`]), and the writes and the reads of the game's own files
//! go there while everything else stays id's search path. A read falls back
//! to the game directory, the way a stored `config.cfg` written by an older
//! layout would still work.

use std::cell::RefCell;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use quake_rs::common::{Filesystem, check_progs, init_filesystem};
use quake_rs::pak::Pak;

/// `GAMENAME` (quakedef.h): the game directory under the base directory.
pub(crate) const GAMENAME: &str = quake_rs::common::GAMENAME;

/// The search path and what `COM_CheckRegistered` found, set once by
/// [`init`] (or, in tests, the shareware pak next to the crate on first
/// use).
static FILES: OnceLock<Option<Filesystem>> = OnceLock::new();

/// The data directory the platform named ([`set_data_dir`]), or `None`.
static DATA_DIR: OnceLock<Option<PathBuf>> = OnceLock::new();

thread_local! {
    /// `com_gamedir`. A thread-local so each test gets its own directory,
    /// the way each test gets its own `App`.
    static GAMEDIR: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

/// Name the directory the saves and `config.cfg` go in; called once, before
/// the first frame (LazyOS: the package's per-user folder in the player's
/// home, `/tmp/quake` when there is no home). Later calls are ignored, like
/// everywhere else a `OnceLock` names a thing.
pub(crate) fn set_data_dir(dir: PathBuf) {
    let _ = DATA_DIR.set(Some(dir));
}

/// `COM_InitFilesystem` and `COM_CheckRegistered`, then `PR_LoadProgs`'s
/// checks of the game's `progs.dat`: the search path opened as `quaketool`
/// opens a pak — each directory now, each file's bytes when it is read
/// ([`Pak::open`]), so no copy of an archive lives in the program. `mod_dirs`
/// and `force_modified` are `-rogue`/`-hipnotic`/`-game <dir>`, parsed by
/// `main.rs` ([`init_filesystem`]'s doc has id's order). Returns what id
/// printed on the way (the packs, "Playing … version."); an `Err` is the
/// `Sys_Error` the game stops with.
pub(crate) fn init(basedir: &Path, mod_dirs: &[&str], force_modified: bool) -> Result<Vec<String>, String> {
    let fs = init_filesystem(basedir, mod_dirs, force_modified)?;
    check_progs(&fs.files)?;
    let log = fs.log.clone();
    GAMEDIR.with(|g| *g.borrow_mut() = Some(fs.gamedir.clone()));
    // A second init (tests only) keeps the first path: it is the same files.
    let _ = FILES.set(Some(fs));
    Ok(log)
}

/// The search path's head (the directories; entries read on demand), or
/// `None` before [`init`]. Cloning copies the first pack's directory only.
pub(crate) fn pak() -> Option<Pak> {
    files().map(|f| f.files.clone())
}

/// `static_registered`: the path holds id's `gfx/pop.lmp`.
pub(crate) fn registered() -> bool {
    files().is_some_and(|f| f.registered)
}

/// `COM_Path_f`'s lines (nothing before [`init`]).
pub(crate) fn path_lines() -> Vec<String> {
    files().map(|f| quake_rs::common::path_lines(&f.files)).unwrap_or_default()
}

fn files() -> Option<&'static Filesystem> {
    FILES.get_or_init(default_files).as_ref()
}

/// The tests read the shareware pak where the repo keeps it, alone on the
/// path (unregistered). On LazyOS the <build tree>'s sibling `quake-data/`
/// (the assembly the fetch leaves next to the crate) is that directory, as
/// quake-srp's own CI keeps it under `quake-data/` beside `quake-wasm/`.
#[cfg(test)]
fn default_files() -> Option<Filesystem> {
    let pak = Pak::open(concat!(env!("CARGO_MANIFEST_DIR"), "/../quake-data/ID1/PAK0.PAK")).ok()?;
    // (The tests' game directory is each thread's own: `gamedir`.)
    Some(Filesystem {
        files: pak,
        gamedir: PathBuf::from(GAMENAME),
        registered: false,
        modified: false,
        log: Vec::new(),
    })
}

#[cfg(not(test))]
fn default_files() -> Option<Filesystem> {
    None
}

/// `com_gamedir`: where the saves and `config.cfg` go.
pub(crate) fn gamedir() -> PathBuf {
    GAMEDIR.with(|g| g.borrow_mut().get_or_insert_with(default_gamedir).clone())
}

/// The program's game directory before [`init`] runs: `./id1`.
#[cfg(not(test))]
fn default_gamedir() -> PathBuf {
    PathBuf::from(GAMENAME)
}

/// Each test thread gets an empty directory of its own under `target/`
/// (never `/tmp`), wiped on first use so a previous run's files cannot leak
/// in.
#[cfg(test)]
fn default_gamedir() -> PathBuf {
    let id = format!("{:?}", std::thread::current().id());
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-gamedirs")
        .join(id.trim_start_matches("ThreadId(").trim_end_matches(')'));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("test gamedir");
    dir
}

/// `COM_WriteFile`: `name` (relative to the game directory) with `data`.
pub(crate) fn write_file(name: &str, data: &[u8]) -> io::Result<()> {
    match data_dir() {
        Some(dir) => std::fs::write(dir.join(name), data),
        None => std::fs::write(gamedir().join(name), data),
    }
}

/// `COM_LoadFile` for the game directory's own files (not the pak's): the
/// data directory first, the game directory as the fallback.
pub(crate) fn read_file(name: &str) -> io::Result<Vec<u8>> {
    if let Some(dir) = data_dir() {
        if let Ok(bytes) = std::fs::read(dir.join(name)) {
            return Ok(bytes);
        }
    }
    std::fs::read(gamedir().join(name))
}

fn data_dir() -> Option<&'static PathBuf> {
    DATA_DIR.get().and_then(Option::as_ref)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_round_trip_through_the_game_directory() {
        assert!(read_file("s7.sav").is_err(), "a fresh gamedir is empty");
        write_file("s7.sav", b"SAVEGAME").unwrap();
        assert_eq!(read_file("s7.sav").unwrap(), b"SAVEGAME");
        assert!(gamedir().join("s7.sav").exists());
    }

    #[test]
    fn the_pak_opens_by_directory_and_reads_on_demand() {
        let pak = pak().expect("the shareware pak");
        assert!(pak.find("maps/e1m1.bsp").is_some());
        let palette = pak.read_file("gfx/palette.lmp").unwrap().unwrap();
        assert_eq!(palette.len(), 768);
        assert!(!registered());
    }

    #[test]
    fn init_needs_a_game_directory_with_ids_pak0() {
        // An empty game directory: no pak0.pak, so no progs.dat on the path.
        let empty = gamedir().join("empty-base");
        std::fs::create_dir_all(empty.join(GAMENAME)).unwrap();
        let err = init(&empty, &[], false).unwrap_err();
        assert_eq!(err, "PR_LoadProgs: couldn't load progs.dat");
    }

    #[test]
    fn the_data_directory_serves_the_game_s_own_files() {
        // In the tests the data directory has not been named: the fallback
        // (the game directory) is what the port's bridge sets before the
        // first frame.
        assert!(data_dir().is_none(), "the tests leave the platform's choice out");
        write_file("config.cfg", b"// fallback\n").unwrap();
        assert_eq!(read_file("config.cfg").unwrap(), b"// fallback\n");
    }
}
