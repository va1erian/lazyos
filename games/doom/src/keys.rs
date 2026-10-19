//! The key queue `DG_GetKey` drains, with clean edges.
//!
//! Doom reads key *transitions*. LazyOS can deliver a held key's auto-repeat as
//! more presses (the legacy path has PS/2 typematic, the session path marks it
//! `Repeat`), and a window that loses focus never sees the releases of the keys
//! still held. This queue tracks which keys are down, drops a press of a key
//! already down, drops a release of a key that is up, and releases every held
//! key on [`Keys::release_all`] (focus loss), so the player never runs on after
//! an Alt+Tab.

use std::collections::VecDeque;

/// A press is queued only while fewer transitions than this wait (the engine
/// drains the queue every tic, so only a stalled game fills it). A release is
/// always queued, which stays bounded because each one needs a key that is
/// down: the queue never holds more than `2 * CAPACITY` entries.
pub const CAPACITY: usize = 256;

/// Held keys and the transitions not yet handed to the engine.
pub struct Keys {
    down: [bool; 256],
    queue: VecDeque<(bool, u8)>,
}

impl Default for Keys {
    fn default() -> Keys {
        Keys::new()
    }
}

impl Keys {
    pub fn new() -> Keys {
        Keys {
            down: [false; 256],
            queue: VecDeque::new(),
        }
    }

    /// `key` went down; ignored while it is already down (auto-repeat).
    pub fn press(&mut self, key: u8) {
        if self.down[key as usize] || self.queue.len() >= CAPACITY {
            return;
        }
        self.down[key as usize] = true;
        self.queue.push_back((true, key));
    }

    /// `key` went up; ignored when it is not down.
    pub fn release(&mut self, key: u8) {
        if !self.down[key as usize] {
            return;
        }
        self.down[key as usize] = false;
        self.queue.push_back((false, key));
    }

    /// Release every held key (the window lost the keyboard).
    pub fn release_all(&mut self) {
        for key in 0..=u8::MAX {
            self.release(key);
        }
    }

    /// The oldest transition: `(pressed, key)`.
    pub fn pop(&mut self) -> Option<(bool, u8)> {
        self.queue.pop_front()
    }

    /// Whether `key` is held.
    pub fn is_down(&self, key: u8) -> bool {
        self.down[key as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drain(keys: &mut Keys) -> Vec<(bool, u8)> {
        std::iter::from_fn(|| keys.pop()).collect()
    }

    #[test]
    fn auto_repeat_is_one_press() {
        let mut keys = Keys::new();
        keys.press(b'a');
        keys.press(b'a');
        keys.press(b'a');
        keys.release(b'a');
        assert_eq!(drain(&mut keys), vec![(true, b'a'), (false, b'a')]);
    }

    #[test]
    fn a_stray_release_is_dropped() {
        let mut keys = Keys::new();
        keys.release(b'x');
        assert_eq!(keys.pop(), None);
    }

    #[test]
    fn focus_loss_releases_every_held_key() {
        let mut keys = Keys::new();
        keys.press(1);
        keys.press(200);
        drain(&mut keys);
        keys.release_all();
        assert_eq!(drain(&mut keys), vec![(false, 1), (false, 200)]);
        assert!(!keys.is_down(1) && !keys.is_down(200));
        keys.release_all();
        assert_eq!(keys.pop(), None);
    }

    #[test]
    fn a_full_queue_drops_presses_but_keeps_releases() {
        let mut keys = Keys::new();
        for key in 0..=u8::MAX {
            keys.press(key);
        }
        assert_eq!(keys.queue.len(), CAPACITY);
        // The queue is full of presses; every release still lands.
        keys.release_all();
        assert_eq!(keys.queue.len(), CAPACITY * 2);
        let mut fresh = Keys::new();
        for _ in 0..CAPACITY {
            fresh.queue.push_back((false, 0));
        }
        fresh.press(7);
        assert!(!fresh.is_down(7), "a press past capacity is dropped whole");
    }
}
