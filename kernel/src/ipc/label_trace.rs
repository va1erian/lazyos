//! The label-policy trace (`LAZYOS_LABEL_TRACE=1`, issue #509): one serial
//! line per call a labelled task is refused, so a package's `[permissions]`
//! can be derived from a run of the app instead of by hand.
//!
//! `LABEL:DENY label=<label> iface=<id hex> method=<n>` for a Messenger call
//! (map the id with `idl/manifest.json`), `... resolve=<name>` for a service
//! name and `... topic=<name> mode=<n>` for a topic. Compiled only with the
//! switch; the audit ring records the same denials either way.

use crate::ipc::labels;

/// Report one refusal of the labelled task `label_id`.
pub fn denied(label_id: u32, what: core::fmt::Arguments<'_>) {
    if label_id == 0 {
        return;
    }
    let label = labels::name_of(label_id).unwrap_or_default();
    crate::serial_println!("LABEL:DENY label={label} {what}");
}
