//! The command line: what the wrapper itself reads, and the argv the engine
//! gets, plus the two directories the engine works in.
//!
//! Three callers start `lazysrb2kart`, and all of them are untrusted input:
//!
//! - `init`, for the installed package: `<install>/bin/srb2kart.elf --client
//!   attempt=N` (the manifest's `entry.args`, then `init`'s own suffix);
//! - a developer or a test at a shell: `srb2kart.elf -headless -frames 300`;
//! - any extra engine flags (`+map ...`, `-skill`), passed through.
//!
//! The wrapper consumes `--client` (its default anyway), `attempt=N`,
//! `-headless`, `-frames N`, `-data DIR` and `-home DIR`; everything else
//! reaches the engine. The game data is not in the package: it lives on the
//! OS volume ([`DATA_DIR`]) unless `-data` names another directory.

/// Where the image build puts the game data.
pub const DATA_DIR: &str = fhs::share::SRB2KART_DATA;
/// The writable directory when there is no home.
pub const TMP_HOME: &str = fhs::state::SRB2KART_TMP;
/// The headless verdict line is also written here.
pub const RESULT_FILE: &str = fhs::state::SRB2KART_RESULT;
/// The package's `system_name`: its per-user folder is named after it.
pub const SYSTEM_NAME: &str = "org.lazy.srb2kart";
/// The file whose presence marks a data directory.
pub const IWAD: &str = "srb2.srb";

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
    /// The engine's argv, `argv[0]` first.
    pub engine_args: Vec<String>,
    /// The directory holding `srb2.srb` and the `.kart` files.
    pub data_dir: String,
    /// The writable directory (config, saves, replays), created at start-up.
    pub home_dir: String,
}

/// Parse `args` (`argv[0]` first). `home` is `$HOME`, `cwd` resolves relative
/// directory arguments.
pub fn parse(args: &[String], home: Option<&str>, cwd: &str) -> Launch {
    let argv0 = args.first().map(String::as_str).unwrap_or("");
    let mut mode = Mode::Window;
    let mut engine_args = vec![if argv0.is_empty() {
        "srb2kart".to_string()
    } else {
        argv0.to_string()
    }];
    let mut data = None;
    let mut home_dir = None;
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
            "-data" => data = rest.next().cloned(),
            "-home" => home_dir = rest.next().cloned(),
            other if other.starts_with("attempt=") => {}
            other if other.len() <= MAX_ARG_BYTES && engine_args.len() < MAX_ARGS => {
                engine_args.push(other.to_string());
            }
            _ => {}
        }
    }
    // Headless runs are checks, not play: the smallest mode (the engine's own
    // default is 1280x800) keeps them fast, unless the caller chose one.
    if matches!(mode, Mode::Headless { .. }) && !engine_args.iter().any(|a| a == "-width") {
        engine_args.extend(["-width", "320", "-height", "200"].map(String::from));
    }
    let data_dir = absolute(data, cwd).unwrap_or_else(|| DATA_DIR.to_string());
    let home_dir = absolute(home_dir, cwd).unwrap_or_else(|| config_dir(home));
    Launch {
        mode,
        engine_args,
        data_dir,
        home_dir,
    }
}

/// `path` made absolute against `cwd` (the engine changes directory), or
/// `None` when it is missing, empty or too long.
fn absolute(path: Option<String>, cwd: &str) -> Option<String> {
    let path = path.filter(|path| !path.is_empty() && path.len() <= MAX_ARG_BYTES)?;
    Some(if path.starts_with('/') {
        path
    } else {
        format!(
            "{}/{}",
            cwd.trim_end_matches('/'),
            path.trim_start_matches("./")
        )
    })
}

/// The package's own per-user folder, `$HOME/.apps/org.lazy.srb2kart` (the one
/// place the manifest asks to write, docs/packages.md), when there is a home;
/// [`TMP_HOME`] otherwise. `init` hands an installed app its session's `HOME`,
/// and so does a Terminal.
pub fn config_dir(home: Option<&str>) -> String {
    match home.filter(|home| home.starts_with('/') && *home != "/") {
        Some(home) => fhs::app_data_dir(home, SYSTEM_NAME),
        None => TMP_HOME.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| arg.to_string()).collect()
    }

    #[test]
    fn init_launch_uses_the_image_data_and_the_app_folder() {
        let launch = parse(
            &args(&["/apps/org.lazy.srb2kart/1/bin/srb2kart.elf", "--client", "attempt=2"]),
            Some("/home/ana"),
            "/",
        );
        assert_eq!(launch.mode, Mode::Window);
        assert_eq!(launch.data_dir, DATA_DIR);
        assert_eq!(launch.home_dir, "/home/ana/.apps/org.lazy.srb2kart");
        assert_eq!(
            launch.engine_args,
            args(&["/apps/org.lazy.srb2kart/1/bin/srb2kart.elf"])
        );
    }

    #[test]
    fn wrapper_flags_are_consumed_and_engine_flags_pass_through() {
        let launch = parse(
            &args(&[
                "srb2kart.elf", "-headless", "-frames", "300", "-data", "/d", "-home", "./h",
                "+map", "MAP01", "-nomusic",
            ]),
            Some("/home/ana"),
            "/w",
        );
        assert_eq!(launch.mode, Mode::Headless { frames: Some(300) });
        assert_eq!(launch.data_dir, "/d");
        assert_eq!(launch.home_dir, "/w/h");
        assert_eq!(
            launch.engine_args,
            args(&["srb2kart.elf", "+map", "MAP01", "-nomusic", "-width", "320", "-height", "200"])
        );
    }

    #[test]
    fn headless_picks_the_small_mode_unless_told_otherwise() {
        let default = parse(&args(&["k.elf", "-headless"]), None, "/");
        assert_eq!(default.engine_args[1..], args(&["-width", "320", "-height", "200"]));
        let chosen = parse(
            &args(&["k.elf", "-headless", "-width", "640", "-height", "400"]),
            None,
            "/",
        );
        assert_eq!(chosen.engine_args[1..], args(&["-width", "640", "-height", "400"]));
        let window = parse(&args(&["k.elf"]), None, "/");
        assert_eq!(window.engine_args, args(&["k.elf"]));
    }

    #[test]
    fn headless_without_a_frame_count_runs_until_the_engine_exits() {
        let launch = parse(&args(&["k.elf", "-headless"]), None, "/");
        assert_eq!(launch.mode, Mode::Headless { frames: None });
        let bad = parse(&args(&["k.elf", "-frames", "lots"]), None, "/");
        assert_eq!(bad.mode, Mode::Headless { frames: None });
    }

    #[test]
    fn hostile_arguments_are_bounded() {
        let mut list = vec!["k.elf".to_string(), "x".repeat(MAX_ARG_BYTES + 1)];
        list.extend((0..200).map(|i| format!("-a{i}")));
        list.push("-data".to_string());
        list.push("y".repeat(MAX_ARG_BYTES + 1));
        let launch = parse(&list, None, "/");
        assert!(launch.engine_args.iter().all(|arg| arg.len() <= MAX_ARG_BYTES));
        assert!(launch.engine_args.len() <= MAX_ARGS);
        assert_eq!(launch.data_dir, DATA_DIR);
        assert_eq!(parse(&[], None, "/").engine_args[0], "srb2kart");
    }

    #[test]
    fn config_lives_in_the_home_or_tmp() {
        assert_eq!(
            config_dir(Some("/home/ana")),
            "/home/ana/.apps/org.lazy.srb2kart"
        );
        assert_eq!(
            config_dir(Some("/home/ana/")),
            "/home/ana/.apps/org.lazy.srb2kart"
        );
        assert_eq!(config_dir(Some("/")), TMP_HOME);
        assert_eq!(config_dir(Some("relative")), TMP_HOME);
        assert_eq!(config_dir(None), TMP_HOME);
    }
}
