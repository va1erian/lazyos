//! The `running` marker: it holds the id of the process that made it, so a second LazyWeb+ window
//! (another process on the same profile) is not taken for a crash, and a quitting window never
//! removes the marker of one that still runs.
//!
//! The decision is pure ([`decide`]); only [`process_alive`] looks at the system.

use std::path::Path;

/// What a start finds, given the marker and who made it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Start {
    /// No marker: the last run ended on purpose. This process takes the marker.
    Clean,
    /// A marker nobody holds (its process is gone, it names none, or it is this process's own id
    /// reused): the last run crashed. This process takes the marker.
    Crashed,
    /// A marker held by another running process: no restore offer, and the marker is left alone.
    Shared,
}

/// The marker's text for process `pid`.
pub fn text(pid: u32) -> String {
    format!("{pid}\n")
}

/// The process id a marker names. A marker from an older version, or a damaged one, names none.
pub fn parse_pid(text: &str) -> Option<u32> {
    let pid: u32 = text.lines().next()?.trim().parse().ok()?;
    (pid != 0).then_some(pid)
}

/// What a start does with the marker `existing` (`None`: there is none), as process `own`;
/// `alive` says whether another process id is running.
pub fn decide(existing: Option<&str>, own: u32, alive: impl Fn(u32) -> bool) -> Start {
    let Some(existing) = existing else {
        return Start::Clean;
    };
    match parse_pid(existing) {
        Some(pid) if pid != own && alive(pid) => Start::Shared,
        _ => Start::Crashed,
    }
}

/// Whether the marker `existing` is this process's, so that it may remove it.
pub fn is_own(existing: &str, own: u32) -> bool {
    parse_pid(existing) == Some(own)
}

/// Whether process `pid` runs. LazyOS and Linux list processes in `/proc`; where that is not known
/// to exist a marker is taken to be stale, which at worst offers a restore that was not needed.
pub fn process_alive(pid: u32) -> bool {
    cfg!(target_os = "linux") && Path::new(&format!("/proc/{pid}")).exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_marker_names_its_process_and_old_ones_name_none() {
        assert_eq!(parse_pid(&text(4242)), Some(4242));
        assert_eq!(parse_pid("LazyWeb+ is running.\n"), None);
        assert_eq!(parse_pid("x"), None);
        assert_eq!(parse_pid("0\n"), None);
        assert_eq!(parse_pid(""), None);
        assert_eq!(parse_pid("-3"), None);
    }

    #[test]
    fn a_start_tells_clean_crashed_and_shared_apart() {
        let alive = |pid: u32| pid == 77;
        assert_eq!(decide(None, 5, alive), Start::Clean);
        // Another process that still runs holds it.
        assert_eq!(decide(Some("77\n"), 5, alive), Start::Shared);
        // One that is gone, a marker with no id, and our own id reused are a crash.
        assert_eq!(decide(Some("78\n"), 5, alive), Start::Crashed);
        assert_eq!(
            decide(Some("LazyWeb+ is running.\n"), 5, alive),
            Start::Crashed
        );
        assert_eq!(decide(Some("77\n"), 77, alive), Start::Crashed);
    }

    #[test]
    fn only_the_holder_removes_the_marker() {
        assert!(is_own("5\n", 5));
        assert!(!is_own("6\n", 5));
        assert!(!is_own("junk", 5));
    }
}
