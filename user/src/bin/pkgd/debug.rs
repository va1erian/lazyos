//! Who may install an uploaded package (docs/dbgd-plan.md, v2 app
//! swapping): `dbgd`'s kernel-stamped identity, on a box whose
//! `/boot/lazyos.cfg` says `diag.dbg.control=1`. The switch is read here,
//! not taken from the requester; an image built without `LAZYOS_DBGD` has
//! no `dbgd` and refuses every call.

use pkgstore::access::Caller;

/// Whether `caller` is `dbgd` on a box that allows remote control.
#[cfg(lazyos_dbgd)]
pub(crate) fn allowed(caller: &Caller) -> bool {
    use core::sync::atomic::{AtomicU8, Ordering};
    /// Bytes of `lazyos.cfg` read (the kernel caps the file at 4 KiB).
    const CFG_LIMIT: usize = 4096;
    static KNOWN: AtomicU8 = AtomicU8::new(0);
    let dbgd =
        caller.uid == dbgwire::config::DBGD_UID && caller.label_id == 0 && caller.session == 0;
    if !dbgd {
        return false;
    }
    match KNOWN.load(Ordering::Relaxed) {
        1 => false,
        2 => true,
        _ => {
            let on = user::files::read_up_to(fhs::boot::LAZYOS_CFG_PATH, CFG_LIMIT)
                .ok()
                .and_then(|bytes| alloc::string::String::from_utf8(bytes).ok())
                .is_some_and(|cfg| dbgwire::config::control_enabled(&cfg));
            KNOWN.store(if on { 2 } else { 1 }, Ordering::Relaxed);
            on
        }
    }
}

#[cfg(not(lazyos_dbgd))]
pub(crate) fn allowed(_caller: &Caller) -> bool {
    false
}
