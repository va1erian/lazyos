//! The `running` marker: it holds the id of the process that made it, so a quitting window never
//! removes the marker of a later one.
//!
//! A marker found at start is a crash. Whether its process still runs cannot be told on LazyOS
//! (there is no `/proc/<pid>`, and process ids are reused soon after a boot), so a second LazyWeb
//! process on the same profile also offers a restore, which the person can dismiss. One process
//! owning every window (docs/lazyweb-plan.md, B1) removes that case.

/// The marker's text for process `pid`.
pub fn text(pid: u32) -> String {
    format!("{pid}
")
}

/// The process id a marker names. A marker from an older version, or a damaged one, names none.
pub fn parse_pid(text: &str) -> Option<u32> {
    let pid: u32 = text.lines().next()?.trim().parse().ok()?;
    (pid != 0).then_some(pid)
}

/// Whether the marker `existing` is this process's, so that it may remove it.
pub fn is_own(existing: &str, own: u32) -> bool {
    parse_pid(existing) == Some(own)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_marker_names_its_process_and_old_ones_name_none() {
        assert_eq!(parse_pid(&text(4242)), Some(4242));
        assert_eq!(parse_pid("LazyWeb is running.
"), None);
        assert_eq!(parse_pid("x"), None);
        assert_eq!(parse_pid("0
"), None);
        assert_eq!(parse_pid(""), None);
        assert_eq!(parse_pid("-3"), None);
    }

    #[test]
    fn only_the_holder_removes_the_marker() {
        assert!(is_own("5
", 5));
        assert!(!is_own("6
", 5));
        assert!(!is_own("junk", 5));
    }
}
