//! `select`, `pselect6` and `ppoll` over pipes.

use super::*;

const SELECT: u64 = 23;
const PSELECT6: u64 = 270;
const PPOLL: u64 = 271;

/// An `fd_set` with the given descriptors.
fn set(fds: &[u64]) -> [u64; 16] {
    let mut bits = [0u64; 16];
    for &fd in fds {
        bits[(fd / 64) as usize] |= 1 << (fd % 64);
    }
    bits
}

fn has(bits: &[u64; 16], fd: u64) -> bool {
    bits[(fd / 64) as usize] & (1 << (fd % 64)) != 0
}

/// Readiness is reported per set, the sets are rewritten to what is ready,
/// and `ppoll`/`pselect6` agree with `select`.
pub fn select_and_ppoll() -> Result<(), String> {
    fresh()?;
    let (r, w) = pipe()?;
    let nfds = w.max(r) + 1;
    let (mut rd, mut wr) = (set(&[r]), set(&[w]));
    let zero = [0i64, 0];
    let got = sys(
        SELECT,
        &[
            nfds,
            rd.as_mut_ptr() as u64,
            wr.as_mut_ptr() as u64,
            0,
            zero.as_ptr() as u64,
        ],
    );
    check!(
        got == 1 && !has(&rd, r) && has(&wr, w),
        "empty pipe: {got} read={} write={}",
        has(&rd, r),
        has(&wr, w)
    );
    check!(sys(1, &[w, b"x".as_ptr() as u64, 1]) == 1, "write");
    let (mut rd, mut wr) = (set(&[r]), set(&[w]));
    let got = sys(
        SELECT,
        &[
            nfds,
            rd.as_mut_ptr() as u64,
            wr.as_mut_ptr() as u64,
            0,
            zero.as_ptr() as u64,
        ],
    );
    check!(got == 2 && has(&rd, r) && has(&wr, w), "with data: {got}");
    // pselect6 with a mask pointer block { set, size }.
    let mask = 0u64;
    let block = [&mask as *const u64 as u64, 8];
    let mut rd = set(&[r]);
    let got = sys(
        PSELECT6,
        &[
            nfds,
            rd.as_mut_ptr() as u64,
            0,
            0,
            zero.as_ptr() as u64,
            block.as_ptr() as u64,
        ],
    );
    check!(got == 1 && has(&rd, r), "pselect6: {got}");
    let bad_block = [&mask as *const u64 as u64, 4];
    check!(
        sys(
            PSELECT6,
            &[
                nfds,
                rd.as_mut_ptr() as u64,
                0,
                0,
                zero.as_ptr() as u64,
                bad_block.as_ptr() as u64
            ]
        ) == EINVAL,
        "a 4-byte sigset was accepted"
    );
    // ppoll: struct pollfd { fd, events, revents }.
    let mut pfd = [(r as u32 as u64) | (1u64 << 32)];
    let got = sys(
        PPOLL,
        &[pfd.as_mut_ptr() as u64, 1, zero.as_ptr() as u64, 0, 8],
    );
    check!(
        got == 1 && (pfd[0] >> 48) & 1 == 1,
        "ppoll: {got} revents {:#x}",
        pfd[0] >> 48
    );
    // Hang-up: the write end closed with data drained is readable (EOF).
    let mut byte = [0u8; 1];
    sys(0, &[r, byte.as_mut_ptr() as u64, 1]);
    sys(3, &[w]);
    let mut rd = set(&[r]);
    check!(
        sys(
            SELECT,
            &[r + 1, rd.as_mut_ptr() as u64, 0, 0, zero.as_ptr() as u64]
        ) == 1,
        "EOF is not readable"
    );
    sys(3, &[r]);
    Ok(())
}

/// A timeout waits (and the remaining time is written back), a closed
/// descriptor in a set is `EBADF`, and an oversized `nfds` is `EINVAL`.
pub fn select_timeout_and_ebadf() -> Result<(), String> {
    fresh()?;
    let (r, w) = pipe()?;
    let mut rd = set(&[r]);
    let mut tv = [0i64, 30_000]; // 30 ms as a timeval
    let start = task::ticks();
    let got = sys(
        SELECT,
        &[r + 1, rd.as_mut_ptr() as u64, 0, 0, tv.as_mut_ptr() as u64],
    );
    let waited = task::ticks() - start;
    check!(got == 0, "an idle select returned {got:#x}");
    check!(
        (3..=20).contains(&waited),
        "a 30 ms select took {waited} ticks"
    );
    check!(tv == [0, 0], "the timeval was not run down: {tv:?}");
    check!(!has(&rd, r), "a timed-out select left a bit set");
    let mut rd = set(&[12]);
    let zero = [0i64, 0];
    check!(
        sys(
            SELECT,
            &[13, rd.as_mut_ptr() as u64, 0, 0, zero.as_ptr() as u64]
        ) == EBADF,
        "closed fd not EBADF"
    );
    check!(
        sys(SELECT, &[5000, 0, 0, 0, zero.as_ptr() as u64]) == EINVAL,
        "nfds 5000 accepted"
    );
    let bad = [0i64, 1_000_000];
    check!(
        sys(SELECT, &[1, 0, 0, 0, bad.as_ptr() as u64]) == EINVAL,
        "a bad timeval accepted"
    );
    let mut ts = [0i64, 20_000_000];
    let mut pfd = [(r as u32 as u64) | (1u64 << 32)];
    check!(
        sys(
            PPOLL,
            &[pfd.as_mut_ptr() as u64, 1, ts.as_mut_ptr() as u64, 0, 8]
        ) == 0,
        "idle ppoll"
    );
    sys(3, &[r]);
    sys(3, &[w]);
    Ok(())
}

/// Thousands of zero-timeout selects over a changing set of pipes agree with
/// what was written to them.
pub fn select_soak() -> Result<(), String> {
    fresh()?;
    let pipes: Vec<(u64, u64)> = (0..4).map(|_| pipe()).collect::<Result<_, _>>()?;
    let nfds = pipes.iter().map(|&(r, w)| r.max(w)).max().unwrap_or(0) + 1;
    let zero = [0i64, 0];
    let mut filled = [false; 4];
    for round in 0..4000usize {
        let i = round % 4;
        let (r, w) = pipes[i];
        if round % 3 == 0 && !filled[i] {
            sys(1, &[w, b"z".as_ptr() as u64, 1]);
            filled[i] = true;
        } else if round % 5 == 0 && filled[i] {
            let mut byte = [0u8; 1];
            sys(0, &[r, byte.as_mut_ptr() as u64, 1]);
            filled[i] = false;
        }
        let reads: Vec<u64> = pipes.iter().map(|&(r, _)| r).collect();
        let mut rd = set(&reads);
        let got = sys(
            SELECT,
            &[nfds, rd.as_mut_ptr() as u64, 0, 0, zero.as_ptr() as u64],
        );
        let want = filled.iter().filter(|&&f| f).count() as u64;
        check!(got == want, "round {round}: {got} ready, want {want}");
        for (j, &(r, _)) in pipes.iter().enumerate() {
            check!(
                has(&rd, r) == filled[j],
                "round {round}: pipe {j} misreported"
            );
        }
    }
    for (r, w) in pipes {
        sys(3, &[r]);
        sys(3, &[w]);
    }
    Ok(())
}
