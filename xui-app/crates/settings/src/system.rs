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
    /// Step the wall clock to `unix` (UTC seconds). Needs `CAP_SYS_TIME`.
    fn set_time(&self, unix: i64) -> Result<(), String>;
    /// Switch the system time zone (persisted by the service).
    fn set_zone(&self, zone: &str) -> Result<(), String>;
    /// The OS name and version (`LazyOS 0.1.0 (x86_64)`).
    fn os_version(&self) -> String;
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

/// An in-memory [`System`] for tests and previews.
pub struct MemSystem {
    pub now: RefCell<Option<Now>>,
    pub uptime: Option<u64>,
    pub store: Option<StoreStatus>,
    /// The desktop pictures [`System::wallpapers`] reports.
    pub pictures: Vec<String>,
    /// When set, `set_time` and `set_zone` fail with this message.
    pub fail: RefCell<Option<String>>,
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

    fn os_version(&self) -> String {
        String::from("LazyOS (test)")
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
