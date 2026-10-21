//! The view table: every live view, who owns it and what it has in use.
//!
//! View ids are chosen here (a counter from 1, never reused within the process, so a stale id held by a
//! client can never name a later view). A view is looked up *with* the caller's [`ClientKey`]: a view of
//! another client is indistinguishable from one that does not exist.

use std::collections::BTreeMap;

use crate::fabric::Mapping;
use crate::ident::ClientKey;
use crate::slots::Slots;

/// One light view.
pub struct View {
    pub owner: ClientKey,
    /// The client's event endpoint (a handle the service owns).
    pub channel: u64,
    pub width: u32,
    pub height: u32,
    pub visible: bool,
    pub slots: Slots<Box<dyn Mapping>>,
    /// Frames sent so far; the last `Frame` carried this number.
    pub seq: u64,
    /// `Some(reason)` once the engine gave the view up (a `LostReason` value). The entry stays until
    /// the client calls `DestroyView`, so its quota is still held and its calls still get an answer.
    pub lost: Option<u32>,
    /// The client does not have the newest picture: nothing was delivered yet, or a frame was
    /// dropped (no free slot, a full queue, a size that no longer matches).
    pub dirty: bool,
    /// A `Refresh` is already on its way to the engine for the picture `dirty` stands for.
    pub refresh_sent: bool,
}

impl View {
    pub fn new(owner: ClientKey, channel: u64, width: u32, height: u32) -> Self {
        View {
            owner,
            channel,
            width,
            height,
            visible: true,
            slots: Slots::default(),
            seq: 0,
            lost: None,
            dirty: true,
            refresh_sent: false,
        }
    }

    /// A frame was lost or never seen: the next free slot needs a fresh one.
    pub fn want_frame(&mut self) {
        self.dirty = true;
        self.refresh_sent = false;
    }

    /// A frame reached the client.
    pub fn frame_delivered(&mut self) {
        self.dirty = false;
        self.refresh_sent = false;
    }

    /// Whether the engine should be asked for a whole frame now (and records that it was). Only
    /// when a slot is free to receive it, and once per lost picture.
    pub fn should_refresh(&mut self) -> bool {
        if self.lost.is_some() || !self.dirty || self.refresh_sent || !self.slots.has_free() {
            return false;
        }
        self.refresh_sent = true;
        true
    }
}

/// The table.
pub struct Views {
    map: BTreeMap<u64, View>,
    next: u64,
}

impl Default for Views {
    fn default() -> Self {
        Views {
            map: BTreeMap::new(),
            next: 1,
        }
    }
}

impl Views {
    /// Add a view and return its fresh id; `None` when the ids are used up.
    pub fn insert(&mut self, view: View) -> Option<u64> {
        let id = self.next;
        self.next = id.checked_add(1)?;
        self.map.insert(id, view);
        Some(id)
    }

    /// `id` if it is a view of `client`.
    pub fn get(&self, client: ClientKey, id: u64) -> Option<&View> {
        self.map.get(&id).filter(|view| view.owner == client)
    }

    pub fn get_mut(&mut self, client: ClientKey, id: u64) -> Option<&mut View> {
        self.map.get_mut(&id).filter(|view| view.owner == client)
    }

    /// Any view by id, whoever owns it (for what the engine says).
    pub fn any_mut(&mut self, id: u64) -> Option<&mut View> {
        self.map.get_mut(&id)
    }

    pub fn remove(&mut self, id: u64) -> Option<View> {
        self.map.remove(&id)
    }

    pub fn ids(&self) -> Vec<u64> {
        self.map.keys().copied().collect()
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&u64, &View)> {
        self.map.iter()
    }

    /// Views `client` holds (lost ones included).
    pub fn count_of(&self, client: ClientKey) -> usize {
        self.map
            .values()
            .filter(|view| view.owner == client)
            .count()
    }

    /// Bytes of slot memory `client` has attached.
    pub fn slot_bytes_of(&self, client: ClientKey) -> u64 {
        self.map
            .values()
            .filter(|view| view.owner == client)
            .fold(0, |sum, view| sum.saturating_add(view.slots.bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(label: u32) -> ClientKey {
        ClientKey {
            uid: 1000,
            label,
            task: 0,
        }
    }

    #[test]
    fn ids_are_fresh_and_never_reused() {
        let mut views = Views::default();
        let a = views.insert(View::new(key(1), 10, 4, 4)).unwrap();
        let b = views.insert(View::new(key(1), 11, 4, 4)).unwrap();
        assert!(a > 0 && b > a);
        views.remove(a);
        let c = views.insert(View::new(key(1), 12, 4, 4)).unwrap();
        assert!(c > b);
    }

    #[test]
    fn a_view_is_found_only_by_its_owner() {
        let mut views = Views::default();
        let id = views.insert(View::new(key(1), 10, 4, 4)).unwrap();
        assert!(views.get(key(1), id).is_some());
        assert!(views.get(key(2), id).is_none());
        assert!(views.get_mut(key(2), id).is_none());
        assert!(views.get(key(1), id + 1).is_none());
        assert_eq!((views.count_of(key(1)), views.count_of(key(2))), (1, 0));
    }

    #[test]
    fn the_refresh_is_asked_once_per_lost_picture_and_only_with_a_free_slot() {
        let mut view = View::new(key(1), 10, 4, 4);
        assert!(!view.should_refresh(), "no slot to receive it");
        view.slots.attach(0, Box::new(Null), 64).unwrap();
        assert!(view.should_refresh());
        assert!(!view.should_refresh(), "already asked");
        view.want_frame();
        assert!(view.should_refresh());
        view.frame_delivered();
        assert!(!view.should_refresh(), "nothing is missing");
        view.want_frame();
        view.lost = Some(2);
        assert!(!view.should_refresh(), "a lost view has no pictures");
    }

    struct Null;
    impl Mapping for Null {
        fn capacity(&self) -> usize {
            0
        }
        fn mapped_bytes(&self) -> u64 {
            0
        }
        fn write(&mut self, _bytes: &[u8]) {}
    }
}
