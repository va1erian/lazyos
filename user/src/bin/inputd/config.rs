//! The keyboard layout, from `confd`.
//!
//! `sys/input/layout` selects it live; absent means the image's build-time
//! default (`LAZYOS_KBD_LAYOUT`, `us` unless set). Like `timed`, the `confd`
//! link is opened lazily and dropped on the first failure: `inputd` starts
//! beside `confd` and must keep typing working while it is not reachable.

use alloc::vec::Vec;

use inputmap::{Layout, LAYOUT_KEY};
use user::central::Subscription;
use user::messenger::confd::{name_system_confd_changed, Client as Confd};
use user::messenger::{self, Error, EXPIRED_DEADLINE};
use user::sys;

/// Ticks (100 Hz) between attempts to reach `confd`.
const RETRY_TICKS: u64 = 100;

/// The layout an image boots with when `confd` holds none.
pub(super) fn default_layout() -> Layout {
    match option_env!("LAZYOS_KBD_LAYOUT") {
        Some(name) => Layout::from_name(name).unwrap_or(Layout::Us),
        None => Layout::Us,
    }
}

pub(super) struct Config {
    confd: Option<Confd>,
    watch: Option<Subscription>,
    synced: bool,
    next_try: u64,
    buffer: Vec<u8>,
}

impl Config {
    pub(super) fn new() -> Config {
        Config {
            confd: None,
            watch: None,
            synced: false,
            next_try: 0,
            buffer: alloc::vec![0u8; messenger::DEFAULT_BUFFER],
        }
    }

    /// The layout to use if it may have changed since the last call: `Some`
    /// after a (re)read of `confd`, `None` when nothing changed.
    pub(super) fn poll(&mut self) -> Option<Layout> {
        if !self.synced {
            if sys::clock() < self.next_try {
                return None;
            }
            return self.sync();
        }
        let mut changed = false;
        let mut lost = false;
        if let Some(watch) = &self.watch {
            loop {
                match watch.recv_with(&mut self.buffer, Some(EXPIRED_DEADLINE)) {
                    Ok(Some(_)) => changed = true,
                    Ok(None) => break,
                    Err(_) => {
                        lost = true;
                        break;
                    }
                }
            }
        }
        if lost {
            self.drop_link();
            return None;
        }
        if !changed {
            return None;
        }
        match self.read() {
            Ok(layout) => Some(layout),
            Err(_) => {
                self.drop_link();
                None
            }
        }
    }

    /// Read the layout and subscribe to changes.
    fn sync(&mut self) -> Option<Layout> {
        let layout = match self.read() {
            Ok(layout) => layout,
            Err(_) => {
                self.drop_link();
                return None;
            }
        };
        let topic = name_system_confd_changed(LAYOUT_KEY).ok();
        self.watch = match (&self.confd, topic) {
            (Some(confd), Some(topic)) => confd.watch(&topic).ok(),
            _ => None,
        };
        if self.watch.is_some() {
            self.synced = true;
        } else {
            // Without a subscription changes would go unnoticed: retry the
            // whole sync shortly.
            self.next_try = sys::clock() + RETRY_TICKS;
        }
        Some(layout)
    }

    /// The layout `confd` holds; absent or unknown falls back to the default
    /// rather than trusting arbitrary text.
    fn read(&mut self) -> Result<Layout, Error> {
        if self.confd.is_none() {
            self.confd = Some(Confd::connect()?);
        }
        let client = self
            .confd
            .as_ref()
            .ok_or(Error::Errno(-messenger::errno::ENOENT))?;
        Ok(match client.get(LAYOUT_KEY)? {
            Some(confd::Value::Str(name)) => {
                Layout::from_name(&name).unwrap_or_else(default_layout)
            }
            Some(_) | None => default_layout(),
        })
    }

    fn drop_link(&mut self) {
        if let Some(watch) = self.watch.take() {
            let _ = watch.unsubscribe();
        }
        self.confd = None;
        self.synced = false;
        self.next_try = sys::clock() + RETRY_TICKS;
    }
}
