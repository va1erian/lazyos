//! The center's per-toast state: a toast shown or waiting, and one sinking
//! out. Fields are the center's business (`pub(super)`); the presenter reads
//! [`Toast`]'s `id`, `owner`, `note`, `merged` and `rev`.

use lazymotion::{Motion, Tween};

use super::{Note, TICK_MS};

/// One toast, shown or waiting.
#[derive(Clone, Debug)]
pub struct Toast {
    pub id: u64,
    pub owner: String,
    pub note: Note,
    /// How many notifications this stands for (more than 1: a summary).
    pub merged: u32,
    /// Bumped whenever `note` changes, so a presenter re-lays it out.
    pub rev: u32,
    /// Height in design pixels, set by the presenter once it measured the
    /// text; 0 until then (the toast takes no space and is not drawn).
    pub(super) height: i32,
    /// The tick it took its place; `None` until laid out.
    pub(super) at: Option<u64>,
    /// Its top edge, in milliseconds.
    pub(super) y: Tween,
    /// The tick its life ends; `None` while paused or unending.
    pub(super) deadline: Option<u64>,
    /// Ticks of life left while hovered (paused).
    pub(super) left: Option<u64>,
    /// The whole life in ticks, for the countdown bar.
    pub(super) span: u64,
    pub(super) hovered: bool,
    /// The progress bar's fill in thousandths, eased.
    pub(super) progress: Tween,
}

impl Toast {
    pub(super) fn new(id: u64, owner: &str, note: Note) -> Toast {
        let fill = i32::from(note.progress.unwrap_or(0)) * 10;
        Toast {
            id,
            owner: owner.to_owned(),
            note,
            merged: 1,
            rev: 0,
            height: 0,
            at: None,
            y: Tween::at_rest(0),
            deadline: None,
            left: None,
            span: 0,
            hovered: false,
            progress: Tween::at_rest(fill),
        }
    }

    /// Whether a burst of these may be merged into a summary.
    pub(super) fn plain(&self) -> bool {
        !self.note.level.persists() && self.note.actions.is_empty() && self.note.progress.is_none()
    }

    /// Start (or restart) its life at `now`.
    pub(super) fn arm(&mut self, now: u64) {
        self.span = self
            .note
            .lifetime_ms()
            .map_or(0, |ms| u64::from(ms) / TICK_MS);
        if self.span == 0 {
            self.deadline = None;
            self.left = None;
        } else if self.hovered {
            self.deadline = None;
            self.left = Some(self.span);
        } else {
            self.deadline = Some(now.saturating_add(self.span));
            self.left = None;
        }
    }
}

/// A toast sinking out.
#[derive(Clone, Debug)]
pub(super) struct Leaving {
    pub(super) id: u64,
    pub(super) height: i32,
    pub(super) y: Tween,
    /// The tick it started to leave (the reduced-motion fade).
    pub(super) since: u64,
    pub(super) motion: Motion,
}
