//! `NoteKeysHeld` (issue #648 follow-up): while the shell's panel menu has
//! the keyboard, the focused session keeps its focus (no
//! `KeyboardLeave`/`KeyboardEnter`, which a client may read as a new focus)
//! and its key content is held instead. The rules, owed releases included so
//! nothing sticks, are `inputmap::hold` (host-tested); this wires them to the
//! compositor's note, the delivery path and the key-state pages.
//!
//! Serial: `INPUTD:KEYS:HELD on|off`.

use inputmap::hold::Edge;
use inputmap::{KeyOut, KeyState};

use user::messenger::input::shell_wire;
use user::messenger::{Error, Result};
use user::sys;

use super::hub::Hub;

impl Hub {
    /// The compositor's `NoteKeysHeld`.
    pub(super) fn note_keys_held(&mut self, body: &[u8]) -> Result<()> {
        let args = shell_wire::decode_note_keys_held_args(body).map_err(Error::Parcel)?;
        if self.hold.set(args.held, self.engine.down_bits()) {
            // A repeat in flight must not carry on into (or out of) a hold.
            self.engine.cancel_repeat();
            sys::write_str(if args.held {
                "INPUTD:KEYS:HELD on\n"
            } else {
                "INPUTD:KEYS:HELD off\n"
            });
            self.publish_key_pages();
        }
        Ok(())
    }

    /// Whether `key` reaches the focused session under the hold.
    pub(super) fn admit_key(&mut self, key: &KeyOut) -> bool {
        let edge = match key.state {
            KeyState::Down => Edge::Down,
            KeyState::Repeat => Edge::Repeat,
            KeyState::Up => Edge::Up,
        };
        self.hold.admit(key.code, edge)
    }

    /// Whether composed text reaches the focused session (none while held).
    pub(super) fn admit_text(&self) -> bool {
        !self.hold.active()
    }

    /// The keys the focused session's page shows.
    pub(super) fn page_keys(&self) -> [u64; 4] {
        self.hold.mask(self.engine.down_bits())
    }
}
