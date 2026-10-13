//! The label-policy trace (`LAZYOS_LABEL_TRACE=1`, issue #509): one trace
//! line per call a labelled task is refused, so a package's `[permissions]`
//! can be derived from a run of the app instead of by hand.
//!
//! `LABEL:DENY label=<label> iface=<id hex> method=<n>` for a Messenger call
//! (map the id with `idl/manifest.json`), `... resolve=<name>` for a service
//! name, `... topic=<name> mode=<n>` for a topic, `... spawn=<label>` for a
//! refused spawn into a `dev:` label and `... start=<call>` for a program a
//! child label tried to start; `LABEL:ENTER label=<child> pid=<n>
//! program=<path>` says a task entered a child label. The `label` channel is
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

/// Report the task in `slot` entering the child label `child` by `execve` of
/// `program` (`ipc::children`, issue #832): `LABEL:ENTER label=<child>
/// pid=<slot> program=<path>`, the evidence that a helper really runs under
/// its narrower label.
pub fn entered(slot: usize, child: u32, program: &str) {
    if !trace::enabled(Channel::Label) {
        return;
    }
    let label = labels::name_of(child).unwrap_or_default();
    trace::line!(
        Channel::Label,
        "ENTER label={label} pid={slot} program={program}"
    );
}
