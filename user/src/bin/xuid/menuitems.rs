//! The desktop menu's entries, loaded from `confd` (`sys/ui/menu`, schema in
//! the `deskmenu` crate) and validated against `init`'s app registry.
//!
//! The compositor is one task, so the list lives in a single-threaded cell
//! that [`menu`](super::menu) and [`themefeed`](super::themefeed) share; it
//! starts as the built-in defaults, so a late or absent `confd` costs nothing.

use alloc::vec::Vec;
use core::cell::UnsafeCell;
use deskmenu::Entry;
use user::messenger::confd::Client;
use user::messenger::services::{self, INIT_NAME};
use user::sys;

struct Items(UnsafeCell<Vec<Entry>>);

// SAFETY: `xuid` runs its compositor on a single task and never shares this
// across threads; every access goes through `with`/`replace` below, which
// never hold a reference across a call back into this module.
unsafe impl Sync for Items {}

static ITEMS: Items = Items(UnsafeCell::new(Vec::new()));
static INIT: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Run `f` over the current list (the defaults until a load succeeds).
pub(super) fn with<R>(f: impl FnOnce(&[Entry]) -> R) -> R {
    // SAFETY: single-task access (see `Items`); the borrow ends with `f`.
    let list = unsafe { &mut *ITEMS.0.get() };
    if !INIT.swap(true, core::sync::atomic::Ordering::Relaxed) {
        *list = deskmenu::defaults();
    }
    f(list)
}

/// Install `next`; `true` when it differs from the current list.
fn replace(next: Vec<Entry>) -> bool {
    with(|_| ());
    // SAFETY: as in `with`; no other borrow is live here.
    let list = unsafe { &mut *ITEMS.0.get() };
    if *list == next {
        return false;
    }
    *list = next;
    true
}

/// How long the compositor waits on `init` for the app list (1 s at 100 Hz):
/// a stalled supervisor must not freeze compositing and input.
const REGISTRY_TIMEOUT_TICKS: u64 = 100;

struct Ids(UnsafeCell<Option<Vec<alloc::string::String>>>);

// SAFETY: single compositor task, as for `Items`.
unsafe impl Sync for Ids {}

/// The registry ids from the last successful fetch. The registry is fixed for
/// a boot (the shipped manifest), and the user bump allocator never reclaims
/// the reply buffer, so one good answer is reused for later reloads.
static IDS: Ids = Ids(UnsafeCell::new(None));

/// The registry's app ids, or `None` when `init` cannot be asked in time (then
/// only the id syntax is checked and a bad launch answers as unavailable).
fn registry_ids() -> Option<Vec<alloc::string::String>> {
    // SAFETY: single-task access (see `Ids`); the borrow ends before return.
    let cached = unsafe { &mut *IDS.0.get() };
    if cached.is_none() {
        let init = services::resolve_service(INIT_NAME).ok()?;
        let deadline = sys::clock().saturating_add(REGISTRY_TIMEOUT_TICKS);
        let apps = services::fetch_apps_until(&init, Some(deadline)).ok()?;
        *cached = Some(apps.into_iter().map(|app| app.id).collect());
    }
    cached.clone()
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
    let ids = registry_ids();
    let known = |app: &str| {
        ids.as_ref()
            .is_none_or(|ids| ids.iter().any(|id| id == app))
    };
    let next = deskmenu::from_value(stored.as_ref(), &known);
    let changed = replace(next);
    if changed {
        sys::write_str("XUID:MENU:RELOAD\n");
    }
    changed
}
