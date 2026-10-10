//! The center's queue mechanics: ids, events, replace in place, eviction,
//! promotion with coalescing, and closing (with the sink-out).

use lazymotion::{Curve, Motion, Tween};

use super::toast::{Leaving, Toast};
use super::{
    Center, Event, EventKind, COALESCE_AT, LEAVE_MS, OWNER_VISIBLE_MAX, PROGRESS_MS, SLIDE_PX,
    TICK_MS, VISIBLE_MAX, WAITING_MAX,
};
use crate::notify::note::{CloseReason, Level, Request, PROGRESS_DONE};
use crate::notify::policy::SYSTEM_OWNER;

impl Center {
    pub(super) fn fresh_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub(super) fn emit(&mut self, owner: &str, id: u64, kind: EventKind) {
        self.events.push(Event {
            owner: owner.to_owned(),
            id,
            kind,
        });
    }

    pub(super) fn find(&self, id: u64) -> Option<&Toast> {
        self.live
            .iter()
            .chain(self.waiting.iter())
            .find(|toast| toast.id == id)
    }

    pub(super) fn alive_of(&self, owner: &str) -> usize {
        self.live
            .iter()
            .chain(self.waiting.iter())
            .filter(|toast| toast.owner == owner)
            .count()
    }

    pub(super) fn live_of(&self, owner: &str) -> usize {
        self.live
            .iter()
            .filter(|toast| toast.owner == owner)
            .count()
    }

    /// Update the sender's own toast named by `request.replaces`.
    pub(super) fn replace(&mut self, owner: &str, request: &Request, now: u64) -> Option<u64> {
        let id = request.replaces?;
        let toast = self
            .live
            .iter_mut()
            .chain(self.waiting.iter_mut())
            .find(|toast| toast.id == id && toast.owner == owner)?;
        let fill = i32::from(request.note.progress.unwrap_or(0)) * 10;
        let was_done = toast.note.progress.is_none_or(|p| p >= PROGRESS_DONE);
        toast.note = request.note.clone();
        toast.merged = 1;
        toast.rev += 1;
        if toast.note.progress.is_some() {
            let now_ms = now * TICK_MS;
            let from = toast.progress.value(now_ms);
            toast.progress = if was_done && toast.at.is_none() {
                Tween::at_rest(fill)
            } else {
                Tween::new(from, fill, now_ms, PROGRESS_MS, Curve::EaseOut)
            };
        }
        if toast.at.is_some() {
            toast.arm(now);
        }
        Some(id)
    }

    /// Drop waiting toasts beyond [`WAITING_MAX`]: plain ones first, the
    /// oldest first.
    pub(super) fn evict_waiting(&mut self) {
        while self.waiting.len() > WAITING_MAX {
            let index = self
                .waiting
                .iter()
                .position(|toast| toast.owner != SYSTEM_OWNER && toast.plain())
                .or_else(|| {
                    self.waiting
                        .iter()
                        .position(|toast| toast.owner != SYSTEM_OWNER)
                })
                .unwrap_or(0);
            if let Some(toast) = self.waiting.remove(index) {
                self.emit(
                    &toast.owner.clone(),
                    toast.id,
                    EventKind::Closed(CloseReason::Replaced),
                );
            }
        }
    }

    /// Move waiting toasts onto free places, merging a burst of plain ones
    /// from one sender into a summary first.
    pub(super) fn promote(&mut self, _now: u64) {
        while self.live.len() < VISIBLE_MAX {
            let Some(index) = self
                .waiting
                .iter()
                .position(|toast| self.live_of(&toast.owner) < OWNER_VISIBLE_MAX)
            else {
                break;
            };
            let index = self.coalesce(index);
            let Some(toast) = self.waiting.remove(index) else {
                break;
            };
            let (owner, id) = (toast.owner.clone(), toast.id);
            self.live.push(toast);
            self.emit(&owner, id, EventKind::Shown);
        }
    }

    /// If `index` starts a burst of [`COALESCE_AT`] plain waiting toasts
    /// of one sender, replace them by one summary; the index of what to
    /// promote.
    pub(super) fn coalesce(&mut self, index: usize) -> usize {
        let owner = self.waiting[index].owner.clone();
        if !self.waiting[index].plain() || owner == SYSTEM_OWNER {
            return index;
        }
        let burst: Vec<usize> = self
            .waiting
            .iter()
            .enumerate()
            .filter(|(_, toast)| toast.owner == owner && toast.plain())
            .map(|(i, _)| i)
            .collect();
        if burst.len() < COALESCE_AT {
            return index;
        }
        // The newest of the burst stands for all of them.
        let newest = *burst.last().unwrap_or(&index);
        let total: u32 = burst.iter().map(|&i| self.waiting[i].merged).sum();
        let mut summary = self.waiting[newest].clone();
        summary.merged = total;
        summary.note.body = std::mem::take(&mut summary.note.title);
        summary.note.title = format!("{total} notifications");
        summary.note.actions.clear();
        summary.note.level = burst
            .iter()
            .map(|&i| self.waiting[i].note.level)
            .max()
            .unwrap_or(Level::Info);
        summary.rev += 1;
        let first = burst[0];
        let mut merged_away = Vec::new();
        for &i in burst.iter().rev() {
            if let Some(toast) = self.waiting.remove(i) {
                if toast.id != summary.id {
                    merged_away.push((toast.owner, toast.id));
                }
            }
        }
        for (owner, id) in merged_away {
            self.emit(&owner, id, EventKind::Closed(CloseReason::Replaced));
        }
        let at = first.min(self.waiting.len());
        self.waiting.insert(at, summary);
        at
    }

    /// Close toast `id`, shown or waiting, for `reason`.
    pub(super) fn remove(&mut self, id: u64, reason: CloseReason, now: u64) {
        if let Some(index) = self.live.iter().position(|toast| toast.id == id) {
            let toast = self.live.remove(index);
            self.emit(&toast.owner, id, EventKind::Closed(reason));
            self.sink(&toast, now);
            self.promote(now);
        } else if let Some(index) = self.waiting.iter().position(|toast| toast.id == id) {
            if let Some(toast) = self.waiting.remove(index) {
                self.emit(&toast.owner, id, EventKind::Closed(reason));
            }
        }
    }

    /// Start `toast`'s way out, when it was on screen.
    pub(super) fn sink(&mut self, toast: &Toast, now: u64) {
        if toast.at.is_none() || self.motion == Motion::None {
            return;
        }
        let now_ms = now * TICK_MS;
        let top = toast.y.value(now_ms);
        let to = if self.motion.travels() {
            top + SLIDE_PX
        } else {
            top
        };
        self.leaving.push(Leaving {
            id: toast.id,
            height: toast.height,
            y: self.motion.tween(top, to, now_ms, LEAVE_MS, Curve::EaseOut),
            since: now,
            motion: self.motion,
        });
    }
}
