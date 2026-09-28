//! `epoll(7)` objects (Linux ABI round 2).
//!
//! An epoll fd owns a list of interests: `(descriptor number, open file
//! handle, requested events, user data)`. `epoll_wait` scans the interest list
//! against the handles' own `poll` state; because the handle is cloned in (as
//! Linux holds a file reference), a scan never needs the task's descriptor
//! table and cannot race a `close`.
//!
//! Level-triggered interests report whenever the handle is ready. With
//! `EPOLLET`, each handle's [`poll_gen`](crate::ipc::pipe::Pipe::poll_gen)
//! freshness counter is remembered so a fresh edge (new data, new space, or a
//! close) is reported even while the handle stays ready; a repeated wait with
//! no change reports nothing. Data arrival on a still-ready pipe therefore
//! wakes an edge waiter, unlike a bare "was ready last time" snapshot.
//!
//! `close` of a user descriptor removes its interest (Linux drops the item
//! when the file is released); the cloned handle keeps the pipe/socket alive
//! until then. Interests are keyed by descriptor number, matching the task's
//! descriptor table.

use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;

use crate::task::Fd;

/// `struct epoll_event` bits (Linux values).
pub const EPOLLIN: u32 = 0x0000_0001;
pub const EPOLLOUT: u32 = 0x0000_0004;
pub const EPOLLERR: u32 = 0x0000_0008;
pub const EPOLLHUP: u32 = 0x0000_0010;
pub const EPOLLRDHUP: u32 = 0x0000_2000;
pub const EPOLLET: u32 = 0x8000_0000;

const REPORT_ALWAYS: u32 = EPOLLERR | EPOLLHUP;

/// Deepest chain of epoll instances that watch other epoll instances (Linux's
/// `EPOLL_MAX_NESTS`). Readiness of a nested instance is computed by polling
/// it recursively, so an unbounded (or cyclic) chain would overflow the kernel
/// stack.
pub const MAX_NEST: usize = 5;

/// Why an epoll descriptor cannot be added to another instance.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NestError {
    /// The instance would (transitively) watch itself: `-ELOOP`, or `-EINVAL`
    /// for the direct self-registration.
    Loop,
    /// The chain of watching instances would exceed [`MAX_NEST`]: `-EINVAL`.
    TooDeep,
}

/// One registered descriptor.
#[derive(Clone)]
struct Interest {
    fd: usize,
    target: Fd,
    events: u32,
    data: u64,
    /// Revents at the end of the last `epoll_wait` (edge bookkeeping).
    last_revents: u16,
    /// Handle freshness at the end of the last `epoll_wait`.
    last_gen: u64,
}

/// An epoll instance behind `epoll_create1`.
pub struct Epoll {
    interests: Mutex<Vec<Interest>>,
    nonblock: AtomicBool,
    /// Other epoll instances that currently have `self` registered as one of
    /// their interests (`Weak`, so being watched cannot keep a watcher alive,
    /// and a watcher's own `Epoll` can still be dropped once its last fd
    /// closes). [`check_nest`] walks this upward to bound the *total* chain
    /// length; without it, only the chain *below* a candidate was bounded, so
    /// building a chain bottom-up (`add(E1, E2)`, then `add(E2, E3)`, ...)
    /// never re-checked what was already stacked on top and grew without
    /// limit.
    watched_by: Mutex<Vec<Weak<Epoll>>>,
}

impl Epoll {
    /// Create an empty instance.
    pub fn new() -> Arc<Epoll> {
        Arc::new(Epoll {
            interests: Mutex::new(Vec::new()),
            nonblock: AtomicBool::new(false),
            watched_by: Mutex::new(Vec::new()),
        })
    }

    /// `O_NONBLOCK` of the epoll fd.
    pub fn nonblock(&self) -> bool {
        self.nonblock.load(Ordering::Acquire)
    }

    /// Set `O_NONBLOCK` on the epoll fd.
    pub fn set_nonblock(&self, on: bool) {
        self.nonblock.store(on, Ordering::Release);
    }

    /// `EPOLL_CTL_ADD`: register `fd`/`target` in `this`. The caller already
    /// checked the descriptor is open and passed [`check_nest`]; a duplicate
    /// registration is rejected. Takes `this` (rather than a plain `&self`)
    /// because a nested `target` needs a `Weak` back-reference to `this` for
    /// [`depth_above`](Epoll::depth_above) to walk.
    pub fn add(this: &Arc<Epoll>, fd: usize, target: Fd, events: u32, data: u64) -> Result<(), ()> {
        let watched = match &target {
            Fd::Epoll { epoll } => Some(Arc::clone(epoll)),
            _ => None,
        };
        {
            let mut interests = this.interests.lock();
            if interests.iter().any(|interest| interest.fd == fd) {
                return Err(());
            }
            interests.push(Interest {
                fd,
                target,
                events,
                data,
                last_revents: 0,
                last_gen: 0,
            });
        }
        if let Some(watched) = watched {
            watched.watched_by.lock().push(Arc::downgrade(this));
        }
        Ok(())
    }

    /// Instances this one watches directly.
    fn nested(&self) -> Vec<Arc<Epoll>> {
        self.interests
            .lock()
            .iter()
            .filter_map(|interest| match &interest.target {
                Fd::Epoll { epoll } => Some(Arc::clone(epoll)),
                _ => None,
            })
            .collect()
    }

    /// Other epoll instances that currently watch this one directly, pruning
    /// entries whose watcher has since been dropped.
    fn watchers(&self) -> Vec<Arc<Epoll>> {
        let mut watched_by = self.watched_by.lock();
        watched_by.retain(|watcher| watcher.strong_count() > 0);
        watched_by.iter().filter_map(Weak::upgrade).collect()
    }

    /// Whether `needle` is reachable from this instance through nested epoll
    /// registrations, and how many levels lie below it.
    fn reaches(&self, needle: &Epoll, level: usize) -> (bool, usize) {
        if level > MAX_NEST {
            return (false, level);
        }
        let mut deepest = level;
        for child in self.nested() {
            if core::ptr::eq(&*child, needle) {
                return (true, level + 1);
            }
            let (found, depth) = child.reaches(needle, level + 1);
            if found {
                return (true, depth);
            }
            deepest = deepest.max(depth);
        }
        (false, deepest)
    }

    /// How many epoll levels are already stacked *above* this instance: 0 if
    /// nothing watches it, 1 if something watches it and nothing watches that,
    /// and so on. The counterpart of [`reaches`](Epoll::reaches)'s `below`,
    /// walking [`watched_by`](Epoll::watched_by) instead of `interests`.
    fn depth_above(&self, level: usize) -> usize {
        if level > MAX_NEST {
            return level;
        }
        self.watchers()
            .iter()
            .map(|watcher| watcher.depth_above(level + 1))
            .max()
            .unwrap_or(level)
    }

    /// Check that registering `candidate` in `this` cannot loop or nest too
    /// deeply. Non-epoll descriptors are always fine.
    ///
    /// Without this an `epoll_ctl(ADD)` of an epoll onto itself (or two epolls
    /// onto each other) makes every `epoll_wait` recurse without bound and
    /// leaks the cycle (each instance holds an `Arc` to the other). Checking
    /// only the chain *below* `candidate` is not enough either: a chain built
    /// bottom-up (`add(E1, E2)`, then `add(E2, E3)`, ...) always finds an empty
    /// candidate, so it never re-examines what is already stacked on top of
    /// `this` and grows without bound. `depth_above` closes that gap.
    pub fn check_nest(this: &Arc<Epoll>, candidate: &Fd) -> Result<(), NestError> {
        let Fd::Epoll { epoll: inner } = candidate else {
            return Ok(());
        };
        if Arc::ptr_eq(this, inner) {
            return Err(NestError::Loop);
        }
        // Would `inner` (transitively) already watch `this`?
        let (loops, below) = inner.reaches(this, 0);
        if loops {
            return Err(NestError::Loop);
        }
        // The new edge joins whatever already watches `this` (`above`) to
        // `inner`'s own subtree (`below`), plus the edge itself.
        let above = this.depth_above(0);
        if above + below + 1 > MAX_NEST {
            return Err(NestError::TooDeep);
        }
        Ok(())
    }

    /// `EPOLL_CTL_MOD`: replace the events/data of a registered descriptor.
    pub fn modify(&self, fd: usize, events: u32, data: u64) -> Result<(), ()> {
        let mut interests = self.interests.lock();
        let Some(interest) = interests.iter_mut().find(|interest| interest.fd == fd) else {
            return Err(());
        };
        interest.events = events;
        interest.data = data;
        // Re-arm: the next wait reports the new interest set's current state.
        interest.last_revents = 0;
        interest.last_gen = 0;
        Ok(())
    }

    /// `EPOLL_CTL_DEL`: drop a registered descriptor from `this`.
    pub fn delete(this: &Arc<Epoll>, fd: usize) -> Result<(), ()> {
        let removed = {
            let mut interests = this.interests.lock();
            let Some(index) = interests.iter().position(|interest| interest.fd == fd) else {
                return Err(());
            };
            interests.remove(index)
        };
        Self::unwatch(this, &removed.target);
        Ok(())
    }

    /// Drop `fd`'s interest from `this` because the user closed the
    /// descriptor. Called by `task::fd_close`, so a reused descriptor number
    /// cannot inherit a stale registration.
    pub fn drop_fd(this: &Arc<Epoll>, fd: usize) {
        let removed = {
            let mut interests = this.interests.lock();
            let Some(index) = interests.iter().position(|interest| interest.fd == fd) else {
                return;
            };
            interests.remove(index)
        };
        Self::unwatch(this, &removed.target);
    }

    /// Drop `this`'s back-reference from a removed interest's target, if it
    /// named another epoll instance. Without this a removed (and later
    /// re-added elsewhere) epoll interest would leave a stale `watched_by`
    /// entry that could wrongly inflate [`depth_above`](Epoll::depth_above)
    /// forever (the entry is a `Weak`, so it cannot leak the instance itself,
    /// only overcount depth until the watcher is dropped).
    fn unwatch(this: &Arc<Epoll>, target: &Fd) {
        if let Fd::Epoll { epoll: watched } = target {
            watched
                .watched_by
                .lock()
                .retain(|watcher| !watcher.ptr_eq(&Arc::downgrade(this)));
        }
    }

    /// One `epoll_wait` pass: at most `max` `(events, data)` pairs. Ready
    /// interests are found by polling each handle outside the list lock, then
    /// the edge-trigger bookkeeping is written back.
    pub fn ready(&self, max: usize) -> Vec<(u32, u64)> {
        // Clone the list so handle polls (which may take the task table for a
        // terminal) run without the interests lock.
        let snapshot: Vec<Interest> = self.interests.lock().clone();
        // (fd, revents, freshness) after this scan.
        let mut scanned: Vec<(usize, u16, u64)> = Vec::with_capacity(snapshot.len());
        let mut out: Vec<(u32, u64)> = Vec::new();
        for interest in &snapshot {
            let (revents, gen) = interest.target.poll_gen((interest.events & 0xffff) as u16);
            let reportable = (revents as u32) & (interest.events | REPORT_ALWAYS);
            let edge = interest.events & EPOLLET != 0;
            let fresh = gen != interest.last_gen || revents != interest.last_revents;
            let wants_report = reportable != 0 && (!edge || fresh);
            if wants_report && out.len() == max {
                // Over the `maxevents` cap: a pending edge must stay pending,
                // so its bookkeeping is left untouched for the next wait.
                // (Level-triggered interests re-report regardless.)
                if !edge {
                    scanned.push((interest.fd, revents, gen));
                }
                continue;
            }
            scanned.push((interest.fd, revents, gen));
            if wants_report {
                out.push((reportable, interest.data));
            }
        }
        {
            let mut interests = self.interests.lock();
            for (fd, revents, gen) in scanned {
                if let Some(interest) = interests.iter_mut().find(|interest| interest.fd == fd) {
                    interest.last_revents = revents;
                    interest.last_gen = gen;
                }
            }
        }
        out
    }

    /// Whether any interest is ready right now (`poll` on the epoll fd).
    pub fn has_ready(&self) -> bool {
        let snapshot: Vec<Interest> = self.interests.lock().clone();
        snapshot.iter().any(|interest| {
            let (revents, _) = interest.target.poll_gen((interest.events & 0xffff) as u16);
            (revents as u32) & (interest.events | REPORT_ALWAYS) != 0
        })
    }

    /// Number of registered descriptors (tests/diagnostics).
    pub fn len(&self) -> usize {
        self.interests.lock().len()
    }

    /// Whether nothing is registered (tests/diagnostics).
    pub fn is_empty(&self) -> bool {
        self.interests.lock().is_empty()
    }
}
