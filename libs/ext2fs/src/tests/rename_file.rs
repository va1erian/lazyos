//! Crash-safe file rename: a power cut at every write, replacing a
//! destination, rolling back, and a leak soak (issue #407's write-temp-then-
//! rename pattern).

use super::*;
use crate::Ext2Error;

const OLD: &[u8] = b"the old contents of the store, long enough to own a block";
const NEW: &[u8] = b"the NEW contents, written to the temp file before the rename";

fn put(fs: &Ext2, path: &str, data: &[u8]) {
    fs.write_file(path, data, 0o644, 0, 0, 5).unwrap();
}

/// A volume with `/a`, `/b` and a stored file plus a temp file to rename over it.
fn scenario(replace: bool, cross: bool) -> (MemIo, &'static str, &'static str) {
    let (io, fs) = fresh(2 * 1024 * 1024, 1024);
    fs.mkdir("/a", 0o755, crate::Owner::ROOT).unwrap();
    fs.mkdir("/b", 0o755, crate::Owner::ROOT).unwrap();
    let to = if cross { "/b/store" } else { "/a/store" };
    if replace {
        put(&fs, to, OLD);
    }
    put(&fs, "/a/store.tmp", NEW);
    fs.flush().unwrap();
    drop(fs);
    (io, "/a/store.tmp", to)
}

/// The problems fsck may report after a cut inside a rename: only space that
/// the commit left behind (an extra link count on the moved file, a replaced
/// file nobody names), never a missing or dangling name.
fn is_leak_class(problem: &str) -> bool {
    !(problem.contains("dangling") || problem.contains("not marked used"))
}

#[test]
fn a_power_cut_at_every_write_keeps_the_data_reachable() {
    for (replace, cross) in [(true, false), (true, true), (false, false), (false, true)] {
        let (base, from, to) = scenario(replace, cross);
        let mut cut = 0;
        loop {
            let io = MemIo::from_bytes(base.snapshot());
            let fs = open(&io);
            io.fail_writes_after(cut);
            let result = fs.rename(from, to);
            drop(fs);
            io.fail_writes_after(u64::MAX);

            let fs = open(&io);
            let at_dest = fs.read_file(to).ok();
            let at_source = fs.read_file(from).ok();
            let label = std::format!("replace={replace} cross={cross} cut={cut}");
            match (&at_dest, &at_source) {
                // Before the commit: the old state, untouched.
                (old, Some(src)) if old.as_deref() == replace.then_some(OLD) => {
                    assert_eq!(src, NEW, "{label}")
                }
                // After the commit: the new name holds the complete data.
                (Some(dest), _) => assert_eq!(dest, NEW, "{label}"),
                other => panic!("{label}: neither name resolves: {other:?}"),
            }
            if result.is_ok() {
                assert_eq!(at_dest.as_deref(), Some(NEW), "{label}");
                assert!(at_source.is_none(), "{label}");
                assert_clean(&io);
            } else {
                let leaks: Vec<_> = fsck(&io.snapshot());
                assert!(leaks.iter().all(|p| is_leak_class(p)), "{label}: {leaks:?}");
            }
            // Whatever the cut left, both names stay safe to unlink: the
            // extra link means removing one name leaves the other intact.
            if at_dest.as_deref() == Some(NEW) && at_source.is_some() {
                fs.unlink(from).unwrap();
                assert_eq!(fs.read_file(to).unwrap(), NEW, "{label}");
            }
            if result.is_ok() {
                break;
            }
            cut += 1;
            assert!(cut < 64, "a rename should take few writes");
        }
    }
}

#[test]
fn replacing_an_existing_file_frees_it() {
    let (io, from, to) = scenario(true, false);
    let fs = open(&io);
    let before = (fs.free_blocks().unwrap(), fs.free_inodes().unwrap());
    fs.rename(from, to).unwrap();
    assert_eq!(fs.read_file(to).unwrap(), NEW);
    assert!(fs.lookup(from).is_err());
    assert_eq!(fs.link_count(to).unwrap(), 1);
    // The victim's inode and block came back.
    assert_eq!(fs.free_inodes().unwrap(), before.1 + 1);
    assert!(fs.free_blocks().unwrap() > before.0);
    fs.flush().unwrap();
    assert_clean(&io);
}

#[test]
fn renaming_across_directories_moves_the_file() {
    for replace in [false, true] {
        let (io, from, to) = scenario(replace, true);
        let fs = open(&io);
        fs.rename(from, to).unwrap();
        assert_eq!(fs.read_file(to).unwrap(), NEW);
        assert!(fs.lookup(from).is_err());
        assert_eq!(fs.link_count(to).unwrap(), 1);
        fs.flush().unwrap();
        assert_clean(&io);
    }
}

#[test]
fn renaming_onto_itself_changes_nothing() {
    let (io, _, to) = scenario(true, false);
    let fs = open(&io);
    fs.rename(to, to).unwrap();
    assert_eq!(fs.read_file(to).unwrap(), OLD);
    assert_eq!(fs.link_count(to).unwrap(), 1);
    fs.flush().unwrap();
    assert_clean(&io);
}

#[test]
fn a_file_cannot_replace_a_directory() {
    let (io, fs) = fresh(2 * 1024 * 1024, 1024);
    put(&fs, "/f", NEW);
    fs.mkdir("/d", 0o755, crate::Owner::ROOT).unwrap();
    assert_eq!(fs.rename("/f", "/d"), Err(Ext2Error::IsDir));
    assert_eq!(fs.read_file("/f").unwrap(), NEW);
    assert_eq!(fs.link_count("/f").unwrap(), 1);
    fs.flush().unwrap();
    assert_clean(&io);
}

/// Write calls of a clean `op`, counted from the start of the run.
fn writes_of(base: &MemIo, op: impl Fn(&Ext2)) -> u64 {
    let io = MemIo::from_bytes(base.snapshot());
    let fs = open(&io);
    io.fail_write_number(u64::MAX); // arms nothing, resets the count
    op(&fs);
    io.write_calls()
}

/// One write of the rename fails once (a transient error, the disk keeps
/// working): the rename reports it, and the replaced file is not leaked unless
/// the failing write is one of the release's own. A late failure, after the
/// new name committed, must release the replaced file at once instead of
/// leaving it for an fsck.
#[test]
fn a_late_error_never_leaks_the_replaced_file() {
    let (base, from, to) = scenario(true, true);
    let total = writes_of(&base, |fs| fs.rename(from, to).unwrap());
    // The release is what a rename onto a free name does not write.
    let (free_base, _, _) = scenario(false, true);
    let release = total - writes_of(&free_base, |fs| fs.rename(from, to).unwrap());
    assert!(release > 0);
    let mut late_failures = 0;
    for call in 0..total {
        let io = MemIo::from_bytes(base.snapshot());
        let fs = open(&io);
        io.fail_write_number(call);
        let result = fs.rename(from, to);
        io.fail_write_number(u64::MAX);
        if result.is_ok() {
            assert_clean(&io); // the failed write was best-effort bookkeeping
            continue;
        }
        let dest = fs.read_file(to).unwrap();
        assert!(dest == OLD || dest == NEW, "call={call}");
        if dest != NEW {
            continue;
        }
        late_failures += 1;
        // Committed: the replaced file is already free (unless the failing
        // write was its own release), so at most the moved file's extra link
        // count is out of line.
        let releasing = call + release >= total;
        for problem in fsck(&io.snapshot()) {
            assert!(
                releasing || problem.contains("links"),
                "call={call}: {problem}"
            );
        }
    }
    assert!(late_failures > 1, "no write failed after the commit");
}

/// Rename the same temp over the same store 120 times: nothing may leak.
#[test]
fn a_rename_soak_returns_every_block_and_inode() {
    let (io, fs) = fresh(2 * 1024 * 1024, 1024);
    put(&fs, "/store", OLD);
    fs.flush().unwrap();
    let baseline = (fs.free_blocks().unwrap(), fs.free_inodes().unwrap());
    for generation in 0..120u32 {
        let data = std::format!(
            "generation {generation} {}",
            "x".repeat(2000 + generation as usize)
        );
        put(&fs, "/store.tmp", data.as_bytes());
        fs.rename("/store.tmp", "/store").unwrap();
        assert_eq!(fs.read_file("/store").unwrap(), data.as_bytes());
    }
    fs.flush().unwrap();
    let free = (fs.free_blocks().unwrap(), fs.free_inodes().unwrap());
    assert_eq!(free.1, baseline.1);
    assert!(
        free.0 <= baseline.0 + 4 && free.0 + 4 >= baseline.0,
        "{free:?} vs {baseline:?}"
    );
    assert_clean(&io);
}
