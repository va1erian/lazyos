//! Saving the blocker's settings off the caller's thread (issue #894).
//!
//! A save is three `confd` calls; the shield button and `about:blocker`'s switches change a setting
//! on the window's thread, which must not wait for them. A change takes effect in memory at once and
//! hands a snapshot to the [`Saver`], whose thread writes it. Only the newest snapshot waiting is
//! written: switches flipped while a save runs end in one more save, of what the user chose last.
//! The thread exists while there is something to save and ends when there is not.

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use super::{Persist, Settings};

#[derive(Default)]
struct State {
    /// The newest snapshot nobody has started writing, and where it goes.
    pending: Option<(Arc<dyn Persist>, Settings)>,
    /// A thread is writing, or about to: a new snapshot only replaces `pending`.
    running: bool,
}

/// Writes snapshots of the settings on a thread of its own, newest last.
#[derive(Clone, Default)]
pub struct Saver {
    shared: Arc<(Mutex<State>, Condvar)>,
}

impl Saver {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.shared.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queues `settings` for `persist`, replacing a snapshot that is still waiting, and returns at
    /// once. Callers hand snapshots over in the order they were taken (the blocker does it under its
    /// settings lock), so the last one written is the newest.
    pub fn submit(&self, persist: Arc<dyn Persist>, settings: Settings) {
        let start = {
            let mut state = self.lock();
            state.pending = Some((persist, settings));
            !std::mem::replace(&mut state.running, true)
        };
        if !start {
            return;
        }
        let saver = self.clone();
        let spawned = thread::Builder::new()
            .name(String::from("blocksave"))
            .spawn(move || saver.run());
        if spawned.is_err() {
            // No thread (task_max, memory): the snapshot stays queued for the next change or the
            // flush at the end, which write it. Never here: the caller holds the settings lock.
            self.lock().running = false;
        }
    }

    /// Writes snapshots until none is waiting.
    fn run(&self) {
        loop {
            let next = {
                let mut state = self.lock();
                match state.pending.take() {
                    Some(next) => next,
                    None => {
                        state.running = false;
                        self.shared.1.notify_all();
                        return;
                    }
                }
            };
            let (persist, settings) = next;
            persist.save(&settings);
        }
    }

    /// Waits up to `limit` for every queued snapshot to be written (the window is closing, the
    /// process exits next). True when nothing is left to write.
    pub fn flush(&self, limit: Duration) -> bool {
        let until = Instant::now() + limit;
        let mut state = self.lock();
        if !state.running && state.pending.is_some() {
            // Queued when no thread could start: written now, on the flushing thread.
            state.running = true;
            drop(state);
            self.run();
            state = self.lock();
        }
        while state.running {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            state = self
                .shared
                .1
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        true
    }
}
