//! Readiness: `poll` and the `epoll` freshness counters of one pipe end, and
//! the suite's parking hooks.

use super::*;

impl Pipe {
    /// `poll` revents for one end: `POLLIN`/`POLLOUT` when the requested event
    /// can proceed, `POLLHUP` when the read end has no writers left, `POLLERR`
    /// when the write end has no readers left.
    pub fn poll(&self, end: End, events: u16) -> u16 {
        let ring = self.state.lock();
        let mut revents = 0;
        match end {
            End::Read => {
                if events & POLLIN != 0 && ring.has_data() {
                    revents |= POLLIN;
                }
                if self.writers.load(Ordering::Acquire) == 0 {
                    revents |= POLLHUP;
                }
            }
            End::Write => {
                if events & POLLOUT != 0
                    && self.space_for(&ring, 1)
                    && self.readers.load(Ordering::Acquire) > 0
                {
                    revents |= POLLOUT;
                }
                if self.readers.load(Ordering::Acquire) == 0 {
                    revents |= POLLERR;
                }
            }
        }
        revents
    }

    /// [`poll`](Pipe::poll) plus the freshness counter for edge-triggered
    /// `epoll` interests (read end: writes/EOF; write end: reads/`-EPIPE`).
    pub fn poll_gen(&self, end: End, events: u16) -> (u16, u64) {
        let revents = self.poll(end, events);
        let gen = match end {
            End::Read => self.read_events(),
            End::Write => self.write_events(),
        };
        (revents, gen)
    }

    /// Park the current task on the reader queue (test hook). Production reads
    /// go through [`Pipe::read`], which uses the same queue.
    #[cfg(lazyos_tests)]
    pub fn park_reader(&self, task: usize) {
        self.read_wq.park(task, None);
    }

    /// Park the current task on the writer queue (test hook).
    #[cfg(lazyos_tests)]
    pub fn park_writer(&self, task: usize) {
        self.write_wq.park(task, None);
    }
}
