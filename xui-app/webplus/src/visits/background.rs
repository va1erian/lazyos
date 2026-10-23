//! Writing the history file on [`crate::disk`]'s thread, off the window's (`disk.rs` says why).
//! A visit's append and the occasional rewrite that keeps the file short are queued and their
//! errors reported there; a change the person asked for (clearing the history, removing a
//! suggestion) waits for its answer, so the window still says when it was not written.

use std::io;
use std::time::Duration;

use super::Visits;

/// How long a change the person asked for waits for the disk thread.
const CHECKED_WAIT: Duration = Duration::from_secs(3);

impl Visits {
    /// Writes the file on the disk thread from now on.
    pub fn in_background(mut self) -> Visits {
        self.background = true;
        self
    }

    /// Runs `write` now, or on the disk thread in the order asked when the file is written in the
    /// background: queued, or waited for when `checked`.
    pub(super) fn write(
        &self,
        what: &'static str,
        checked: bool,
        write: impl FnOnce() -> io::Result<()> + Send + 'static,
    ) -> io::Result<()> {
        if !self.background {
            return write();
        }
        let disk = crate::disk::global();
        if checked {
            return disk.call(CHECKED_WAIT, write);
        }
        disk.run(move || {
            if let Err(e) = write() {
                eprintln!("lazywebplus: history not {what}: {e}");
            }
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::MAX_VISITS;
    use super::*;

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lazywebplus-visits-bg-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("history.tsv")
    }

    #[test]
    fn a_history_written_in_the_background_is_on_disk_after_a_flush() {
        let file = temp("flush");
        let mut visits = Visits::open(file.clone()).in_background();
        for n in 0..2 * MAX_VISITS {
            visits
                .record(&format!("http://{n}.test/"), "", n as u64)
                .unwrap();
        }
        assert!(crate::disk::global().flush(Duration::from_secs(10)));
        assert_eq!(
            Visits::open(file).entries(),
            visits.entries(),
            "appends, then the rewrite"
        );
    }

    #[test]
    fn a_cleared_history_is_gone_from_disk_when_clear_returns_and_a_failure_is_told() {
        let file = temp("clear");
        let mut visits = Visits::open(file.clone()).in_background();
        visits.record("http://a.test/", "A", 1).unwrap();
        visits.record("http://b.test/", "B", 2).unwrap();
        visits.clear().unwrap();
        assert!(Visits::open(file.clone()).entries().is_empty());
        // The folder becomes a file: the rewrite cannot write, and the caller hears it.
        let dir = file.parent().unwrap();
        std::fs::remove_dir_all(dir).unwrap();
        std::fs::write(dir, b"not a folder").unwrap();
        visits.record("http://c.test/", "C", 3).unwrap();
        assert!(visits.remove_url("http://c.test/").is_err());
        let _ = std::fs::remove_file(dir);
    }
}
