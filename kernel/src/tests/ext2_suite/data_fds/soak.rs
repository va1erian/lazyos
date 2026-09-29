//! Sustained load: many open/write/close/unlink cycles, descriptor-table
//! churn, and filling the volume. Each ends by proving that no descriptor,
//! registered open file, block or inode was leaked.

use super::*;

/// Cycles of the create/write/close/unlink loop.
const CYCLES: usize = 300;

/// File `i`'s contents: a few blocks now and then, so the indirect block is
/// allocated and freed as well.
fn contents(i: usize) -> Vec<u8> {
    pattern_bytes(i as u32, 200 + (i % 7) * 1700)
}

/// Create, write, verify, close and unlink, with every third file unlinked
/// while still open and every fifth truncated and synced on the way.
pub fn create_write_unlink() -> Result<(), String> {
    let data = Data::new(0)?;
    let baseline = free_space()?;
    for i in 0..CYCLES {
        let path = format!("/data/f{}", i % 5);
        let body = contents(i);
        let fd = open(&path, O_CREAT | O_TRUNC | O_RDWR);
        check!(fd < 16, "cycle {i}: create returned {fd:#x}");
        check!(
            write(fd, &body) == body.len() as u64,
            "cycle {i}: short write"
        );
        if i % 5 == 0 {
            let cut = body.len() as u64 / 2;
            check!(
                syscall(SYS_FTRUNCATE, fd, cut, 0, 0) == 0,
                "cycle {i}: ftruncate"
            );
            check!(syscall(SYS_FSYNC, fd, 0, 0, 0) == 0, "cycle {i}: fsync");
            check!(fstat_size(fd)? == cut, "cycle {i}: size after ftruncate");
        }
        if i % 3 == 0 {
            check!(
                path_call(SYS_UNLINK, &path, 0) == 0,
                "cycle {i}: unlink while open"
            );
            check!(
                pread(fd, 8, 0) == Ok(body[..8].to_vec()),
                "cycle {i}: orphan unreadable"
            );
            check!(close(fd) == 0, "cycle {i}: close");
        } else {
            check!(close(fd) == 0, "cycle {i}: close");
            let want = if i % 5 == 0 {
                &body[..body.len() / 2]
            } else {
                &body[..]
            };
            check!(slurp(&path)? == want, "cycle {i}: read back differs");
            check!(path_call(SYS_UNLINK, &path, 0) == 0, "cycle {i}: unlink");
        }
        check!(
            parked(&data_names()?) == 0,
            "cycle {i}: a hidden entry was left"
        );
    }
    check!(
        free_space()? == baseline,
        "blocks or inodes leaked over {CYCLES} cycles"
    );
    data.check_clean()
}

fn parked(names: &[String]) -> usize {
    names
        .iter()
        .filter(|name| name.starts_with(".unlinked-"))
        .count()
}

/// Fill the whole descriptor table with `/data` files and close it again, over
/// and over: the table and the open-file registry return to empty every time.
pub fn fd_table_churn() -> Result<(), String> {
    let data = Data::new(0)?;
    let baseline = free_space()?;
    for round in 0..100 {
        let mut fds = Vec::new();
        for slot in 0..(task::FD_COUNT - 3) {
            let fd = open(&format!("/data/c{}", slot % 4), O_CREAT | O_RDWR);
            check!(fd < 16, "round {round}: open #{slot} returned {fd:#x}");
            check!(
                write(fd, &[slot as u8; 33]) == 33,
                "round {round}: write failed"
            );
            fds.push(fd);
        }
        // Table full: the next open fails cleanly and registers nothing.
        let refused = open("/data/c0", O_RDONLY);
        check!(
            refused >= 16,
            "round {round}: opened past the table ({refused:#x})"
        );
        for fd in fds {
            check!(close(fd) == 0, "round {round}: close failed");
        }
        check!(
            nothing_open(data.open_files),
            "round {round}: descriptors or opens leaked"
        );
    }
    for name in 0..4 {
        check!(
            path_call(SYS_UNLINK, &format!("/data/c{name}"), 0) == 0,
            "unlink failed"
        );
    }
    check!(free_space()? == baseline, "blocks or inodes leaked");
    data.check_clean()
}

/// Write until the volume is full: the failure is `ENOSPC` (never a hang or a
/// bogus success), a short write reports what landed, and unlinking returns
/// every block.
pub fn fill_and_free() -> Result<(), String> {
    let data = Data::new(0)?;
    let baseline = free_space()?;
    let chunk = pattern_bytes(3, 4096);
    for round in 0..4 {
        let fd = open("/data/fill", O_CREAT | O_TRUNC | O_WRONLY);
        let mut total = 0u64;
        let outcome = loop {
            let wrote = write(fd, &chunk);
            if wrote > chunk.len() as u64 {
                break wrote;
            }
            total += wrote;
            if wrote < chunk.len() as u64 {
                // Short write at the end of the space: the next one must fail.
                break write(fd, &chunk[..1]);
            }
        };
        check!(
            outcome == errno(ENOSPC),
            "round {round}: full volume answered {outcome:#x}"
        );
        check!(
            fstat_size(fd)? == total,
            "round {round}: size differs from the bytes written"
        );
        check!(
            free_space()?.0 == 0,
            "round {round}: the volume is not actually full"
        );
        close(fd);
        check!(
            path_call(SYS_UNLINK, "/data/fill", 0) == 0,
            "round {round}: unlink failed"
        );
        check!(
            free_space()? == baseline,
            "round {round}: space did not come back"
        );
    }
    data.check_clean()
}
