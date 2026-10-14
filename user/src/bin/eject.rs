//! `eject` (`/system/bin/eject`): flush a removable volume, unmount it, and
//! say when its stick may be pulled (docs/usb-hotplug-plan.md D).
//!
//! ```text
//! eject [-f] <volume>   usb1, the volume's name (MY_STICK) or /media/MY_STICK
//! eject -l              list the removable volumes
//! ```
//!
//! It asks `mountd` (`os.lazy.mount.v1` `Eject`), whose `fatfuse` for the
//! volume writes everything back, closes the filesystem (the dirty flag is
//! cleared), flushes the stick and unmounts, then waits until the volume is
//! `ejected`. A volume with a file open on it is refused and the holders are
//! listed; `-f` ejects anyway (open files then fail with an I/O error).
//!
//! Exit status: `0` ejected, `1` failed, `2` usage, `3` busy.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use core::panic::PanicInfo;

use user::messenger::mount::{self, wire::VolumeInfo};
use user::messenger::{errno, registry, Error};
use user::sys;

/// Longest one request to `mountd` may take (100 Hz): it answers at once.
const CALL_TICKS: u64 = 500;
/// Longest an eject may take before the command gives up waiting (a slow
/// stick's flush; `mountd` itself ends one after 45 s).
const WAIT_TICKS: u64 = 5500;
/// Ticks between looks at the volume list while waiting.
const POLL_TICKS: u64 = 25;

const USAGE: &str = "usage: eject [-f] <usbN|NAME|/media/NAME>\n       eject -l\n";

#[no_mangle]
pub extern "C" fn _start() -> ! {
    let status = match run() {
        Ok(()) => 0,
        Err(Exit { status, message }) => {
            sys::write_str(&message);
            status
        }
    };
    sys::exit(status)
}

struct Exit {
    status: u32,
    message: String,
}

fn fail(status: u32, message: String) -> Exit {
    Exit { status, message }
}

fn run() -> Result<(), Exit> {
    let mut force = false;
    let mut list = false;
    let mut target: Option<String> = None;
    for arg in sys::args().skip(1) {
        match arg {
            "-f" | "--force" => force = true,
            "-l" | "--list" => list = true,
            word if word.starts_with('-') => return Err(fail(2, String::from(USAGE))),
            word if target.is_none() => target = Some(String::from(word)),
            _ => return Err(fail(2, String::from(USAGE))),
        }
    }
    let service = registry::resolve(mount::NAME).map_err(|_| {
        fail(
            1,
            String::from(
                "eject: the mount service is not running (no USB support in this image)\n",
            ),
        )
    })?;
    let deadline = || Some(sys::clock() + CALL_TICKS);
    let volumes = mount::volumes(&service, deadline()).map_err(|e| refused(&e))?;
    if list {
        for volume in &volumes {
            sys::write_str(&line(volume));
        }
        if volumes.is_empty() {
            sys::write_str("eject: no removable volumes\n");
        }
        return Ok(());
    }
    let Some(target) = target else {
        return Err(fail(2, String::from(USAGE)));
    };
    let volume = find(&volumes, &target)
        .ok_or_else(|| fail(1, format!("eject: {target}: no such volume\n")))?;
    let disk = volume.disk.clone();
    let (state, detail) =
        mount::eject(&service, &disk, force, deadline()).map_err(|e| refused(&e))?;
    match state.as_str() {
        "busy" => {
            let mut text = format!("eject: {disk} is busy:\n");
            for holder in detail.lines() {
                text.push_str("  ");
                text.push_str(holder);
                text.push('\n');
            }
            text.push_str("close them, or eject -f to eject anyway\n");
            return Err(fail(3, text));
        }
        "ejected" => {}
        _ => wait_ejected(&service, &disk)?,
    }
    sys::write_str(&format!(
        "eject: {disk}{}: ejected, it is safe to remove\n",
        if volume.name.is_empty() {
            String::new()
        } else {
            format!(" ({})", volume.name)
        }
    ));
    Ok(())
}

/// The volume `target` names: by disk, by name, or by mount point.
fn find<'a>(volumes: &'a [VolumeInfo], target: &str) -> Option<&'a VolumeInfo> {
    volumes.iter().find(|v| {
        v.disk == target || (!v.name.is_empty() && (v.name == target || v.path == target))
    })
}

/// One line of the listing.
fn line(volume: &VolumeInfo) -> String {
    let mut text = format!("{} {}", volume.disk, volume.state);
    if !volume.path.is_empty() {
        text.push(' ');
        text.push_str(&volume.path);
    }
    if !volume.writable {
        text.push_str(" (read-only)");
    }
    if !volume.detail.is_empty() {
        text.push_str(": ");
        text.push_str(&volume.detail);
    }
    text.push('\n');
    text
}

/// Poll the list until `disk` is `ejected`, or ends some other way.
fn wait_ejected(service: &user::messenger::Endpoint, disk: &str) -> Result<(), Exit> {
    let give_up = sys::clock() + WAIT_TICKS;
    loop {
        let volumes =
            mount::volumes(service, Some(sys::clock() + CALL_TICKS)).map_err(|e| refused(&e))?;
        match volumes.iter().find(|v| v.disk == disk) {
            // Gone from the list: the stick was pulled before it finished.
            None => {
                return Err(fail(
                    1,
                    format!("eject: {disk} disappeared before it was ejected\n"),
                ))
            }
            Some(v) if v.state == "ejected" => return Ok(()),
            Some(v) if v.state == "ejecting" => {}
            Some(v) => {
                return Err(fail(
                    1,
                    format!(
                        "eject: {disk}: {}{}{}\n",
                        v.state,
                        if v.detail.is_empty() { "" } else { ": " },
                        v.detail
                    ),
                ))
            }
        }
        if sys::clock() >= give_up {
            return Err(fail(
                1,
                format!("eject: {disk} did not finish: do not remove it\n"),
            ));
        }
        sys::sleep_ns(POLL_TICKS * 10_000_000);
    }
}

/// A failed request in words.
fn refused(error: &Error) -> Exit {
    let why = match error.errno() {
        Some(code) if code == -errno::ENOENT => "no such disk",
        Some(code) if code == -errno::EPERM => "not your volume",
        Some(code) if code == -errno::EINVAL => "not a disk name",
        Some(code) if code == -errno::EAGAIN => "the volume is still starting: try again",
        _ => error.message(),
    };
    fail(1, format!("eject: {why}\n"))
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::write_str("eject: internal error\n");
    sys::exit(1)
}
