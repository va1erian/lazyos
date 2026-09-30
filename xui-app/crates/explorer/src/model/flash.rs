#![forbid(unsafe_code)]

//! The open-folder flash: which folder names show the open icon, and until
//! when.
//!
//! Opening a folder flags its name for [`FLASH_DURATION`]; the icon lookup
//! reports it as flashing while its deadline is in the future. Several folders
//! can flash at once, each with its own deadline, and re-opening one restarts
//! its deadline. The clock is injected, so expiry is testable without sleeping.
//! Nothing here names a widget or a backend.

use std::cell::RefCell;
use std::ffi::{OsStr, OsString};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// How long an opened folder keeps its open icon.
pub const FLASH_DURATION: Duration = Duration::from_millis(2000);

/// A source of `now`. The default is the system clock; a test injects one it
/// can advance.
pub type Clock = Rc<dyn Fn() -> Instant>;

/// The set of names currently showing their open icon, each with the instant it
/// reverts.
pub struct Flash {
    clock: Clock,
    entries: RefCell<Vec<(OsString, Instant)>>,
}

impl Flash {
    /// A flash on the system clock.
    pub fn new() -> Flash {
        Flash::with_clock(Rc::new(Instant::now))
    }

    /// A flash on `clock`.
    pub fn with_clock(clock: Clock) -> Flash {
        Flash {
            clock,
            entries: RefCell::new(Vec::new()),
        }
    }

    /// Starts (or restarts) the flash for `name`, returning whether the list
    /// was empty before, so the caller knows to start its tick timer.
    pub fn flash(&self, name: &OsStr) -> bool {
        let deadline = (self.clock)() + FLASH_DURATION;
        let mut entries = self.entries.borrow_mut();
        let was_empty = entries.is_empty();
        match entries
            .iter_mut()
            .find(|(existing, _)| existing.as_os_str() == name)
        {
            Some((_, existing_deadline)) => *existing_deadline = deadline,
            None => entries.push((name.to_os_string(), deadline)),
        }
        was_empty
    }

    /// Whether `name` is showing its open icon.
    pub fn is_flashing(&self, name: &OsStr) -> bool {
        let now = (self.clock)();
        self.entries
            .borrow()
            .iter()
            .any(|(existing, deadline)| existing.as_os_str() == name && now < *deadline)
    }

    /// Drops expired entries and every entry failing `keep`. Returns how many
    /// are still flashing.
    pub fn retain(&self, mut keep: impl FnMut(&OsStr) -> bool) -> usize {
        let now = (self.clock)();
        let mut entries = self.entries.borrow_mut();
        entries.retain(|(name, deadline)| now < *deadline && keep(name));
        entries.len()
    }

    /// Drops expired entries. Returns how many are still flashing.
    pub fn prune(&self) -> usize {
        self.retain(|_| true)
    }

    /// Forgets every flashing name.
    pub fn clear(&self) {
        self.entries.borrow_mut().clear();
    }

    /// How many names are recorded, including any whose deadline has passed but
    /// which have not been pruned yet.
    pub fn len(&self) -> usize {
        self.entries.borrow().len()
    }

    /// Whether no name is recorded.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for Flash {
    fn default() -> Flash {
        Flash::new()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    /// A clock a test can advance, shared with the `Flash`.
    struct TestClock {
        now: Rc<Cell<Instant>>,
    }

    impl TestClock {
        fn new() -> TestClock {
            TestClock {
                now: Rc::new(Cell::new(Instant::now())),
            }
        }

        fn flash(&self) -> Flash {
            let now = Rc::clone(&self.now);
            Flash::with_clock(Rc::new(move || now.get()))
        }

        fn advance(&self, millis: u64) {
            self.now.set(self.now.get() + Duration::from_millis(millis));
        }
    }

    #[test]
    fn flashing_flags_a_name_until_its_deadline() {
        let clock = TestClock::new();
        let flash = clock.flash();
        assert!(!flash.is_flashing(OsStr::new("docs")));
        flash.flash(OsStr::new("docs"));
        assert!(flash.is_flashing(OsStr::new("docs")));
        clock.advance(1999);
        assert!(flash.is_flashing(OsStr::new("docs")));
    }

    #[test]
    fn a_name_expires_at_exactly_the_duration() {
        let clock = TestClock::new();
        let flash = clock.flash();
        flash.flash(OsStr::new("docs"));
        clock.advance(2000);
        assert!(
            !flash.is_flashing(OsStr::new("docs")),
            "2000 ms is the first instant no longer flashing"
        );
        assert_eq!(flash.prune(), 0, "the tick removes it");
        assert!(flash.is_empty());
    }

    #[test]
    fn re_opening_a_folder_restarts_its_deadline() {
        let clock = TestClock::new();
        let flash = clock.flash();
        flash.flash(OsStr::new("docs"));
        clock.advance(1500);
        let was_empty = flash.flash(OsStr::new("docs"));
        assert!(!was_empty, "the list was not empty");
        clock.advance(1500);
        assert!(
            flash.is_flashing(OsStr::new("docs")),
            "1000 ms since the restart, not yet expired"
        );
        assert_eq!(flash.len(), 1, "re-opening did not add a second entry");
    }

    #[test]
    fn the_first_flash_reports_an_empty_list_and_the_next_does_not() {
        let clock = TestClock::new();
        let flash = clock.flash();
        assert!(flash.flash(OsStr::new("a")));
        assert!(!flash.flash(OsStr::new("b")));
        assert_eq!(flash.len(), 2, "several folders flash at once");
    }

    #[test]
    fn a_tick_prunes_each_entry_on_its_own_deadline() {
        let clock = TestClock::new();
        let flash = clock.flash();
        flash.flash(OsStr::new("a"));
        clock.advance(1000);
        flash.flash(OsStr::new("b"));
        clock.advance(1000);
        assert_eq!(flash.prune(), 1, "only b is left");
        assert!(!flash.is_flashing(OsStr::new("a")));
        assert!(flash.is_flashing(OsStr::new("b")));
    }

    #[test]
    fn retain_drops_names_a_refresh_no_longer_knows() {
        let clock = TestClock::new();
        let flash = clock.flash();
        flash.flash(OsStr::new("kept"));
        flash.flash(OsStr::new("gone"));
        let remaining = flash.retain(|name| name == OsStr::new("kept"));
        assert_eq!(remaining, 1);
        assert!(flash.is_flashing(OsStr::new("kept")));
        assert!(!flash.is_flashing(OsStr::new("gone")));
    }

    #[test]
    fn clear_forgets_every_name() {
        let clock = TestClock::new();
        let flash = clock.flash();
        flash.flash(OsStr::new("a"));
        flash.flash(OsStr::new("b"));
        flash.clear();
        assert!(flash.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_non_utf8_name_can_flash() {
        use std::os::unix::ffi::OsStringExt;

        let clock = TestClock::new();
        let flash = clock.flash();
        let name = OsString::from_vec(vec![0xff, 0x61]);
        flash.flash(&name);
        assert!(flash.is_flashing(&name));
        assert_eq!(flash.len(), 1);
    }
}
