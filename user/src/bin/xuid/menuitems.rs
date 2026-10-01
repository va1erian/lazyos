//! The desktop menu's entries: the configured list from `confd` (`sys/ui/menu`,
//! schema in the `deskmenu` crate), then the apps the package manager installed
//! (`init`'s `ListApps`, rows with `installed` set), then the power rows
//! ([`powermenu`](super::powermenu)). While a power row is being confirmed the
//! menu shows the confirmation rows instead.
//!
//! The configured part is user-editable (Settings) and validated against
//! `init`'s app registry; the installed part is not configurable, it simply
//! follows what is installed, appended after the configured entries so the
//! built-ins keep their order and positions. [`refresh_installed`] re-reads it
//! each time the menu opens: one `ListApps` call, bounded by a deadline.
//!
//! The compositor is one task, so the lists live in a single-threaded cell that
//! [`menu`](super::menu) and [`themefeed`](super::themefeed) share; they start
//! as the built-in defaults, so a late or absent `confd` or `init` costs nothing.

use alloc::string::String;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use deskmenu::Entry;
use user::messenger::confd::Client;
use user::messenger::services::{self, INIT_NAME};
use user::messenger::Endpoint;
use user::sys;

/// Most installed apps shown after the configured entries.
const MAX_INSTALLED: usize = 16;
/// The longest `init` may take to list apps (100 Hz): a quarter second.
const LIST_TICKS: u64 = 25;

struct Lists {
    /// The `sys/ui/menu` entries (or the defaults).
    configured: Vec<Entry>,
    /// The installed apps, as of the last [`refresh_installed`].
    installed: Vec<Entry>,
    /// `configured`, `installed`, then the power rows: what the menu paints
    /// and launches.
    merged: Vec<Entry>,
    /// The confirmation rows while a power row is being confirmed; empty
    /// otherwise.
    confirm: Vec<Entry>,
    /// The cached `init` endpoint for [`refresh_installed`].
    init: Option<Endpoint>,
}

struct Items(UnsafeCell<Lists>);

// SAFETY: `xuid` runs its compositor on a single task and never shares this
// across threads; every access goes through `with`/`replace`/
// `refresh_installed` below, which never hold a reference across a call back
// into this module.
unsafe impl Sync for Items {}

static ITEMS: Items = Items(UnsafeCell::new(Lists {
    configured: Vec::new(),
    installed: Vec::new(),
    merged: Vec::new(),
    confirm: Vec::new(),
    init: None,
}));
static INIT: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// The state, seeding the defaults on first use.
fn lists() -> &'static mut Lists {
    // SAFETY: single-task access (see `Items`); callers drop the borrow before
    // calling back into this module.
    let lists = unsafe { &mut *ITEMS.0.get() };
    if !INIT.swap(true, core::sync::atomic::Ordering::Relaxed) {
        lists.configured = deskmenu::defaults();
        rebuild(lists);
    }
    lists
}

fn rebuild(lists: &mut Lists) {
    lists.merged = lists.configured.clone();
    lists.merged.extend(lists.installed.iter().cloned());
    lists.merged.extend(super::powermenu::entries());
}

/// Run `f` over the current list (the defaults until a load succeeds), or the
/// confirmation rows while one is pending.
pub(super) fn with<R>(f: impl FnOnce(&[Entry]) -> R) -> R {
    let lists = lists();
    if lists.confirm.is_empty() {
        f(&lists.merged)
    } else {
        f(&lists.confirm)
    }
}

/// Show `rows` instead of the list until [`clear_confirm`].
pub(super) fn set_confirm(rows: Vec<Entry>) {
    lists().confirm = rows;
}

/// Back to the full list.
pub(super) fn clear_confirm() {
    lists().confirm.clear();
}

/// Install `next` as the configured list; `true` when it differs.
fn replace(next: Vec<Entry>) -> bool {
    let lists = lists();
    if lists.configured == next {
        return false;
    }
    lists.configured = next;
    rebuild(lists);
    true
}

/// A menu entry for an installed app: its id launches it, its manifest name
/// labels it (capped and cleaned like any other label).
fn installed_entry(id: &str, name: &str) -> Option<Entry> {
    if id.is_empty() {
        return None;
    }
    let label: String = name
        .chars()
        .filter(|c| !c.is_control())
        .take(deskmenu::MAX_LABEL)
        .collect();
    Some(Entry {
        app: String::from(id),
        label: if label.trim().is_empty() {
            String::from(id)
        } else {
            label
        },
    })
}

/// Re-read the installed apps from `init` and rebuild the list. Returns `true`
/// when the visible list changed. An unreachable or slow `init` keeps the
/// previous list.
pub(super) fn refresh_installed() -> bool {
    let lists = lists();
    if lists.init.is_none() {
        lists.init = services::resolve_service(INIT_NAME).ok();
    }
    let Some(init) = lists.init else {
        return false;
    };
    let mut buffer = alloc::vec![0u8; user::messenger::DEFAULT_BUFFER];
    let deadline = Some(sys::clock() + LIST_TICKS);
    let reply = match init.call_with(&services::list_apps_request(), &mut buffer, deadline) {
        Ok(reply) => reply,
        Err(_) => {
            // A dead or wedged endpoint is dropped, so the next open resolves
            // afresh.
            if let Some(stale) = lists.init.take() {
                let _ = stale.release();
            }
            return false;
        }
    };
    let Ok(apps) = services::decode_apps(&reply) else {
        return false;
    };
    let next: Vec<Entry> = apps
        .iter()
        .filter(|app| app.installed)
        .filter_map(|app| installed_entry(&app.id, &app.name))
        .take(MAX_INSTALLED)
        .collect();
    if lists.installed == next {
        return false;
    }
    lists.installed = next;
    rebuild(lists);
    true
}

/// Read `sys/ui/menu` and install it; seeds the defaults when the key is
/// absent. Returns `true` when the visible list changed. A failing `confd`
/// keeps the previous list.
pub(super) fn reload(client: &Client) -> bool {
    let Ok(stored) = client.get(deskmenu::KEY) else {
        return false;
    };
    if stored.is_none() {
        // First boot: make the default list visible to Settings. Best effort.
        if client
            .set(deskmenu::KEY, &deskmenu::to_value(&deskmenu::defaults()))
            .is_err()
        {
            sys::write_str("MENU:SEED:FAIL\n");
        }
    }
    // Every well-formed id is kept, shipped or not (the old hardcoded menu
    // behaved the same): a launch of an unshipped app answers unavailable, and
    // asking `init` here would put a blocking call in the compositor.
    let next = deskmenu::from_value(stored.as_ref(), &|_| true);
    let changed = replace(next);
    if changed {
        sys::write_str("XUID:MENU:RELOAD\n");
    }
    changed
}
