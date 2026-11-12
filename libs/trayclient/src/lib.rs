//! The LazyOS tray client (docs/tray-plan.md section 5.1): one app's item on
//! LazyShell's taskbar, over `os.lazy.shell.tray.v1`.
//!
//! An app has at most one item, keyed by its kernel-stamped label, so the
//! client is small: [`Tray::set`] shows (or replaces) the item and hands the
//! shell a fresh event channel, [`Tray::update`] changes parts of it,
//! [`Tray::clear`] removes it, and [`decode_event`] reads what the shell sends
//! on the channel (clicks, menu picks, the wheel, liveness pings).
//!
//! A restarted shell starts with an empty tray and publishes a new generation
//! on the retained `session/<s>/shell/tray` topic. The caller feeds every
//! generation it sees to [`Tray::generation`], which calls `Set` again when it
//! changed, so the item comes back by itself (StatusNotifier's rule).
//!
//! It is `no_std` + `alloc` and knows nothing about syscalls: the calls are the
//! generated `os.lazy.shell.tray.v1` client over a `libmessenger` transport
//! (`xui_app::tray` uses the xui one), and [`Channels`] makes channel pairs.
//! The host tests run it over `libmessenger::client::Mock`.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

mod build;
mod event;

#[cfg(test)]
mod tests;

pub use build::{item, lucide, menu_row, pixels, Item};
pub use event::{decode_event, Event, Rect};
pub use libmessenger::Object;
pub use messenger_generated::os_lazy_shell_tray_events_v1 as events_wire;
pub use messenger_generated::os_lazy_shell_tray_v1 as wire;

use libmessenger::client::{CallError, Errno, Transport};

/// LazyShell's registered name for the tray.
pub const NAME: &str = wire::SERVICE_NAME;

/// A failed call: the transport's failure or the shell's refusal.
pub type Error = CallError;
pub type Result<T> = core::result::Result<T, Error>;

/// The errors a later `Set` may not get: the shell was not reached
/// (`ENOENT`: no service registered, `EPIPE`: it died, `ETIMEDOUT`: it did
/// not answer), or it did not find this task in `init`'s table yet (`ESRCH`:
/// the shell reads that table once per heartbeat, so an app launched a
/// moment ago can be missing from the copy its first `Set` is checked
/// against). Anything else is the shell's refusal.
const TRANSIENT: [i64; 4] = [2, 3, 32, 110];

/// Where an app's event channels come from: the one thing the runtime
/// provides besides the shell's connection (a [`Transport`], which the
/// generated [`wire::Client`] frames its calls over).
pub trait Channels {
    /// A fresh channel pair: `(sent, kept)`. `sent` is transferred to the
    /// shell with `Set`; the app receives its events on `kept`.
    fn create_pair(&mut self) -> core::result::Result<(u64, u64), Errno>;
    /// Close a handle this task holds.
    fn close(&mut self, handle: u64);
}

/// One app's tray item.
pub struct Tray<T: Transport, C: Channels> {
    client: wire::Client<T>,
    channels: C,
    /// The last item set, kept so it can be set again after a shell restart.
    item: Option<wire::Item>,
    /// Where the shell's events arrive.
    events: Option<u64>,
    /// The tray generation the item is registered with.
    registered: Option<u64>,
    /// The newest generation seen.
    seen: Option<u64>,
    /// The last `Set` was refused by the shell (not lost on the way).
    refused: bool,
}

impl<T: Transport, C: Channels> Tray<T, C> {
    /// An app's tray over `client` (`wire::Client::from_connection`; the shell
    /// is reached when a call first needs it, with one attempt).
    pub fn new(client: wire::Client<T>, channels: C) -> Tray<T, C> {
        Tray {
            client,
            channels,
            item: None,
            events: None,
            registered: None,
            seen: None,
            refused: false,
        }
    }

    /// The generated client.
    pub fn client(&self) -> &wire::Client<T> {
        &self.client
    }

    /// The channel source.
    pub fn channels_mut(&mut self) -> &mut C {
        &mut self.channels
    }

    /// Resolve the shell with one attempt when no handle is held: a tray
    /// call must not wait for a shell that is not there (an app's UI thread
    /// makes it), and the next generation sets the item again.
    fn reach(&self) -> Result<()> {
        let conn = self.client.connection();
        if !conn.is_connected() {
            conn.try_connect().map_err(CallError::Transport)?;
        }
        Ok(())
    }

    /// The channel the shell's events arrive on, once an item is set.
    pub fn events(&self) -> Option<u64> {
        self.events
    }

    /// `Set(item)`: show `item`, replacing the app's current one, with a new
    /// event channel. The item is kept for re-registration even when the
    /// call fails (no shell yet), so the next generation sets it.
    pub fn set(&mut self, item: wire::Item) -> Result<()> {
        self.item = Some(item);
        self.register()
    }

    /// `Update(...)`: change the given parts of the item.
    pub fn update(&mut self, patch: wire::UpdateArgs) -> Result<()> {
        if let Some(item) = self.item.as_mut() {
            apply(item, &patch);
        }
        self.reach()?;
        self.client.update(
            patch.icon.as_ref(),
            patch.tooltip.as_deref(),
            patch.status,
            patch.badge.as_deref(),
            patch.menu.as_ref(),
        )
    }

    /// `Clear()`: remove the item and stop re-registering it.
    pub fn clear(&mut self) -> Result<()> {
        self.item = None;
        self.registered = None;
        self.drop_events();
        self.reach()?;
        self.client.clear()
    }

    /// The shell published tray generation `generation`: set the item again
    /// if it is not registered with that generation. Returns whether a `Set`
    /// was sent (and its result).
    pub fn generation(&mut self, generation: u64) -> Option<Result<()>> {
        if self.seen != Some(generation) {
            self.refused = false;
        }
        self.seen = Some(generation);
        if self.item.is_none() || self.registered == Some(generation) {
            return None;
        }
        Some(self.register())
    }

    /// Try again to register with the newest generation seen, after a `Set`
    /// that failed because the shell could not be reached (a restarted
    /// shell not answering yet). Call it now and then; `None` when nothing is
    /// pending. A `Set` the shell refused is not retried until the next
    /// generation: the answer would be the same.
    pub fn retry(&mut self) -> Option<Result<()>> {
        if self.refused {
            return None;
        }
        self.generation(self.seen?)
    }

    /// The event channel broke (the shell went away): forget it, so the next
    /// generation registers afresh.
    pub fn disconnected(&mut self) {
        self.registered = None;
        self.drop_events();
    }

    fn register(&mut self) -> Result<()> {
        let Some(item) = self.item.clone() else {
            return Ok(());
        };
        self.reach()?;
        let (sent, kept) = self.channels.create_pair().map_err(CallError::Transport)?;
        match self.client.set(&item, sent) {
            Ok(()) => {
                self.drop_events();
                self.events = Some(kept);
                self.registered = self.seen.or(Some(0));
                Ok(())
            }
            Err(error) => {
                // A refused call may not have moved `sent`; closing it here
                // is harmless if it did (the handle is no longer ours).
                self.channels.close(sent);
                self.channels.close(kept);
                self.refused = !TRANSIENT.contains(&-error.code());
                Err(error)
            }
        }
    }

    fn drop_events(&mut self) {
        if let Some(events) = self.events.take() {
            self.channels.close(events);
        }
    }
}

/// Apply an `Update` to the kept copy of the item, so a re-`Set` after a
/// shell restart shows what the app last asked for.
fn apply(item: &mut wire::Item, patch: &wire::UpdateArgs) {
    if let Some(icon) = &patch.icon {
        item.icon = icon.clone();
    }
    if let Some(tooltip) = &patch.tooltip {
        item.tooltip = tooltip.clone();
    }
    if let Some(status) = patch.status {
        item.status = status;
    }
    if let Some(badge) = &patch.badge {
        item.badge = (!badge.is_empty()).then(|| badge.clone());
    }
    if let Some(menu) = &patch.menu {
        item.menu = menu.rows.clone();
    }
}
