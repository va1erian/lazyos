//! `pkgctl` (`/system/bin/pkgctl`): the command line of the package manager `pkgd`
//! (`docs/packages.md`).
//!
//! ```text
//! pkgctl inspect <path>     what a .lzp declares and asks for; changes nothing
//! pkgctl install <path>     install it (the same call a GUI installer makes)
//! pkgctl remove <name>      remove an installed app by its system name
//! pkgctl list               every installed app
//! ```
//!
//! A thin client: all checks (who may install, which files may be read, whether
//! the package is valid) are `pkgd`'s, so this prints its friendly refusals
//! as they come. Exit status 0 on success, 1 otherwise.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::panic::PanicInfo;

use user::messenger::mime;
use user::messenger::pkgd::{Client, Failure, Installed, PackageInfo};
use user::sys;

const USAGE: &str = "usage: pkgctl <inspect|install|remove|list|open> [args]\n\
    inspect <path>\n\
    install <path>\n\
    remove <system-name>\n\
    list\n\
    open <path>      hand the package to the GUI installer through mimed\n";

/// Attempts to reach `pkgd`: it is a supervised service that may be starting,
/// or restarting to recycle its memory.
const CONNECT_ATTEMPTS: usize = 600;

#[no_mangle]
pub extern "C" fn _start() -> ! {
    let args = args();
    let status = match run(&args) {
        Ok(()) => 0,
        Err(message) => {
            say(&message);
            1
        }
    };
    sys::exit(status)
}

/// The argument string the shell passed.
fn args() -> Vec<String> {
    let mut buffer = [0u8; 1100];
    let len = sys::service_args(&mut buffer).min(buffer.len());
    let text = core::str::from_utf8(&buffer[..len]).unwrap_or("");
    text.split_whitespace().map(String::from).collect()
}

fn say(text: &str) {
    sys::write_str(text);
    sys::write_str("\n");
}

fn failed(failure: Failure) -> String {
    format!("pkgctl: {}", failure.text)
}

fn run(args: &[String]) -> Result<(), String> {
    let command = args.first().map(String::as_str);
    if !matches!(
        command,
        Some("inspect" | "install" | "remove" | "list" | "open")
    ) {
        return Err(String::from(USAGE.trim_end()));
    }
    if command == Some("open") {
        return open_with_installer(arg(args, "open <path>")?);
    }
    let client = Client::connect_retry(CONNECT_ATTEMPTS).map_err(|error| {
        format!(
            "pkgctl: the package manager is not running: {}",
            error.message()
        )
    })?;
    match command {
        Some("inspect") => {
            let path = arg(args, "inspect <path>")?;
            show_info(&client.inspect(path).map_err(failed)?);
            Ok(())
        }
        Some("install") => {
            let path = arg(args, "install <path>")?;
            let app = client.install(path).map_err(failed)?;
            say(&format!(
                "PKGCTL:INSTALL:PASS {} {} {}",
                app.system_name, app.version, app.install_dir
            ));
            Ok(())
        }
        Some("remove") => {
            let name = arg(args, "remove <system-name>")?;
            client.remove(name).map_err(failed)?;
            say(&format!("PKGCTL:REMOVE:PASS {name}"));
            Ok(())
        }
        _ => {
            let apps = client.list().map_err(failed)?;
            say(&format!("PKGCTL:LIST:{}", apps.len()));
            for app in &apps {
                show_installed(app);
            }
            Ok(())
        }
    }
}

/// `pkgctl open <path>`: the route Files takes for a `.lzp`, `mimed.Open` with
/// the `install` verb, so the GUI installer starts with the package path as its
/// argument and shows the consent screen. A Terminal session script types a
/// shell line reliably where typing into a GUI field under TCG drops keys.
fn open_with_installer(path: &str) -> Result<(), String> {
    let mimed = mime::Client::connect()
        .map_err(|error| format!("pkgctl: mimed is not running: {}", error.message()))?;
    let opened = mimed
        .open(path, "install")
        .map_err(|error| format!("pkgctl: open failed: {}", error.message()))?;
    if !opened.launched {
        return Err(format!(
            "pkgctl: {} handles {} but init did not launch it",
            opened.app, opened.mime
        ));
    }
    say(&format!("PKGCTL:OPEN:PASS {} {}", opened.app, opened.mime));
    Ok(())
}

fn arg<'a>(args: &'a [String], usage: &str) -> Result<&'a str, String> {
    args.get(1)
        .map(String::as_str)
        .ok_or_else(|| format!("usage: pkgctl {usage}"))
}

fn show_installed(app: &Installed) {
    say(&format!(
        "  {} {} ({}) {}",
        app.system_name, app.version, app.name, app.install_dir
    ));
}

fn show_info(info: &PackageInfo) {
    if info.system_name.is_empty() {
        say("PKGCTL:INSPECT:INVALID");
    } else {
        say(&format!(
            "PKGCTL:INSPECT:{} {} {}",
            info.system_name, info.version, info.install_dir
        ));
        say(&format!("  {} by {}", info.name, info.author));
        if !info.description.is_empty() {
            say(&format!("  {}", info.description));
        }
        for handler in &info.mime {
            say(&format!(
                "  opens {} ({})",
                handler.mime_type,
                handler.verbs.join(", ")
            ));
        }
        for permission in &info.permissions {
            say(&format!(
                "  permission [{}] {}: {}",
                permission.risk, permission.value, permission.explanation
            ));
        }
    }
    for problem in &info.problems {
        say(&format!("  problem: {problem}"));
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
