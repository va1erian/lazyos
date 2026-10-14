//! CPU use by process group, from the scheduler's task list (native syscall 13, open to every
//! task, `kernel/src/task/introspect.rs`): the UI process's heartbeat prints it (`INFO:CPU`, issue
//! #861) because `/proc/<pid>/stat` does not exist here.
//!
//! A row is a thread. The list does not say which process a thread belongs to, but the launcher makes
//! each engine process its own group (`setpgid(0, 0)`), so the threads of the WebProcess or the
//! NetworkProcess are the rows whose group is the process's id. The UI process's own are the rows
//! of the group it runs in; if other programs share that group, its number includes them.

/// Words in the snapshot's header (version, row count) and in each row.
const HEADER_WORDS: usize = 2;
const ROW_WORDS: usize = 11;
/// Rows the kernel always writes (`MAX_TASKS`).
const ROWS: usize = 1024;
/// Bytes of one snapshot.
pub const SNAPSHOT_SIZE: usize = (HEADER_WORDS + ROWS * ROW_WORDS) * 8;
/// Scheduler ticks per second (the kernel's 100 Hz PIT).
pub const TICKS_PER_SECOND: u64 = 100;

/// One live task of the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Row {
    /// The task slot, which is the id `fork` and `getpid` give a process's first thread.
    pub pid: u64,
    pub pgid: u64,
    /// Scheduler ticks the task has used.
    pub cpu_ticks: u64,
}

/// The live rows of a snapshot block; empty for one of the wrong size.
pub fn parse(bytes: &[u8]) -> Vec<Row> {
    if bytes.len() != SNAPSHOT_SIZE {
        return Vec::new();
    }
    let word = |index: usize| -> u64 {
        let at = index * 8;
        u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap_or([0; 8]))
    };
    let count = usize::try_from(word(1)).unwrap_or(0).min(ROWS);
    (0..count)
        .filter_map(|slot| {
            let base = HEADER_WORDS + slot * ROW_WORDS;
            (word(base) != 0).then(|| Row {
                pid: word(base + 1),
                pgid: word(base + 3),
                cpu_ticks: word(base + 8),
            })
        })
        .collect()
}

/// Ticks used by every thread in group `pgid`.
pub fn group_ticks(rows: &[Row], pgid: u64) -> u64 {
    rows.iter()
        .filter(|row| row.pgid == pgid)
        .map(|row| row.cpu_ticks)
        .sum()
}

/// The group the thread group with first thread `pid` runs in.
pub fn group_of(rows: &[Row], pid: u64) -> Option<u64> {
    rows.iter().find(|row| row.pid == pid).map(|row| row.pgid)
}

/// The share of one CPU that `ticks` scheduler ticks are over `elapsed`, in percent.
pub fn percent(ticks: u64, elapsed: std::time::Duration) -> u64 {
    let possible = elapsed.as_millis() as u64 * TICKS_PER_SECOND / 1000;
    if possible == 0 {
        return 0;
    }
    ticks * 100 / possible
}

/// The scheduler's task list now.
#[cfg(all(target_os = "linux", target_env = "musl"))]
pub fn snapshot() -> Option<Vec<Row>> {
    let mut bytes = vec![0u8; SNAPSHOT_SIZE];
    lazyos_sys::stats::tasks(&mut bytes).ok()?;
    Some(parse(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(rows: &[(u64, u64, u64, u64)]) -> Vec<u8> {
        let mut words = vec![0u64; HEADER_WORDS + ROWS * ROW_WORDS];
        words[0] = 4;
        words[1] = ROWS as u64;
        for &(slot, pgid, ticks, live) in rows {
            let base = HEADER_WORDS + slot as usize * ROW_WORDS;
            words[base] = live;
            words[base + 1] = slot;
            words[base + 3] = pgid;
            words[base + 8] = ticks;
        }
        words.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    #[test]
    fn threads_of_a_group_are_summed_and_free_slots_ignored() {
        let rows = parse(&block(&[
            (3, 3, 10, 1),
            (4, 3, 25, 1),
            (5, 3, 99, 0),
            (9, 9, 7, 1),
        ]));
        assert_eq!(rows.len(), 3);
        assert_eq!(group_ticks(&rows, 3), 35);
        assert_eq!(group_ticks(&rows, 9), 7);
        assert_eq!(group_of(&rows, 4), Some(3));
        assert_eq!(group_of(&rows, 77), None);
    }

    #[test]
    fn a_share_is_ticks_over_the_ticks_a_cpu_could_give() {
        let five = std::time::Duration::from_secs(5);
        assert_eq!(percent(50, five), 10);
        assert_eq!(percent(500, five), 100);
        assert_eq!(percent(5, std::time::Duration::from_millis(1)), 0);
    }

    #[test]
    fn a_block_of_the_wrong_size_is_empty() {
        assert!(parse(&[0u8; 16]).is_empty());
    }
}
