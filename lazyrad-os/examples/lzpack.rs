//! `lzpack`: package a LazyRAD project as a LazyOS `.lzp` on the host, the
//! way the IDE's File → Make LazyOS App does on LazyOS.
//!
//! ```text
//! cargo run --manifest-path lazyrad-os/Cargo.toml --example lzpack -- \
//!     <project dir> --player target/lazyrad/lrplay.elf --out target/pkg/modplayer.lzp \
//!     [--system-name org.lazy.modplayer] [--author LazyOS] [--description <text>]
//! ```
//!
//! Unlike LazyRAD's own `lazyrad-pack`, the permissions come from the LazyOS
//! platform (`LazyOsPlatform::script_permissions`): the Messenger interfaces
//! and topics the scripts use, and the mixer for a script that plays a song.
//! The project is checked first, as the player would compile it.
//! `tools/lazyrad/package.py` drives this for the image's packages.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use lazyrad_os::platform::{Home, LazyOsPlatform};
use lazyrad_packager::lzp::{build_package, read_player, HostPermissions, PackageRequest};
use lazyrad_runtime::platform::Platform;

struct Options {
    project: PathBuf,
    player: PathBuf,
    out: PathBuf,
    system_name: Option<String>,
    author: String,
    description: Option<String>,
}

const USAGE: &str = "usage: lzpack <project dir> --player <lrplay.elf> --out <file.lzp> \
[--system-name <id>] [--author <name>] [--description <text>]";

fn parse() -> Result<Options, String> {
    let mut args = std::env::args().skip(1);
    let mut project = None;
    let (mut player, mut out) = (None, None);
    let (mut system_name, mut description) = (None, None);
    let mut author = "LazyOS".to_owned();
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or(format!("{arg} needs a value"));
        match arg.as_str() {
            "--player" => player = Some(PathBuf::from(value()?)),
            "--out" => out = Some(PathBuf::from(value()?)),
            "--system-name" => system_name = Some(value()?),
            "--author" => author = value()?,
            "--description" => description = Some(value()?),
            flag if flag.starts_with("--") => return Err(format!("unknown option {flag}")),
            path if project.is_none() => project = Some(PathBuf::from(path)),
            extra => return Err(format!("unexpected argument {extra}")),
        }
    }
    Ok(Options {
        project: project.ok_or("no project given")?,
        player: player.ok_or("--player is required")?,
        out: out.ok_or("--out is required")?,
        system_name,
        author,
        description,
    })
}

/// The project's `.lrp`: `path` itself, or the one `.lrp` in that directory.
fn project_file(path: &Path) -> Result<PathBuf, String> {
    if path.is_file() {
        return Ok(path.to_path_buf());
    }
    let entries = std::fs::read_dir(path).map_err(|e| format!("{}: {e}", path.display()))?;
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|p| p.extension().is_some_and(|x| x == "lrp"))
        .ok_or_else(|| format!("{} has no .lrp", path.display()))
}

fn permissions(scripts: &[&str]) -> HostPermissions {
    let found = LazyOsPlatform::ide(Home::from_env()).script_permissions(scripts);
    HostPermissions {
        interfaces: found.interfaces,
        topics: found.topics,
    }
}

/// The compile check the player would run.
fn check(lrp: &Path) -> Result<(), Vec<String>> {
    let dir = lrp.parent().unwrap_or(Path::new("."));
    let report = lazyrad_runtime::check_project(dir).map_err(|e| vec![e.to_string()])?;
    if report.is_empty() {
        Ok(())
    } else {
        Err(vec![format!("{report:?}")])
    }
}

fn run(options: &Options) -> Result<(), String> {
    let lrp = project_file(&options.project)?;
    let player = read_player(&options.player).map_err(|e| e.to_string())?;
    let built = build_package(&PackageRequest {
        project: &lrp,
        player: &player,
        author: &options.author,
        system_name: options.system_name.as_deref(),
        description: options.description.as_deref(),
        icons: None,
        check: Some(&check),
        permissions: Some(&permissions),
    })
    .map_err(|e| e.to_string())?;
    if let Some(dir) = options.out.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(&options.out, &built.bytes)
        .map_err(|e| format!("{}: {e}", options.out.display()))?;
    println!(
        "{}: {} {} ({} entries, {} bytes)\n{}",
        options.out.display(),
        built.system_name,
        built.version,
        built.entries,
        built.bytes.len(),
        built.manifest
    );
    Ok(())
}

fn main() -> ExitCode {
    let options = match parse() {
        Ok(options) => options,
        Err(error) => {
            eprintln!("lzpack: {error}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match run(&options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("lzpack: {error}");
            ExitCode::FAILURE
        }
    }
}
