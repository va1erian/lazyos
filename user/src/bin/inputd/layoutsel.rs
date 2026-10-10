//! Which keyboard layout is in effect: the machine default (`confd`'s
//! `sys/input/layout`, followed by `config.rs`) unless the compositor named
//! the logged-in user's own (`NoteSessionLayout`). `inputd` never reads a
//! user's keys itself: it holds no `confd` authority beyond `sys/**`, and it
//! does not know who is logged in. The rule is `inputmap::session_layout`
//! (host-tested); this applies its result and tells the sessions.
//!
//! Serial, when the session layout changes:
//! `INPUTD:LAYOUT:SESSION <name>|none effective=<name>` (`none` alone when
//! the compositor went away).

use alloc::format;
use alloc::vec::Vec;

use inputmap::Layout;
use user::messenger::input::{shell_wire, wire};
use user::messenger::{Error, Result};
use user::sys;

use super::hub::Hub;

impl Hub {
    /// The machine default changed; whether the layout in effect did.
    pub(super) fn set_machine_layout(&mut self, layout: Layout) -> bool {
        let effective = self.layouts.set_machine(layout);
        self.adopt_layout(effective)
    }

    /// The compositor's `NoteSessionLayout`.
    pub(super) fn note_session_layout(&mut self, body: &[u8]) -> Result<()> {
        let args = shell_wire::decode_note_session_layout_args(body).map_err(Error::Parcel)?;
        let before = self.layouts.session();
        let effective = self.layouts.set_session(args.layout.as_deref());
        if self.layouts.session() != before {
            let named = self.layouts.session().map_or("none", Layout::name);
            sys::write_str(&format!(
                "INPUTD:LAYOUT:SESSION {named} effective={}\n",
                effective.name()
            ));
        }
        self.adopt_layout(effective);
        Ok(())
    }

    /// The compositor went away: its user's layout ends with it.
    pub(super) fn end_session_layout(&mut self) {
        if self.layouts.session().is_some() {
            let effective = self.layouts.set_session(None);
            sys::write_str("INPUTD:LAYOUT:SESSION none\n");
            self.adopt_layout(effective);
        }
    }

    /// Make `layout` the one keys are mapped with and tell every session (the
    /// layout name is not secret); whether it changed.
    fn adopt_layout(&mut self, layout: Layout) -> bool {
        if layout == self.engine.layout() {
            return false;
        }
        self.engine.set_layout(layout);
        let sessions: Vec<u64> = self.router.sessions().collect();
        for session in sessions {
            let body = wire::encode_layout_changed_args(&wire::LayoutChangedArgs {
                layout: layout.name().into(),
            });
            self.send(session, wire::METHOD_LAYOUTCHANGED, body);
        }
        true
    }
}
