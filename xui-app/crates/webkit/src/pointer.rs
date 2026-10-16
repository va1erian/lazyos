//! Pointer moves merged while WebKit's main thread is busy (issue #890).
//!
//! Every engine call queues a task for WebKit's run loop. A move each time the pointer moves would
//! pile up behind a long paint or a slow call, each task an allocation, and be replayed one by one
//! afterwards: the page would follow the hand late and lay out a burst of `mousemove`s. So a move
//! only fills one slot (the latest position, buttons and modifiers), and one task is queued while
//! the slot is pending; that task delivers whatever the slot holds when it runs.
//!
//! Order is kept: a press, a release, a wheel or a key first takes the pending move out of the slot and
//! queues it as an event of its own, ahead of itself, and makes the task already queued for the slot
//! a no-op (each task carries the number of the slot's turn it was queued for). A move after that
//! queues a new task, behind the press. Other calls (a resize, the focus, a load) do not touch the
//! slot: a move may reach the page before one of those queued earlier, which nothing depends on.

use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::engine::Button;

/// One pointer move.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Move {
    pub button: Button,
    pub x: i32,
    pub y: i32,
    pub mods: u32,
}

struct Slot {
    pending: Option<Move>,
    /// A task is queued that will deliver the slot for turn `turn`.
    posted: bool,
    turn: u64,
}

/// The slot, shared by the window's thread (which fills it) and the main thread (which empties it).
pub struct Coalescer {
    slot: Mutex<Slot>,
}

impl Default for Coalescer {
    fn default() -> Self {
        Coalescer::new()
    }
}

impl Coalescer {
    pub const fn new() -> Coalescer {
        Coalescer {
            slot: Mutex::new(Slot {
                pending: None,
                posted: false,
                turn: 0,
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Slot> {
        self.slot.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The pointer moved. `Some(turn)` when the caller must queue a task that calls
    /// [`Coalescer::drain`] with `turn`; `None` when one is queued already (it will carry this move).
    pub fn offer(&self, update: Move) -> Option<u64> {
        let mut slot = self.lock();
        slot.pending = Some(update);
        if slot.posted {
            return None;
        }
        slot.posted = true;
        slot.turn = slot.turn.wrapping_add(1);
        Some(slot.turn)
    }

    /// Another pointer event is about to be queued: the move that must reach the page before it, if
    /// any. The task queued for the slot finds nothing to deliver.
    pub fn take(&self) -> Option<Move> {
        let mut slot = self.lock();
        if slot.posted {
            slot.posted = false;
            slot.turn = slot.turn.wrapping_add(1);
        }
        slot.pending.take()
    }

    /// On the main thread, the task queued for `turn` runs: the move to deliver now, or `None` when
    /// [`Coalescer::take`] already sent it ahead of another event.
    pub fn drain(&self, turn: u64) -> Option<Move> {
        let mut slot = self.lock();
        if !slot.posted || slot.turn != turn {
            return None;
        }
        slot.posted = false;
        slot.pending.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{Call, Engine, MouseKind, Recorder};

    /// What the run loop holds, in order: a task for the slot, or an event queued as itself.
    enum Task {
        Drain(u64),
        Event(MouseKind, Move),
    }

    fn at(x: i32) -> Move {
        Move {
            button: Button::None,
            x,
            y: x / 2,
            mods: 0,
        }
    }

    /// The window's side, as `FfiEngine` does it.
    fn mouse(queue: &mut Vec<Task>, slot: &Coalescer, kind: MouseKind, update: Move) {
        if kind == MouseKind::Move {
            if let Some(turn) = slot.offer(update) {
                queue.push(Task::Drain(turn));
            }
            return;
        }
        if let Some(pending) = slot.take() {
            queue.push(Task::Event(MouseKind::Move, pending));
        }
        queue.push(Task::Event(kind, update));
    }

    /// The main thread, unblocked: runs every queued task against the recording engine.
    fn run(queue: &mut Vec<Task>, slot: &Coalescer, engine: &Recorder) {
        for task in queue.drain(..) {
            let (kind, update) = match task {
                Task::Drain(turn) => match slot.drain(turn) {
                    Some(update) => (MouseKind::Move, update),
                    None => continue,
                },
                Task::Event(kind, update) => (kind, update),
            };
            engine.mouse(kind, update.button, update.x, update.y, update.mods);
        }
    }

    fn moved(x: i32) -> Call {
        Call::Mouse(MouseKind::Move, Button::None, x, x / 2, 0)
    }

    #[test]
    fn a_thousand_moves_during_a_blocked_main_thread_are_delivered_as_one() {
        let (slot, engine, mut queue) = (Coalescer::new(), Recorder::default(), Vec::new());
        for x in 0..1000 {
            mouse(&mut queue, &slot, MouseKind::Move, at(x));
        }
        assert_eq!(queue.len(), 1, "one task for the lot");
        run(&mut queue, &slot, &engine);
        assert_eq!(engine.take(), vec![moved(999)]);
        // The next move queues a task again.
        mouse(&mut queue, &slot, MouseKind::Move, at(5));
        run(&mut queue, &slot, &engine);
        assert_eq!(engine.take(), vec![moved(5)]);
    }

    #[test]
    fn a_press_comes_after_the_move_before_it_and_before_the_moves_after_it() {
        let (slot, engine, mut queue) = (Coalescer::new(), Recorder::default(), Vec::new());
        for x in 0..10 {
            mouse(&mut queue, &slot, MouseKind::Move, at(x));
        }
        let press = Move {
            button: Button::Left,
            ..at(9)
        };
        mouse(&mut queue, &slot, MouseKind::Down, press);
        for x in 20..30 {
            mouse(&mut queue, &slot, MouseKind::Move, at(x));
        }
        mouse(&mut queue, &slot, MouseKind::Up, press);
        run(&mut queue, &slot, &engine);
        assert_eq!(
            engine.take(),
            vec![
                moved(9),
                Call::Mouse(MouseKind::Down, Button::Left, 9, 4, 0),
                moved(29),
                Call::Mouse(MouseKind::Up, Button::Left, 9, 4, 0),
            ]
        );
    }

    #[test]
    fn a_task_that_runs_while_moves_still_arrive_delivers_the_latest_and_a_new_one_follows() {
        let (slot, engine, mut queue) = (Coalescer::new(), Recorder::default(), Vec::new());
        mouse(&mut queue, &slot, MouseKind::Move, at(1));
        run(&mut queue, &slot, &engine);
        mouse(&mut queue, &slot, MouseKind::Move, at(2));
        mouse(&mut queue, &slot, MouseKind::Move, at(3));
        run(&mut queue, &slot, &engine);
        assert_eq!(engine.take(), vec![moved(1), moved(3)]);
        // A press with nothing pending queues only itself.
        mouse(&mut queue, &slot, MouseKind::Down, at(3));
        assert_eq!(queue.len(), 1);
    }

    #[test]
    fn threads_filling_and_emptying_the_slot_never_lose_the_last_move() {
        use std::sync::mpsc;
        let slot = std::sync::Arc::new(Coalescer::new());
        let (tasks, inbox) = mpsc::channel::<u64>();
        let window = {
            let slot = std::sync::Arc::clone(&slot);
            std::thread::spawn(move || {
                for x in 0..20_000 {
                    if let Some(turn) = slot.offer(at(x)) {
                        tasks.send(turn).unwrap();
                    }
                }
            })
        };
        let mut last = None;
        let mut delivered = 0;
        for turn in inbox {
            if let Some(update) = slot.drain(turn) {
                assert!(last.map_or(true, |seen: i32| update.x > seen), "in order");
                last = Some(update.x);
                delivered += 1;
            }
        }
        window.join().unwrap();
        assert_eq!(last, Some(19_999));
        assert!(delivered <= 20_000);
    }
}
