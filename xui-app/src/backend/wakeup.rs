//! The cross-thread wake-up: how a worker thread tells a parked UI loop that
//! it has something for it.
//!
//! xui hands worker threads a `Proxy` whose sends end in the backend's
//! [`waker`](xui_core::backend::Backend::waker). The loop parks in one
//! `wait_any` (`park_client`), and the only thing a thread can ring from
//! outside that wait is a Linux descriptor: the wait names exactly one
//! (`WAIT_FD`), which the Terminal already spends on its pty. So the park
//! names an `epoll` set instead, holding two descriptors:
//!
//! * the **doorbell**, an `eventfd` every waker writes to (descriptor tables
//!   are shared by `CLONE_FILES` threads, and a `write` notifies every
//!   `poll`/`wait` waiter), and
//! * the **watched** descriptor of [`LazyOSBackend::watch_fd`], when set.
//!
//! Level-triggered readiness is what keeps this free of lost wake-ups: a
//! ring that lands while the loop is busy leaves the counter non-zero, so the
//! next park returns at once. The counter is cleared by [`Wakeup::collect`],
//! which runs only after a park, so a ring is never consumed unseen.
//!
//! [`LazyOSBackend::watch_fd`]: super::LazyOSBackend::watch_fd

use std::cell::Cell;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::Arc;

use xui_core::backend::Waker;

/// `epoll_event.u64` of the doorbell.
const DOORBELL_KEY: u64 = 0;
/// `epoll_event.u64` of the watched descriptor.
const WATCHED_KEY: u64 = 1;

/// The eventfd workers ring; shared with every waker handed out, so it
/// outlives the backend while a worker still holds a `Proxy`.
struct Doorbell(OwnedFd);

impl Doorbell {
    fn ring(&self) {
        let one = 1u64.to_ne_bytes();
        // SAFETY: the descriptor is owned by `self` and open, and `one` is
        // eight bytes, which is what an eventfd write takes. A full counter
        // (`EAGAIN`) means a wake-up is already due, so a failed write
        // loses nothing.
        let _ = unsafe { libc::write(self.0.as_raw_fd(), one.as_ptr().cast(), one.len()) };
    }

    /// Reset the counter to zero (a non-blocking read returns and clears it).
    fn clear(&self) {
        let mut sink = [0u8; 8];
        // SAFETY: as `ring`; `sink` is eight bytes, the size of a counter read.
        let _ = unsafe { libc::read(self.0.as_raw_fd(), sink.as_mut_ptr().cast(), sink.len()) };
    }
}

/// The set the UI loop parks on, and the doorbell inside it.
pub(super) struct Wakeup {
    doorbell: Arc<Doorbell>,
    epoll: OwnedFd,
    /// The descriptor registered beside the doorbell.
    watched: Cell<Option<RawFd>>,
}

/// An owned descriptor from a raw `libc` return, or the OS error.
fn owned(fd: libc::c_int) -> io::Result<OwnedFd> {
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` is a fresh descriptor this call returned and nothing else owns.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn event(key: u64) -> libc::epoll_event {
    libc::epoll_event {
        events: libc::EPOLLIN as u32,
        u64: key,
    }
}

impl Wakeup {
    pub(super) fn new() -> io::Result<Wakeup> {
        // SAFETY: plain syscalls with constant arguments.
        let doorbell = owned(unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) })?;
        let epoll = owned(unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) })?;
        let mut added = event(DOORBELL_KEY);
        // SAFETY: both descriptors are open and `added` outlives the call.
        let status = unsafe {
            libc::epoll_ctl(
                epoll.as_raw_fd(),
                libc::EPOLL_CTL_ADD,
                doorbell.as_raw_fd(),
                &mut added,
            )
        };
        if status < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Wakeup {
            doorbell: Arc::new(Doorbell(doorbell)),
            epoll,
            watched: Cell::new(None),
        })
    }

    /// A waker any thread may call: rings the doorbell.
    pub(super) fn waker(&self) -> Waker {
        let doorbell = Arc::clone(&self.doorbell);
        Box::new(move || doorbell.ring())
    }

    /// The descriptor to park on with `WAIT_FD`.
    pub(super) fn epoll_fd(&self) -> RawFd {
        self.epoll.as_raw_fd()
    }

    /// Forget the registered descriptor, so the next [`Wakeup::watch`] adds
    /// whatever it is given again: the app closed its file and a new one may
    /// have taken the same number, which the early return of an unchanged
    /// number would otherwise leave out of the set.
    pub(super) fn forget(&self) {
        if let Some(old) = self.watched.take() {
            // A closed descriptor already left the set; either way it is gone.
            // SAFETY: `epoll` is open; the event argument is ignored by DEL.
            let _ = unsafe {
                libc::epoll_ctl(
                    self.epoll.as_raw_fd(),
                    libc::EPOLL_CTL_DEL,
                    old,
                    std::ptr::null_mut(),
                )
            };
        }
    }

    /// Make the set hold `fd` beside the doorbell (`None`: only the
    /// doorbell). A descriptor `epoll` refuses is reported, and left out.
    pub(super) fn watch(&self, fd: Option<RawFd>) -> io::Result<()> {
        if self.watched.get() == fd {
            return Ok(());
        }
        if let Some(old) = self.watched.take() {
            // A closed descriptor already left the set; either way it is gone.
            // SAFETY: `epoll` is open; the event argument is ignored by DEL.
            let _ = unsafe {
                libc::epoll_ctl(
                    self.epoll.as_raw_fd(),
                    libc::EPOLL_CTL_DEL,
                    old,
                    std::ptr::null_mut(),
                )
            };
        }
        let Some(fd) = fd else {
            return Ok(());
        };
        let mut added = event(WATCHED_KEY);
        // SAFETY: `epoll` is open and `added` outlives the call.
        let status =
            unsafe { libc::epoll_ctl(self.epoll.as_raw_fd(), libc::EPOLL_CTL_ADD, fd, &mut added) };
        if status < 0 {
            return Err(io::Error::last_os_error());
        }
        self.watched.set(Some(fd));
        Ok(())
    }

    /// After a park that reported the set ready: clear the doorbell and say
    /// whether the watched descriptor is readable.
    pub(super) fn collect(&self) -> bool {
        let mut events = [event(0); 2];
        // SAFETY: `events` holds two entries, the maximum asked for; a zero
        // timeout never blocks.
        let count = unsafe { libc::epoll_wait(self.epoll.as_raw_fd(), events.as_mut_ptr(), 2, 0) };
        let mut watched_ready = false;
        for entry in events.iter().take(count.max(0) as usize) {
            // Copy out of the (packed on some targets) struct before use.
            let key = entry.u64;
            match key {
                DOORBELL_KEY => self.doorbell.clear(),
                WATCHED_KEY => watched_ready = true,
                _ => {}
            }
        }
        watched_ready
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whether `fd` is readable now (a zero-timeout `poll`).
    fn readable(fd: RawFd) -> bool {
        let mut item = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid `pollfd` that outlives the call.
        unsafe { libc::poll(&mut item, 1, 0) > 0 }
    }

    #[test]
    fn a_ring_from_another_thread_makes_the_set_readable_until_collected() {
        let wakeup = Wakeup::new().unwrap();
        assert!(!readable(wakeup.epoll_fd()));
        let wake = wakeup.waker();
        std::thread::spawn(move || wake()).join().unwrap();
        assert!(readable(wakeup.epoll_fd()), "the ring was lost");
        // Level-triggered: still readable until the loop looks.
        assert!(readable(wakeup.epoll_fd()));
        assert!(!wakeup.collect(), "no descriptor is watched");
        assert!(
            !readable(wakeup.epoll_fd()),
            "collect left the bell ringing"
        );
    }

    #[test]
    fn a_burst_of_rings_is_one_wake_and_a_ring_after_collect_is_not_lost() {
        let wakeup = Wakeup::new().unwrap();
        let wake = wakeup.waker();
        for _ in 0..1000 {
            wake();
        }
        wakeup.collect();
        assert!(!readable(wakeup.epoll_fd()));
        wake();
        assert!(readable(wakeup.epoll_fd()));
    }

    #[test]
    fn a_reused_descriptor_number_is_registered_again() {
        let wakeup = Wakeup::new().unwrap();
        let pipe = || {
            let mut ends = [0 as libc::c_int; 2];
            // SAFETY: `ends` holds the two descriptors `pipe` fills in.
            assert_eq!(unsafe { libc::pipe(ends.as_mut_ptr()) }, 0);
            (ends[0], ends[1])
        };
        let (read, write) = pipe();
        wakeup.watch(Some(read)).unwrap();
        // SAFETY: both descriptors are open and owned here.
        unsafe {
            libc::close(read);
            libc::close(write);
        }
        // The lowest free numbers come back: a new file with the old number.
        let (read, write) = pipe();
        // `watch_fd` is what the app calls for the new file.
        wakeup.forget();
        wakeup.watch(Some(read)).unwrap();
        // SAFETY: `write` is the pipe's open write end; one byte is written.
        assert_eq!(unsafe { libc::write(write, [1u8].as_ptr().cast(), 1) }, 1);
        assert!(
            readable(wakeup.epoll_fd()),
            "the new file is not in the set"
        );
        assert!(wakeup.collect());
        // SAFETY: both descriptors are open and owned here.
        unsafe {
            libc::close(read);
            libc::close(write);
        }
    }

    #[test]
    fn the_watched_descriptor_is_reported_beside_the_doorbell() {
        let wakeup = Wakeup::new().unwrap();
        let mut ends = [0 as libc::c_int; 2];
        // SAFETY: `ends` holds the two descriptors `pipe` fills in.
        assert_eq!(unsafe { libc::pipe(ends.as_mut_ptr()) }, 0);
        let (read, write) = (ends[0], ends[1]);
        wakeup.watch(Some(read)).unwrap();
        assert!(!wakeup.collect());
        // SAFETY: `write` is the pipe's open write end; one byte is written.
        assert_eq!(unsafe { libc::write(write, [1u8].as_ptr().cast(), 1) }, 1);
        assert!(readable(wakeup.epoll_fd()));
        assert!(wakeup.collect(), "the pipe was readable");
        // Unwatching drops it from the set; the pipe's byte no longer counts.
        wakeup.watch(None).unwrap();
        assert!(!readable(wakeup.epoll_fd()));
        // SAFETY: both descriptors are open and owned here.
        unsafe {
            libc::close(read);
            libc::close(write);
        }
    }
}
