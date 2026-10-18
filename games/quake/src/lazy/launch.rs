//! The command line: what the wrapper itself reads, and the argv the engine
//! command line keeps.
//!
//! Three callers start `lazyquake`, and all of them are untrusted input:
//!
//! - `init`, for the installed package: `<install>/bin/quake.elf --client
//!   attempt=N` (the manifest's `entry.args`, then `init`'s own suffix);
//! - a developer or a test at a shell: `quake.elf -headless -frames 200`;
//! - any Quake command-line flags (`-preset slop`, `-cdtracks ...`),
//!   passed through; `+`-commands run through `stuffcmds` as id's did.
//!
//! The wrapper consumes `--client` (its default anyway), `attempt=N`,
//! `-headless` and `-frames N`; everything else reaches the engine. The
//! shareware pak comes from the package's read-only resources: the engine
//! gets `-basedir <install>/resources`, so the search path finds `id1/`
//! there (the manifest's `resource` file). The player's own files (the
//! saves, `config.cfg`) go to the data directory
//! ([`config_dir`]): `$HOME/.apps/org.lazy.quake`, which the command line
//! never names — the platform layer decides it ([`crate::common`]'s
//! LazyOS change).

/// The package's `system_name`: its per-user folder is named after it.
pub const SYSTEM_NAME: &str = "org.lazy.quake";
/// The resources directory of an install directory, where `id1/` lives.
pub const PACKAGED_BASEDIR: &str = "resources";
/// Longest argument kept, in bytes; a longer one is dropped rather than
/// copied into the engine's command line.
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
    /// The engine command line (`argv[0]` first), wrapper flags removed.
    pub game_args: Vec<String>,
    /// `-basedir` the engine search path opens (a `-basedir` passthrough
    /// wins over the packaged resources).
    pub basedir: Option<String>,
}

/// Parse `args` (`argv[0]` first). `cwd` resolves a relative `argv[0]`.
pub fn parse(args: &[String], cwd: &str) -> Launch {
    let argv0 = args.first().map(String::as_str).unwrap_or("");
    let mut mode = Mode::Window;
    let mut wrapper_basedir = None;
    let mut game_args = vec![argv0.to_string()];
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
            "-basedir" => wrapper_basedir = rest.next().cloned(),
            other if other.starts_with("attempt=") => {}
            other if other.len() <= MAX_ARG_BYTES && game_args.len() < MAX_ARGS => {
                game_args.push(other.to_string());
            }
            _ => {}
        }
    }
    // A `-basedir` the wrapper saw is absolute; a relative one resolves
    // against the working directory, since the wrapper never changes
    // directory.
    let basedir = wrapper_basedir.map(|dir| {
        if dir.starts_with('/') || dir.len() > MAX_ARG_BYTES {
            dir
        } else {
            join(cwd, dir.trim_start_matches("./"))
        }
    });
    Launch { mode, game_args, basedir }
}

/// The archive directory the game search path opens: `-basedir` as given,
/// else the packaged resources of the running binary's install directory.
pub fn resolve_basedir(launch: &Launch, exe: &str) -> String {
    launch
        .basedir
        .clone()
        .unwrap_or_else(|| join(&install_dir(exe), PACKAGED_BASEDIR))
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
        return join(cwd, "quake.elf");
    }
    if argv0.starts_with('/') {
        argv0.to_string()
    } else {
        join(cwd, argv0.trim_start_matches("./"))
    }
}

/// The install directory of a packaged binary: the parent of the `bin/`
/// directory holding it, or the binary's own directory when it is not in a
/// `bin/` (a developer's copy next to an `id1/`).
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

/// Where the engine writes `config.cfg` and the saves: the package's own
/// per-user folder, `$HOME/.apps/org.lazy.quake` (the one write the manifest
/// asks for; `init` starts an installed app with its session's `HOME`), when
/// there is a home; the ramfs `/tmp/quake` otherwise, which a reboot clears.
pub fn config_dir(home: Option<&str>) -> String {
    match home.filter(|home| home.starts_with('/') && *home != "/") {
        Some(home) => fhs::app_data_dir(home, SYSTEM_NAME),
        None => fhs::state::QUAKE_TMP.to_string(),
    }
}

/// The launcher's added command-line preset: id's game, one value on which
/// the headless run's hashes and the window's first draw depend (the slop
/// preset's native resolution follows the window, and a preset stored in
/// `config.cfg` flows from the last session). A `-preset`/`+preset` the
/// caller named wins; the wrapper leaves the player's stored one alone.
pub const DEFAULT_PRESET_ARGS: &[&str] = &["-preset", "classic"];

/// `true` when the caller already asked for a preset anywhere in the
/// command line.
fn preset_asked(args: &[String]) -> bool {
    args.iter().any(|arg| matches!(arg.as_str(), "-preset" | "+preset"))
}

/// `game_args` with the wrapper's default preset made explicit, when the
/// caller did not name one (the headless checksums and the window's opening
/// both assume id's game; `preset slop` in the console still switches).
pub fn with_default_preset(mut game_args: Vec<String>) -> Vec<String> {
    if preset_asked(&game_args) {
        return game_args;
    }
    let extra = DEFAULT_PRESET_ARGS.iter().map(|arg| (*arg).to_string());
    game_args.splice(1..1, extra);
    game_args
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| arg.to_string()).collect()
    }

    const INSTALL: &str = "/apps/org.lazy.quake/0.1.0-abcd1234";

    #[test]
    fn init_launch_finds_the_packaged_resources() {
        let exe = format!("{INSTALL}/bin/quake.elf");
        let launch = parse(&args(&[&exe, "--client", "attempt=2"]), "/");
        assert_eq!(launch.mode, Mode::Window);
        assert_eq!(resolve_basedir(&launch, &exe), format!("{INSTALL}/resources"));
    }

    #[test]
    fn wrapper_flags_do_not_reach_the_engine() {
        let launch = parse(
            &args(&["quake.elf", "-headless", "-frames", "200", "+map", "e1m1"]),
            "/",
        );
        assert_eq!(launch.mode, Mode::Headless { frames: Some(200) });
        assert_eq!(
            launch.game_args,
            args(&["quake.elf", "+map", "e1m1"])
        );
    }

    #[test]
    fn a_bad_frame_count_is_still_headless() {
        let launch = parse(&args(&["quake.elf", "-frames", "lots"]), "/");
        assert_eq!(launch.mode, Mode::Headless { frames: None });
        let open = parse(&args(&["quake.elf"]), "/");
        assert_eq!(open.mode, Mode::Window);
    }

    #[test]
    fn basedir_is_resolved_and_wins_over_the_package() {
        let launch = parse(&args(&["quake.elf", "-basedir", "/games"]), "/");
        assert_eq!(launch.basedir.as_deref(), Some("/games"));
        let relative = parse(&args(&["quake.elf", "-basedir", "./w"]), "/tmp");
        assert_eq!(relative.basedir.as_deref(), Some("/tmp/w"));
        assert_eq!(resolve_basedir(&relative, "/bin/quake.elf"), "/tmp/w");
    }

    #[test]
    fn a_relative_argv0_resolves_against_the_working_directory() {
        let long = parse(&args(&["bin/quake.elf"]), INSTALL);
        assert_eq!(
            resolve_basedir(&long, &exe_path("bin/quake.elf", INSTALL)),
            format!("{INSTALL}/resources")
        );
        let dotted = parse(&args(&["./quake.elf"]), &format!("{INSTALL}/bin"));
        assert_eq!(
            resolve_basedir(&dotted, &exe_path("./quake.elf", &format!("{INSTALL}/bin"))),
            format!("{INSTALL}/resources")
        );
    }

    #[test]
    fn hostile_arguments_are_bounded() {
        let mut list = vec!["quake.elf".to_string(), "x".repeat(MAX_ARG_BYTES + 1)];
        list.extend((0..200).map(|i| format!("-a{i}")));
        let launch = parse(&list, "/");
        assert!(launch.game_args.iter().all(|arg| arg.len() <= MAX_ARG_BYTES));
        assert!(launch.game_args.len() <= MAX_ARGS + 2);
        assert_eq!(parse(&[], "/").game_args[0], "");
    }

    #[test]
    fn config_lives_in_the_home_or_tmp() {
        assert_eq!(config_dir(Some("/home/ana")), "/home/ana/.apps/org.lazy.quake");
        assert_eq!(config_dir(Some("/home/ana/")), "/home/ana/.apps/org.lazy.quake");
        assert_eq!(config_dir(Some("/")), "/tmp/quake");
        assert_eq!(config_dir(Some("relative")), "/tmp/quake");
        assert_eq!(config_dir(None), "/tmp/quake");
    }

    #[test]
    fn the_install_dir_is_above_bin() {
        assert_eq!(install_dir("/a/b/bin/quake.elf"), "/a/b");
        assert_eq!(install_dir("/a/quake.elf"), "/a");
        assert_eq!(install_dir("/quake.elf"), "/");
        assert_eq!(install_dir("/bin/quake.elf"), "/");
    }

    #[test]
    fn the_default_preset_arrives_once() {
        let applied = with_default_preset(args(&["quake.elf", "+map", "e1m1"]));
        assert_eq!(
            applied,
            args(&["quake.elf", "-preset", "classic", "+map", "e1m1"])
        );
        let named = with_default_preset(args(&["quake.elf", "-preset", "slop"]));
        assert_eq!(named, args(&["quake.elf", "-preset", "slop"]), "the caller's own choice stays");
        let plus = with_default_preset(args(&["quake.elf", "+preset", "slop"]));
        assert_eq!(plus, args(&["quake.elf", "+preset", "slop"]), "the console's preset counts too");
        assert_eq!(with_default_preset(args(&["quake.elf"])), args(&["quake.elf", "-preset", "classic"]));
    }
}
