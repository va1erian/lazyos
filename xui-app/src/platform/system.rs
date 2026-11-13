//! The Settings app's [`System`] on LazyOS: the clock and zone through `timed`
//! (`os.lazy.timed.v1`), the store location through `confd`'s `Info`, uptime
//! from the kernel `sysinfo` block and the version from `uname`.
//!
//! Calls go through the generated client and wait what `idl/timed.midl` states
//! (a read answers from memory; a write may reach the network); a failure is
//! reported as text for the status line.

use std::rc::Rc;

use libmessenger::client::Errno;
use messenger_generated::os_lazy_timed_v1 as wire;
use xui_confd_editor::store::ConfStore;
use xui_settings::system::{Now, NtpState, NtpStatus, StoreStatus, System};

use super::confd_store::ConfdStore;
use super::messenger::{client, CallError, Observed};
use crate::sys::errno;

/// PIT ticks per second.
const HZ: u64 = 100;
/// How often, and how far apart, a zone change is checked for: two seconds.
const ZONE_POLLS: u32 = 40;
const ZONE_POLL_MS: u64 = 50;

/// The live system.
#[derive(Clone, Copy, Debug, Default)]
pub struct OsSystem;

impl OsSystem {
    pub const fn new() -> OsSystem {
        OsSystem
    }

    /// Wait (briefly, bounded) until `timed` reports `zone`: it follows the
    /// confd key `elevd` wrote on its own change notification, so the page
    /// that re-reads the time right after would otherwise show the old zone.
    /// False when `timed` has not switched within the bound.
    fn await_zone(&self, zone: &str) -> bool {
        for _ in 0..ZONE_POLLS {
            if self.now().is_some_and(|now| now.zone == zone) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(ZONE_POLL_MS));
        }
        false
    }

    /// `timed`'s client, waiting for the service to register.
    fn timed(&self) -> Result<Rc<wire::Client<Observed>>, String> {
        client(wire::Client::connect).map_err(|_| String::from("time service unavailable"))
    }
}

/// `ENETUNREACH`: `SyncNow` with no address (the stack's own code).
const ENETUNREACH: i64 = 101;

/// A `timed` error code as status-line text.
fn describe(code: i64) -> String {
    match -code {
        errno::EPERM => String::from("permission denied (needs CAP_SYS_TIME)"),
        errno::EINVAL => String::from("value refused"),
        errno::ENODEV => String::from("Automatic time is off."),
        errno::EAGAIN => String::from(
            "Not now: a time server was asked a moment ago, or asked to be left alone for a while.",
        ),
        ENETUNREACH => String::from("There is no network."),
        _ => format!("time service error {code}"),
    }
}

/// `timed`'s `NtpState` ordinal, `Failed` for one this build does not know.
fn ntp_state(ordinal: u32) -> NtpState {
    match ordinal {
        wire::NTP_STATE_DISABLED => NtpState::Disabled,
        wire::NTP_STATE_WAITING_FOR_NETWORK => NtpState::WaitingForNetwork,
        wire::NTP_STATE_SYNCING => NtpState::Syncing,
        wire::NTP_STATE_SYNCHRONIZED => NtpState::Synchronized,
        _ => NtpState::Failed,
    }
}

/// `sysname release (machine)` from `uname`, e.g. `LazyOS 0.1.0 (x86_64)`.
#[cfg(target_os = "linux")]
fn uname() -> Option<String> {
    uname_fields().map(|[sysname, release, _, machine]| format!("{sysname} {release} ({machine})"))
}

/// `uname`'s version field: the kernel's source revision (`kernel/build.rs`).
#[cfg(target_os = "linux")]
fn revision() -> Option<String> {
    let [_, _, version, _] = uname_fields()?;
    let sha = version.trim();
    (!sha.is_empty() && sha != "unknown").then(|| sha.to_owned())
}

/// The build's image id: the ` image <id>` the kernel appends to
/// `/proc/version` when `lazyos.cfg` named one (`kernel/src/boot_image.rs`).
#[cfg(target_os = "linux")]
fn image() -> Option<String> {
    let text = std::fs::read_to_string("/proc/version").ok()?;
    xui_settings::system::image_from_proc_version(&text)
}

/// `[sysname, release, version, machine]` from `uname(2)`.
#[cfg(target_os = "linux")]
fn uname_fields() -> Option<[String; 4]> {
    // SAFETY: `utsname` is plain bytes, so all-zero is a valid value, and
    // `uname` only writes into the struct it is handed.
    let mut info: libc::utsname = unsafe { std::mem::zeroed() };
    // SAFETY: `info` is a valid, writable `utsname` for the duration of the call.
    if unsafe { libc::uname(&mut info) } != 0 {
        return None;
    }
    let field = |raw: &[libc::c_char]| -> String {
        let bytes: Vec<u8> = raw
            .iter()
            .take_while(|c| **c != 0)
            .map(|c| *c as u8)
            .collect();
        String::from_utf8_lossy(&bytes).into_owned()
    };
    Some([
        field(&info.sysname),
        field(&info.release),
        field(&info.version),
        field(&info.machine),
    ])
}

/// Host builds (tests) have no LazyOS kernel to ask.
#[cfg(not(target_os = "linux"))]
fn uname() -> Option<String> {
    None
}

#[cfg(not(target_os = "linux"))]
fn revision() -> Option<String> {
    None
}

#[cfg(not(target_os = "linux"))]
fn image() -> Option<String> {
    None
}

/// The picture files in `dir` (PNG and JPEG, by extension), sorted by path.
fn pictures_in(dir: &str) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut pictures: Vec<String> = entries
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| {
            let lower = name.to_ascii_lowercase();
            [".png", ".jpg", ".jpeg"]
                .iter()
                .any(|ext| lower.ends_with(ext))
        })
        .map(|name| format!("{dir}/{name}"))
        .collect();
    pictures.sort();
    pictures
}

impl System for OsSystem {
    /// A worker thread builds the same kind of connection, with its own
    /// endpoints ([`xui_settings::system::System::detached`]): the Time
    /// page's machine writes run there.
    fn detached(&self) -> Option<xui_settings::system::Detached> {
        use std::sync::Arc;
        Some(Arc::new(|| {
            // The thread this runs on is one of the tree's workers (no UI
            // loop on it): its `elevd` requests may wait unboundedly, which
            // the platform clients read off the same marker.
            crate::platform::messenger::mark_worker();
            let system: Box<dyn xui_settings::system::System> = Box::new(OsSystem);
            system
        }))
    }

    fn wallpapers(&self) -> Vec<String> {
        pictures_in(fhs::share::WALLPAPERS)
    }

    fn now(&self) -> Option<Now> {
        let now = self.timed().ok()?.now().ok()?;
        Some(Now {
            unix: now.unix_ms.div_euclid(1000),
            offset: now.tz_offset_s,
            zone: now.tz_name,
        })
    }

    /// Setting the clock is a system change (docs/accounts-plan.md U2): a
    /// session never holds `CAP_SYS_TIME`, so a refusal is asked again
    /// through `elevd`, where an administrator approves.
    fn set_time(&self, unix: i64) -> Result<(), String> {
        match self.timed()?.set_time(unix) {
            Ok(()) => Ok(()),
            Err(CallError::Transport(errno)) if errno == Errno::INVAL => {
                Err(String::from("bad time"))
            }
            Err(error) if -error.code() == errno::EPERM => {
                super::elevd::request("time.set", &[&unix.to_string()])
                    .map(|_| ())
                    .map_err(|error| super::elevd::describe(&error))
            }
            Err(error) => Err(describe(error.code())),
        }
    }

    fn ntp_status(&self) -> Option<NtpStatus> {
        let status = self.timed().ok()?.ntp_status().ok()?;
        Some(NtpStatus {
            enabled: status.enabled,
            state: ntp_state(status.state),
            server: status.server,
            offset_ms: status.offset_ms,
            last_sync_unix: status.last_sync_unix,
        })
    }

    fn ntp_sync_now(&self) -> Result<(), String> {
        self.timed()?
            .sync_now()
            .map_err(|error| describe(error.code()))
    }

    /// Unchanged; see the doc on the trait method (a worker builds its own).
    fn set_zone(&self, zone: &str) -> Result<(), String> {
        match self.timed()?.set_zone(zone) {
            Ok(()) => Ok(()),
            Err(CallError::Transport(errno)) if errno == Errno::INVAL => {
                Err(String::from("bad zone name"))
            }
            // A machine setting, like the clock: a session asks an
            // administrator, and `timed` follows the confd key `elevd` writes.
            Err(error) if -error.code() == errno::EPERM => {
                super::elevd::request("conf.set", &[timezone::ZONE_KEY, "str", zone])
                    .map_err(|error| super::elevd::describe(&error))?;
                if self.await_zone(zone) {
                    Ok(())
                } else {
                    Err(String::from(
                        "approved, but the time service has not switched to it yet",
                    ))
                }
            }
            Err(error) => Err(describe(error.code())),
        }
    }

    fn os_version(&self) -> String {
        uname().unwrap_or_else(|| String::from("LazyOS"))
    }

    fn os_revision(&self) -> Option<String> {
        revision()
    }

    fn os_image(&self) -> Option<String> {
        image()
    }

    fn uptime_secs(&self) -> Option<u64> {
        crate::sysinfo::snapshot()
            .ok()
            .map(|snapshot| snapshot.ticks / HZ)
    }

    fn store_status(&self) -> Option<StoreStatus> {
        ConfStore::info(&ConfdStore::new())
            .ok()
            .map(|info| StoreStatus {
                dir: info.store_dir,
                persistent: info.persistent,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_read_as_text() {
        assert!(describe(-errno::EPERM).contains("CAP_SYS_TIME"));
        assert!(describe(-errno::EINVAL).contains("refused"));
        assert!(describe(-99).contains("-99"));
    }

    #[test]
    fn the_now_reply_round_trips() {
        let body = wire::encode_now_reply(&wire::NowReply {
            unix_ms: 1_790_715_899_500,
            tz_offset_s: 7200,
            tz_name: "Europe/Paris".into(),
            dst: true,
        })
        .unwrap();
        let now = wire::decode_now_reply(&body).unwrap();
        assert_eq!(now.unix_ms.div_euclid(1000), 1_790_715_899);
        assert_eq!(now.tz_name, "Europe/Paris");
    }
}
