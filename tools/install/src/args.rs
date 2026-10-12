//! The command line.

use std::path::PathBuf;

pub const USAGE: &str = "\
usage: lazyos-install [install] [options]     install (shows everything first)
       lazyos-install update [options]        replace the system in place, keep its data
       lazyos-install uninstall [options]     remove the ESP files and the GRUB entry
       lazyos-install status [options]        what is installed where

options:
  --stick DIR       the stick's FAT partition, mounted (default: the directory
                    above this program's `install/`)
  --system DEV      the system partition (default: GPT name lazyos-system)
  --home DEV        the home partition (default: GPT name lazyos-home)
  --esp DIR         the mounted ESP (default: /boot/efi, /efi or /boot)
  --image FILE      test mode: a whole-disk image instead of /dev (no root, no
                    GRUB tools; the entry goes into <grub-dir>/custom.cfg on
                    the image's ESP)
  --grub-dir DIR    test mode: GRUB's directory inside the ESP (default EFI/BOOT)
  --format-home     format the home partition even if it holds a LazyOS home
  --home-from FILE  copy this ext2 home volume (the stick's lazyhome) into a
                    newly formatted home
  --dry-run         stop after printing the plan
  --yes             do not ask for the device name back
  --trial-boot      boot LazyOS once at the next restart (grub-reboot)
  --check-limit N   largest volume (MiB) checked in memory (default 2048)";

/// What to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Install,
    Update,
    Uninstall,
    Status,
}

/// The parsed command line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    pub command: Command,
    pub stick: Option<PathBuf>,
    pub system: Option<PathBuf>,
    pub home: Option<PathBuf>,
    pub esp: Option<PathBuf>,
    pub image: Option<PathBuf>,
    pub grub_dir: String,
    pub format_home: bool,
    pub home_from: Option<PathBuf>,
    pub dry_run: bool,
    pub yes: bool,
    pub trial_boot: bool,
    pub check_limit: u64,
}

impl Options {
    fn new(command: Command) -> Options {
        Options {
            command,
            stick: None,
            system: None,
            home: None,
            esp: None,
            image: None,
            grub_dir: "EFI/BOOT".into(),
            format_home: false,
            home_from: None,
            dry_run: false,
            yes: false,
            trial_boot: false,
            check_limit: 2048 << 20,
        }
    }
}

/// Parse the arguments after the program name.
pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Options, String> {
    let mut args = args.into_iter().peekable();
    let command = match args.peek().map(String::as_str) {
        Some("install") => Command::Install,
        Some("update") => Command::Update,
        Some("uninstall") => Command::Uninstall,
        Some("status") => Command::Status,
        Some(other) if !other.starts_with('-') => return Err(format!("unknown command {other:?}")),
        _ => {
            return parse_options(Options::new(Command::Install), args);
        }
    };
    args.next();
    parse_options(Options::new(command), args)
}

fn parse_options(
    mut options: Options,
    mut args: impl Iterator<Item = String>,
) -> Result<Options, String> {
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--stick" => options.stick = Some(value("--stick")?.into()),
            "--system" => options.system = Some(value("--system")?.into()),
            "--home" => options.home = Some(value("--home")?.into()),
            "--esp" => options.esp = Some(value("--esp")?.into()),
            "--image" => options.image = Some(value("--image")?.into()),
            "--grub-dir" => options.grub_dir = value("--grub-dir")?.trim_matches('/').into(),
            "--home-from" => options.home_from = Some(value("--home-from")?.into()),
            "--check-limit" => {
                let text = value("--check-limit")?;
                let mib: u64 = text
                    .parse()
                    .map_err(|_| format!("--check-limit {text:?} is not a number"))?;
                options.check_limit = mib << 20;
            }
            "--format-home" => options.format_home = true,
            "--dry-run" => options.dry_run = true,
            "--yes" | "-y" => options.yes = true,
            "--trial-boot" => options.trial_boot = true,
            "--help" | "-h" => return Err("help".into()),
            other => return Err(format!("unknown option {other:?}")),
        }
    }
    let installs = options.command == Command::Install;
    if !installs && (options.format_home || options.home_from.is_some()) {
        return Err("--format-home and --home-from belong to install".into());
    }
    if options.image.is_some() && options.esp.is_some() {
        return Err("--esp names a mounted ESP; an --image carries its own".into());
    }
    Ok(options)
}
