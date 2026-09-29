//! `sendfile(2)` (syscall 40), which BusyBox `cat` uses for a regular file to
//! stdout. The kernel-side copy runs entirely between two descriptors, so the
//! tests drive it pipe-to-pipe (a stream source and stream sink) and check the
//! byte accounting, the offset rejection, and that many transfers leave no
//! descriptors or pipe buffers behind.

use super::*;

/// `pipe(fds)` helper: returns the read and write descriptors.
fn pipe_pair() -> Result<(u64, u64), String> {
    let mut fds = [0i32; 2];
    let ret = process::linux::dispatch_for_test(22, fds.as_mut_ptr() as u64, 0, 0);
    check!(ret == 0, "pipe returned {ret:#x}");
    Ok((fds[0] as u64, fds[1] as u64))
}

/// `sendfile(out, in, offset, count)` (syscall 40).
fn sendfile(out: u64, input: u64, offset: u64, count: u64) -> u64 {
    process::linux::dispatch_args_for_test(40, out, input, offset, count)
}

pub(super) fn sendfile_pipe_to_pipe_copies_and_accounts() -> Result<(), String> {
    fresh()?;
    let (src_r, src_w) = pipe_pair()?;
    let (dst_r, dst_w) = pipe_pair()?;

    let msg = b"hello sendfile";
    let wrote = write_fd(src_w, msg);
    check!(wrote == msg.len() as u64, "pipe write returned {wrote}");

    // A short count copies only that much and stops, leaving the rest queued.
    let first = sendfile(dst_w, src_r, 0, 5);
    check!(first == 5, "short sendfile copied {first}, expected 5");
    // count beyond what remains returns the remaining bytes and stops at EOF.
    let rest = sendfile(dst_w, src_r, 0, 4096);
    check!(
        rest == (msg.len() - 5) as u64,
        "tail sendfile copied {rest}, expected {}",
        msg.len() - 5
    );

    let mut got = [0u8; 32];
    let n = read_fd(dst_r, &mut got[..msg.len()]);
    check!(n == msg.len() as u64, "read back {n} bytes");
    check!(
        &got[..n as usize] == msg,
        "sendfile round-trip mismatch: {:?}",
        &got[..n as usize]
    );
    Ok(())
}

pub(super) fn sendfile_rejects_positional_and_bad_descriptors() -> Result<(), String> {
    fresh()?;
    let (src_r, src_w) = pipe_pair()?;
    let (_dst_r, dst_w) = pipe_pair()?;
    let wrote = write_fd(src_w, b"abc");
    check!(wrote == 3, "pipe write returned {wrote}");

    // A non-NULL offset asks for the positional form, which is not implemented.
    let positional = sendfile(dst_w, src_r, 0x1000, 3);
    check!(
        positional == EINVAL,
        "positional offset returned {positional:#x}"
    );

    // A regular-file destination is refused (not something a shell asks for).
    let bad_out = sendfile(999, src_r, 0, 3);
    check!(bad_out == EINVAL, "bad out fd returned {bad_out:#x}");

    // A closed source is an errno, never a panic.
    let bad_in = sendfile(dst_w, 999, 0, 3);
    check!(bad_in == EINVAL, "bad in fd returned {bad_in:#x}");

    // Zero count is a no-op success, as on Linux.
    let zero = sendfile(dst_w, src_r, 0, 0);
    check!(zero == 0, "zero-count sendfile returned {zero:#x}");
    Ok(())
}

pub(super) fn sendfile_soak_cycles_no_leaks() -> Result<(), String> {
    fresh()?;
    for round in 0..64u32 {
        let (src_r, src_w) = match pipe_pair() {
            Ok(pair) => pair,
            Err(reason) => return Err(format!("round {round}: {reason}")),
        };
        let (dst_r, dst_w) = match pipe_pair() {
            Ok(pair) => pair,
            Err(reason) => return Err(format!("round {round}: {reason}")),
        };
        let byte = (round as u8) ^ 0x5a;
        let wrote = write_fd(src_w, &[byte; 64]);
        check!(wrote == 64, "round {round}: write returned {wrote}");
        let copied = sendfile(dst_w, src_r, 0, 64);
        check!(copied == 64, "round {round}: sendfile copied {copied}");
        let mut got = [0u8; 64];
        let n = read_fd(dst_r, &mut got);
        check!(
            n == 64 && got.iter().all(|b| *b == byte),
            "round {round}: read back {n} bytes"
        );
        for fd in [src_r, src_w, dst_r, dst_w] {
            check!(
                task::fd_close(fd as usize),
                "round {round}: close {fd} failed"
            );
        }
    }
    check!(fds_clean(), "sendfile soak leaked descriptors");
    Ok(())
}
