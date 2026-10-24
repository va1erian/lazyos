//! What one [`EventQueue::drain`](super::EventQueue::drain) hands the service.

use std::collections::BTreeMap;

use super::pictures::Update;
use super::{BandNow, FrameNow, Once, PageNow, StateNow};

/// Everything queued since the last drain.
#[derive(Debug, Default)]
pub struct Drained {
    pub states: BTreeMap<u64, StateNow>,
    pub scrolls: BTreeMap<u64, (i32, i32, bool)>,
    pub frames: BTreeMap<u64, FrameNow>,
    pub once: Vec<Once>,
    pub pages: BTreeMap<u64, PageNow>,
    /// The pictures of views with deltas.
    pub pictures: BTreeMap<u64, Update>,
    pub bands: BTreeMap<u64, Vec<BandNow>>,
    pub hovers: BTreeMap<u64, String>,
    pub cursors: BTreeMap<u64, i32>,
    pub responsive: BTreeMap<u64, bool>,
    pub blocked: BTreeMap<u64, i32>,
    /// The newest answer to the wheel per view: `(seq, x, y)` (`webd_ui_wheel_done`; each answers all before it).
    pub wheels: BTreeMap<u64, (u64, i32, i32)>,
}

impl Drained {
    pub fn is_empty(&self) -> bool {
        self.states.is_empty()
            && self.scrolls.is_empty()
            && self.frames.is_empty()
            && self.once.is_empty()
            && self.pages.is_empty()
            && self.pictures.is_empty()
            && self.bands.is_empty()
            && self.hovers.is_empty()
            && self.cursors.is_empty()
            && self.responsive.is_empty()
            && self.blocked.is_empty()
            && self.wheels.is_empty()
    }
}
