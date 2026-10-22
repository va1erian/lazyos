//! Pointer moves to webd: one is sent at once, then at most one every [`MOVE_INTERVAL_MS`], the
//! newest waiting in between (issue #890's merging, in the client's process: webd's main thread
//! never sees a flood). A press, a release, a wheel turn or a key first sends the move that waits,
//! so the page sees the pointer where they happened.

use xui_webd_client::transport::ScopedTransport;
use xui_webd_client::MouseKind;
use xui_webkit::engine::Button;

use crate::{RemoteEngine, MOVE_INTERVAL_MS};

/// A move not sent yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Move {
    pub button: Button,
    pub x: i32,
    pub y: i32,
    pub mods: u32,
}

#[derive(Debug, Default)]
pub(crate) struct Moves {
    waiting: Option<Move>,
    /// When the last move went (engine milliseconds); `None` before the first.
    sent_at: Option<u64>,
}

impl Moves {
    pub(crate) fn waiting(&self) -> bool {
        self.waiting.is_some()
    }

    pub(crate) fn clear(&mut self) {
        self.waiting = None;
    }

    /// A move: the one to send now, if it is time (else it waits).
    fn offer(&mut self, update: Move, now: u64) -> Option<Move> {
        let due = self
            .sent_at
            .is_none_or(|at| now.saturating_sub(at) >= MOVE_INTERVAL_MS);
        if due {
            self.waiting = None;
            self.sent_at = Some(now);
            Some(update)
        } else {
            self.waiting = Some(update);
            None
        }
    }

    /// The move that waits: always with `force`, else once it is time.
    fn take(&mut self, now: u64, force: bool) -> Option<Move> {
        let due = force
            || self
                .sent_at
                .is_none_or(|at| now.saturating_sub(at) >= MOVE_INTERVAL_MS);
        if !due {
            return None;
        }
        let update = self.waiting.take()?;
        self.sent_at = Some(now);
        Some(update)
    }
}

impl<T: ScopedTransport> RemoteEngine<T> {
    /// A pointer move from the window.
    pub(crate) fn offer_move(&self, update: Move) {
        let now = self.now_ms();
        let send = self.inner.borrow_mut().moves.offer(update, now);
        if let Some(update) = send {
            self.send_move(update);
        }
    }

    /// Sends the move that waits (all of it with `force`, else when it is time).
    pub(crate) fn flush_move(&self, now: u64, force: bool) {
        let send = self.inner.borrow_mut().moves.take(now, force);
        if let Some(update) = send {
            self.send_move(update);
        }
    }

    fn send_move(&self, update: Move) {
        self.inner.borrow_mut().view.mouse(
            MouseKind::Move,
            update.button.code().max(0),
            update.x,
            update.y,
            update.mods,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(x: i32) -> Move {
        Move {
            button: Button::None,
            x,
            y: 0,
            mods: 0,
        }
    }

    #[test]
    fn the_first_move_goes_at_once_and_the_rest_wait_their_turn() {
        let mut moves = Moves::default();
        assert_eq!(moves.offer(at(1), 100), Some(at(1)));
        assert_eq!(moves.offer(at(2), 101), None);
        assert_eq!(moves.offer(at(3), 102), None, "the newest replaces the one that waited");
        assert_eq!(moves.take(103, false), None, "not yet");
        assert_eq!(moves.take(100 + MOVE_INTERVAL_MS, false), Some(at(3)));
        assert_eq!(moves.take(500, false), None, "nothing waits");
        assert!(!moves.waiting());
    }

    #[test]
    fn a_click_sends_the_waiting_move_first() {
        let mut moves = Moves::default();
        moves.offer(at(1), 10);
        moves.offer(at(2), 11);
        assert_eq!(moves.take(11, true), Some(at(2)));
    }
}
