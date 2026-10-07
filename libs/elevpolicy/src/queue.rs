//! The requests waiting for the prompt (review of #659, H4).
//!
//! `elevd` keeps reading requests while a prompt is up, so a program cannot
//! park a pile of them behind the one being answered: each caller ([`Caller`])
//! has at most one request in hand at a time, the one being answered or one
//! waiting, and at most [`MAX_WAITING`] wait altogether. Anything more is
//! refused at once (`EBUSY`), without a prompt.

use alloc::collections::VecDeque;

use crate::approvals::Caller;

/// Most requests waiting behind the one being answered.
pub const MAX_WAITING: usize = 8;

/// The waiting requests, oldest first, each with its caller.
#[derive(Debug)]
pub struct Queue<T> {
    waiting: VecDeque<(Caller, T)>,
}

impl<T> Default for Queue<T> {
    fn default() -> Queue<T> {
        Queue {
            waiting: VecDeque::new(),
        }
    }
}

impl<T> Queue<T> {
    pub fn new() -> Queue<T> {
        Queue::default()
    }

    /// Queue `item` from `caller`, or hand it back (to be refused) when that
    /// caller already has a request in hand (`active`, the one being
    /// answered, or a waiting one) or the queue is full.
    pub fn admit(&mut self, active: Option<Caller>, caller: Caller, item: T) -> Result<(), T> {
        let busy = active == Some(caller) || self.waiting.iter().any(|(c, _)| *c == caller);
        if busy || self.waiting.len() >= MAX_WAITING {
            return Err(item);
        }
        self.waiting.push_back((caller, item));
        Ok(())
    }

    /// The oldest waiting request.
    pub fn pop(&mut self) -> Option<(Caller, T)> {
        self.waiting.pop_front()
    }

    /// Whether nothing waits.
    pub fn is_empty(&self) -> bool {
        self.waiting.is_empty()
    }

    /// How many requests wait.
    pub fn len(&self) -> usize {
        self.waiting.len()
    }
}
