//! The label-policy trace (`LAZYOS_LABEL_TRACE=1`, issue #509): one trace
//! line per call a labelled task is refused, so a package's `[permissions]`
//! can be derived from a run of the app instead of by hand.
//!
//! `LABEL:DENY label=<label> iface=<id hex> method=<n>` for a Messenger call
//! (map the id with `idl/manifest.json`), `... resolve=<name>` for a service
//! name and `... topic=<name> mode=<n>` for a topic. The `label` channel is
//! off in the default mask; the switch (an alias of `LAZYOS_TRACE_MASK`,
//! `build_support/trace_cfg.rs`) turns it on, and a burst is rate limited.
//! The audit ring records the same denials either way.

use crate::ipc::labels;
use crate::trace;
use tracefmt::Channel;

/// Report one refusal of the labelled task `label_id`.
pub fn denied(label_id: u32, what: core::fmt::Arguments<'_>) {
    if label_id == 0 || !trace::enabled(Channel::Label) {
        return;
    }
    let label = labels::name_of(label_id).unwrap_or_default();
    trace::line!(Channel::Label, "DENY label={label} {what}");
}
