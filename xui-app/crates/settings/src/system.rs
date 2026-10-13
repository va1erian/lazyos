//! The system seam: the time service and the facts the About page shows.
//!
//! On LazyOS the binary implements [`System`] over `timed`, `confd` and the
//! kernel's `sysinfo`; [`MemSystem`] stands in for tests and previews, the same
//! split as [`ConfigStore`](crate::store::ConfigStore).

use std::cell::RefCell;

/// The current instant as `timed` reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Now {
    /// UTC seconds since the epoch.
    pub unix: i64,
    /// The local offset in seconds, DST included.
    pub offset: i32,
    /// The zone name (`Europe/Paris`).
    pub zone: String,
}

/// What the network time client is doing (`timed`'s `NtpState`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NtpState {
    Disabled,
    WaitingForNetwork,
    Syncing,
    Synchronized,
    Failed,
}

/// `timed`'s `NtpStatus`: the network time client and its last exchange.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NtpStatus {
    pub enabled: bool,
    pub state: NtpState,
    /// The server of the last accepted exchange, empty when none yet.
    pub server: String,
    /// Server minus local clock at the last accepted exchange, milliseconds.
    pub offset_ms: i64,
    /// UTC seconds of the last accepted exchange, 0 when none yet.
    pub last_sync_unix: i64,
}

/// Where `confd` keeps the settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoreStatus {
    /// The store directory (`/data/confd`).
    pub dir: String,
    /// Whether it survives a reboot (not the ramfs fallback).
    pub persistent: bool,
}

/// Time and system facts. `&self` methods, like the config store, so one
/// `Rc<dyn System>` is shared by the pages.
pub trait System {
    /// The current instant, or `None` when the time service is unreachable.
    fn now(&self) -> Option<Now>;
    /// Step the wall clock to `unix` (UTC seconds). Needs `CAP_SYS_TIME`,
    /// or an administrator's approval through `elevd`.
    fn set_time(&self, unix: i64) -> Result<(), String>;
    /// Switch the system time zone (persisted by the service). The same
    /// rule as [`System::set_time`]: a machine setting.
    fn set_zone(&self, zone: &str) -> Result<(), String>;
    /// What the network time client is doing, or `None` when the time
    /// service is unreachable.
    fn ntp_status(&self) -> Option<NtpStatus> {
        None
    }
    /// Ask the network time client to query a server now (open to every
    /// user; the service rate-limits it).
    fn ntp_sync_now(&self) -> Result<(), String> {
        Err(String::from("network time is not available"))
    }
    /// The OS name and version (`LazyOS 0.1.0 (x86_64)`).
    fn os_version(&self) -> String;
    /// The SHA-1 of the source revision the OS was built from, when known.
    fn os_revision(&self) -> Option<String> {
        None
    }
    /// The build's image id (`<yyyymmdd-HHMMSS>-<rev8>[-dirty]`), when the
    /// boot config named one (a `LAZYOS_NETBOOT=1` build, docs/netboot-plan.md).
    fn os_image(&self) -> Option<String> {
        None
    }
    /// Seconds since boot, when known.
    fn uptime_secs(&self) -> Option<u64>;
    /// Where the settings live, or `None` when `confd` cannot be reached.
    fn store_status(&self) -> Option<StoreStatus>;
    /// The desktop pictures the system ships, as absolute paths in list
    /// order; empty when there are none.
    fn wallpapers(&self) -> Vec<String> {
        Vec::new()
    }
}

/// The image id the kernel appends to `/proc/version` (` image <id>`,
/// `kernel/src/process/linux/procinfo.rs`), when it names one: 1 to 64 of
/// `[0-9A-Za-z._-]`, the kernel's own rule for the id.
pub fn image_from_proc_version(text: &str) -> Option<String> {
    let (_, id) = text.trim_end().rsplit_once(" image ")?;
    let valid = (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    valid.then(|| id.to_owned())
}

/// An in-memory [`System`] for tests and previews.
pub struct MemSystem {
    pub now: RefCell<Option<Now>>,
    pub uptime: Option<u64>,
    pub store: Option<StoreStatus>,
    /// The desktop pictures [`System::wallpapers`] reports.
    pub pictures: Vec<String>,
    /// When set, `set_time` and `set_zone` fail with this message.
    pub fail: RefCell<Option<String>>,
    /// What [`System::ntp_status`] reports.
    pub ntp: RefCell<Option<NtpStatus>>,
    /// How many times [`System::ntp_sync_now`] was asked.
    pub syncs: RefCell<u32>,
}

impl Default for MemSystem {
    fn default() -> MemSystem {
        MemSystem {
            now: RefCell::new(Some(Now {
                unix: 0,
                offset: 0,
                zone: String::from("UTC"),
            })),
            uptime: Some(0),
            store: Some(StoreStatus {
                dir: String::from("(memory)"),
                persistent: true,
            }),
            pictures: Vec::new(),
            fail: RefCell::new(None),
            ntp: RefCell::new(None),
            syncs: RefCell::new(0),
        }
    }
}

impl MemSystem {
    fn check(&self) -> Result<(), String> {
        match self.fail.borrow().as_ref() {
            Some(message) => Err(message.clone()),
            None => Ok(()),
        }
    }
}

impl System for MemSystem {
    fn now(&self) -> Option<Now> {
        self.now.borrow().clone()
    }

    fn set_time(&self, unix: i64) -> Result<(), String> {
        self.check()?;
        if let Some(now) = self.now.borrow_mut().as_mut() {
            now.unix = unix;
        }
        Ok(())
    }

    fn set_zone(&self, zone: &str) -> Result<(), String> {
        self.check()?;
        let found = timezone::find(zone).ok_or_else(|| String::from("unknown time zone"))?;
        if let Some(now) = self.now.borrow_mut().as_mut() {
            now.zone = found.name.to_owned();
            now.offset = timezone::local(found, now.unix).offset;
        }
        Ok(())
    }

    fn ntp_status(&self) -> Option<NtpStatus> {
        self.ntp.borrow().clone()
    }

    fn ntp_sync_now(&self) -> Result<(), String> {
        self.check()?;
        *self.syncs.borrow_mut() += 1;
        Ok(())
    }

    fn os_version(&self) -> String {
        String::from("LazyOS (test)")
    }

    fn os_revision(&self) -> Option<String> {
        Some(String::from("0123456789abcdef0123456789abcdef01234567"))
    }

    fn os_image(&self) -> Option<String> {
        Some(String::from("20261012-093000-01234567"))
    }

    fn uptime_secs(&self) -> Option<u64> {
        self.uptime
    }

    fn store_status(&self) -> Option<StoreStatus> {
        self.store.clone()
    }

    fn wallpapers(&self) -> Vec<String> {
        self.pictures.clone()
    }
}
