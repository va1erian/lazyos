//! The command line: what the wrapper itself reads, and the argv the engine
//! gets.
//!
//! Three callers start `lazydoom`, and all of them are untrusted input:
//!
//! - `init`, for the installed package: `<install>/bin/doom.elf --client
//!   attempt=N` (the manifest's `entry.args`, then `init`'s own suffix);
//! - a developer or a test at a shell: `doom.elf -headless -timedemo demo1`;
//! - any extra engine flags (`-warp 1 1`, `-skill 4`), passed through.
//!
//! The wrapper consumes `--client` (its default anyway), `attempt=N`,
//! `-headless` and `-frames N`; everything else reaches the engine. Unless the
//! caller names one, the IWAD is the Freedoom WAD shipped in the package, found
//! from the install directory: `/proc/self/exe` is not usable on LazyOS (see
//! `lazyrad-os/src/args.rs`), so the binary locates itself from `argv[0]`,
//! which `init` sets to the absolute spawn path.

/// The IWAD inside an install directory. Its file name matters: the engine
/// identifies the game by it (`d_iwad.c`), and `freedoom1.wad` is Freedoom:
/// Phase 1.
pub const PACKAGED_IWAD: &str = "resources/freedoom1.wad";
/// Longest argument kept, in bytes; a longer one is dropped rather than
/// copied into the engine's argv.
pub const MAX_ARG_BYTES: usize = 4096;
/// Most arguments handed to the engine.
pub const MAX_ARGS: usize = 64;

/// How the wrapper runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// A desktop window on `xuid` (the default).
    Window,
    /// No display: every frame is checksummed, and after `frames` frames (if
    /// set) the checksum is printed and the program exits.
    Headless { frames: Option<u32> },
}

/// The parsed command line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launch {
    pub mode: Mode,
    /// The engine's argv, `argv[0]` first, `-iwad` included.
    pub engine_args: Vec<String>,
    /// The IWAD the engine will open.
    pub iwad: String,
}

/// Parse `args` (`argv[0]` first). `cwd` resolves a relative `argv[0]`.
pub fn parse(args: &[String], cwd: &str) -> Launch {
    let argv0 = args.first().map(String::as_str).unwrap_or("");
    let mut mode = Mode::Window;
    let mut engine_args = vec![if argv0.is_empty() {
        "doom".to_string()
    } else {
        argv0.to_string()
    }];
    let mut iwad = None;
    let mut rest = args.iter().skip(1);
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--client" => {}
            "-headless" => {
                if mode == Mode::Window {
                    mode = Mode::Headless { frames: None };
                }
            }
            "-frames" => {
                let frames = rest.next().and_then(|count| count.parse().ok());
                mode = Mode::Headless { frames };
            }
            "-iwad" => iwad = rest.next().cloned(),
            other if other.starts_with("attempt=") => {}
            other if other.len() <= MAX_ARG_BYTES && engine_args.len() < MAX_ARGS => {
                engine_args.push(other.to_string());
            }
            _ => {}
        }
    }
    // Absolute, because the wrapper changes into the config directory before
    // the engine opens it.
    let iwad = match iwad.filter(|path| !path.is_empty() && path.len() <= MAX_ARG_BYTES) {
        Some(path) if path.starts_with('/') => path,
        Some(path) => join(cwd, path.trim_start_matches("./")),
        None => join(&install_dir(&exe_path(argv0, cwd)), PACKAGED_IWAD),
    };
    engine_args.push("-iwad".to_string());
    engine_args.push(iwad.clone());
    Launch {
        mode,
        engine_args,
        iwad,
    }
}

/// `rel` under `dir`, joined with `/` whatever the host (these are LazyOS
/// paths; the host only runs the tests).
fn join(dir: &str, rel: &str) -> String {
    format!("{}/{rel}", dir.trim_end_matches('/'))
}

/// The running binary's path from `argv[0]`: absolute as given, relative to
/// `cwd` otherwise (a bare name run from its own directory).
pub fn exe_path(argv0: &str, cwd: &str) -> String {
    if argv0.is_empty() {
        join(cwd, "doom.elf")
    } else if argv0.starts_with('/') {
        argv0.to_string()
    } else {
        join(cwd, argv0.trim_start_matches("./"))
    }
}

/// The install directory of a packaged binary: the parent of the `bin/`
/// directory holding it, or the binary's own directory when it is not in a
/// `bin/` (a developer's copy next to a WAD).
pub fn install_dir(exe: &str) -> String {
    let parent = |path: &str| -> String {
        match path.trim_end_matches('/').rsplit_once('/') {
            Some(("", _)) | None => "/".to_string(),
            Some((dir, _)) => dir.to_string(),
        }
    };
    let dir = parent(exe);
    if dir.rsplit('/').next() == Some("bin") {
        parent(&dir)
    } else {
        dir
    }
}

/// Where the engine keeps `default.cfg` and `.savegame/` (it uses its working
/// directory): `$HOME/.doom` when there is a home, `/tmp/doom` otherwise.
pub fn config_dir(home: Option<&str>) -> String {
    match home.filter(|home| home.starts_with('/') && *home != "/") {
        Some(home) => join(home, ".doom"),
        None => fhs::state::DOOM_TMP.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| arg.to_string()).collect()
    }

    const INSTALL: &str = "/data/apps/org.lazy.doom/0.1.0-abcd1234";

    #[test]
    fn init_launch_finds_the_packaged_iwad() {
        let exe = format!("{INSTALL}/bin/doom.elf");
        let launch = parse(&args(&[&exe, "--client", "attempt=2"]), "/");
        let iwad = format!("{INSTALL}/resources/freedoom1.wad");
        assert_eq!(launch.mode, Mode::Window);
        assert_eq!(launch.iwad, iwad);
        assert_eq!(launch.engine_args, args(&[&exe, "-iwad", &iwad]));
    }

    #[test]
    fn engine_flags_pass_through_and_wrapper_flags_do_not() {
        let launch = parse(
            &args(&[
                "doom.elf",
                "-headless",
                "-frames",
                "35",
                "-timedemo",
                "demo1",
                "-iwad",
                "/w/x.wad",
            ]),
            "/tmp",
        );
        assert_eq!(launch.mode, Mode::Headless { frames: Some(35) });
        assert_eq!(launch.iwad, "/w/x.wad");
        assert_eq!(
            launch.engine_args,
            args(&["doom.elf", "-timedemo", "demo1", "-iwad", "/w/x.wad"])
        );
    }

    #[test]
    fn headless_without_a_frame_count_runs_until_the_engine_exits() {
        let launch = parse(&args(&["doom.elf", "-headless"]), "/");
        assert_eq!(launch.mode, Mode::Headless { frames: None });
        let bad = parse(&args(&["doom.elf", "-frames", "lots"]), "/");
        assert_eq!(bad.mode, Mode::Headless { frames: None });
    }

    #[test]
    fn a_relative_argv0_resolves_against_the_working_directory() {
        let launch = parse(&args(&["bin/doom.elf"]), INSTALL);
        assert_eq!(launch.iwad, format!("{INSTALL}/{PACKAGED_IWAD}"));
        let dotted = parse(&args(&["./doom.elf"]), &format!("{INSTALL}/bin"));
        assert_eq!(dotted.iwad, format!("{INSTALL}/{PACKAGED_IWAD}"));
        let relative = parse(&args(&["doom.elf", "-iwad", "./w/x.wad"]), "/games");
        assert_eq!(relative.iwad, "/games/w/x.wad");
        let loose = parse(&args(&["doom.elf"]), "/home/me/games");
        assert_eq!(loose.iwad, "/home/me/games/resources/freedoom1.wad");
    }

    #[test]
    fn hostile_arguments_are_bounded() {
        let mut list = vec!["doom.elf".to_string(), "x".repeat(MAX_ARG_BYTES + 1)];
        list.extend((0..200).map(|i| format!("-a{i}")));
        let launch = parse(&list, "/");
        assert!(launch
            .engine_args
            .iter()
            .all(|arg| arg.len() <= MAX_ARG_BYTES));
        assert!(launch.engine_args.len() <= MAX_ARGS + 2);
        assert_eq!(parse(&[], "/").engine_args[0], "doom");
    }

    #[test]
    fn config_lives_in_the_home_or_tmp() {
        assert_eq!(config_dir(Some("/home/ana")), "/home/ana/.doom");
        assert_eq!(config_dir(Some("/")), "/tmp/doom");
        assert_eq!(config_dir(Some("relative")), "/tmp/doom");
        assert_eq!(config_dir(None), "/tmp/doom");
    }

    #[test]
    fn the_install_dir_is_above_bin() {
        assert_eq!(install_dir("/a/b/bin/doom.elf"), "/a/b");
        assert_eq!(install_dir("/a/doom.elf"), "/a");
        assert_eq!(install_dir("/doom.elf"), "/");
        assert_eq!(install_dir("/bin/doom.elf"), "/");
    }
}
