//! The futex family: wake counts, bitsets, requeue, `WAKE_OP`, per address
//! space keys, timeouts and the `ENOSYS` answer for what is not implemented.

use super::*;
use crate::process::linux::futex::test_hooks as hooks;

const FUTEX: u64 = 202;
const WAIT: u64 = 0;
const WAKE: u64 = 1;
const REQUEUE: u64 = 3;
const CMP_REQUEUE: u64 = 4;
const WAKE_OP: u64 = 5;
const WAIT_BITSET: u64 = 9;
const WAKE_BITSET: u64 = 10;
const PRIVATE: u64 = 128;

/// `n` parked threads (never run) that a test can register as waiters.
fn threads(n: usize) -> Result<Vec<usize>, String> {
    (0..n)
        .map(|_| {
            task::spawn_thread("futex", process::USER_STACK_TOP, 0, 0)
                .map_err(|error| format!("spawn: {error}"))
        })
        .collect()
}

fn blocked(slot: usize) -> bool {
    matches!(
        task::harness::state(slot),
        Some(task::TaskState::Blocked { .. })
    )
}

fn cleanup(slots: &[usize]) {
    for &slot in slots {
        hooks::forget(slot);
    }
    task::harness::reset();
}

fn futex(word: &u32, op: u64, val: u64, val2: u64, word2: u64, val3: u64) -> u64 {
    sys(
        FUTEX,
        &[word as *const u32 as u64, op, val, val2, word2, val3],
    )
}

/// `FUTEX_WAKE` wakes at most `val`, oldest first; a bitset wake only wakes
/// waiters whose bitset intersects.
pub fn futex_wake_counts_and_bitsets() -> Result<(), String> {
    fresh()?;
    let word = 0u32;
    let addr = &word as *const u32 as u64;
    let slots = threads(4)?;
    for &slot in &slots {
        hooks::park(slot, addr, u32::MAX);
    }
    let woken = futex(&word, WAKE | PRIVATE, 2, 0, 0, 0);
    check!(woken == 2, "WAKE 2 woke {woken}");
    check!(
        !blocked(slots[0]) && !blocked(slots[1]),
        "FIFO order broken"
    );
    check!(blocked(slots[2]) && blocked(slots[3]), "woke too many");
    check!(
        hooks::waiting(addr) == 2,
        "{} still queued",
        hooks::waiting(addr)
    );
    // Bitset: only the waiter whose mask meets 0b10 wakes.
    hooks::forget(slots[2]);
    hooks::forget(slots[3]);
    task::harness::reset();
    let slots = threads(2)?;
    hooks::park(slots[0], addr, 0b01);
    hooks::park(slots[1], addr, 0b10);
    let woken = futex(&word, WAKE_BITSET, 10, 0, 0, 0b10);
    check!(
        woken == 1 && blocked(slots[0]) && !blocked(slots[1]),
        "bitset wake {woken}"
    );
    check!(
        futex(&word, WAKE_BITSET, 1, 0, 0, 0) == EINVAL,
        "bitset 0 accepted"
    );
    cleanup(&slots);
    check!(hooks::total() == 0, "{} waiters leaked", hooks::total());
    Ok(())
}

/// `REQUEUE` wakes `val` and moves `val2`; `CMP_REQUEUE` checks the word
/// first and returns woken + moved.
pub fn futex_requeue_and_cmp_requeue() -> Result<(), String> {
    fresh()?;
    let (from, to) = (5u32, 0u32);
    let (a, b) = (&from as *const u32 as u64, &to as *const u32 as u64);
    let slots = threads(5)?;
    for &slot in &slots {
        hooks::park(slot, a, u32::MAX);
    }
    let moved = futex(&from, REQUEUE, 1, 2, b, 0);
    check!(
        moved == 1,
        "REQUEUE returned {moved}, want the woken count 1"
    );
    check!(
        hooks::waiting(a) == 2 && hooks::waiting(b) == 2,
        "after REQUEUE: {}/{}",
        hooks::waiting(a),
        hooks::waiting(b)
    );
    check!(
        futex(&from, CMP_REQUEUE, 1, 1, b, 6) == EAGAIN,
        "CMP_REQUEUE ignored a changed word"
    );
    check!(
        hooks::waiting(a) == 2,
        "a refused CMP_REQUEUE moved waiters"
    );
    let total = futex(&from, CMP_REQUEUE, 0, i32::MAX as u64, b, 5);
    check!(total == 2, "CMP_REQUEUE returned {total}, want 2 moved");
    check!(
        hooks::waiting(a) == 0 && hooks::waiting(b) == 4,
        "requeue targets {}/{}",
        hooks::waiting(a),
        hooks::waiting(b)
    );
    // The moved waiters now wake through the target word.
    check!(
        futex(&to, WAKE, 10, 0, 0, 0) == 4,
        "the requeued waiters did not wake on the target"
    );
    check!(
        futex(&from, REQUEUE, 1, 1, b + 1, 0) == EINVAL,
        "unaligned target accepted"
    );
    check!(
        futex(&from, REQUEUE, (-1i32) as u32 as u64, 1, b, 0) == EINVAL,
        "negative count accepted"
    );
    cleanup(&slots);
    Ok(())
}

/// `FUTEX_WAKE_OP`: the operation runs on the second word, the comparison
/// uses its old value, and both wakes happen as encoded.
pub fn futex_wake_op() -> Result<(), String> {
    fresh()?;
    // op SET(0) oparg 7, cmp GT(4) cmparg 0: old 3 > 0, so the second wake runs.
    let encode =
        |op: u32, cmp: u32, oparg: u32, cmparg: u32| op << 28 | cmp << 24 | oparg << 12 | cmparg;
    check!(
        hooks::wake_op(encode(0, 0, 7, 3), 3) == Some((7, true)),
        "SET/EQ"
    );
    check!(
        hooks::wake_op(encode(1, 1, 2, 3), 3) == Some((5, false)),
        "ADD/NE"
    );
    check!(
        hooks::wake_op(encode(2, 2, 4, 9), 1) == Some((5, true)),
        "OR/LT"
    );
    check!(
        hooks::wake_op(encode(3, 3, 1, 3), 3) == Some((2, true)),
        "ANDN/LE"
    );
    check!(
        hooks::wake_op(encode(4, 5, 1, 4), 3) == Some((2, false)),
        "XOR/GE"
    );
    check!(
        hooks::wake_op(encode(8, 0, 4, 0), 0) == Some((16, true)),
        "OPARG_SHIFT"
    );
    check!(
        hooks::wake_op(encode(5, 0, 0, 0), 0).is_none(),
        "op 5 decoded"
    );
    check!(
        hooks::wake_op(encode(0, 6, 0, 0), 0).is_none(),
        "cmp 6 decoded"
    );
    // The kernel writes the second word behind the compiler's back, so it is
    // read back with a volatile load.
    let mut cells = [0u32, 3u32];
    let a = &mut cells[0] as *mut u32 as u64;
    let b = &mut cells[1] as *mut u32 as u64;
    // SAFETY: `b` points at `cells[1]`, live for this whole test.
    let second = || unsafe { core::ptr::read_volatile(b as *const u32) };
    let wake_op = |val3: u32| sys(FUTEX, &[a, WAKE_OP, 1, 1, b, u64::from(val3)]);
    let slots = threads(4)?;
    hooks::park(slots[0], a, u32::MAX);
    hooks::park(slots[1], a, u32::MAX);
    hooks::park(slots[2], b, u32::MAX);
    hooks::park(slots[3], b, u32::MAX);
    let woken = wake_op(encode(0, 4, 7, 0));
    check!(woken == 2, "WAKE_OP woke {woken}, want 1 + 1");
    check!(second() == 7, "the second word is {}, want 7", second());
    // Old value 7 is not < 0: only the first word's waiter wakes.
    let woken = wake_op(encode(0, 2, 1, 0));
    check!(
        woken == 1 && second() == 1,
        "WAKE_OP with a false comparison woke {woken}"
    );
    check!(wake_op(encode(6, 0, 0, 0)) == ENOSYS, "a bad op ran");
    cleanup(&slots);
    Ok(())
}

/// A waiter in another address space is never woken through this one, even at
/// the same virtual address.
pub fn futex_keys_per_address_space() -> Result<(), String> {
    fresh()?;
    let word = 0u32;
    let addr = &word as *const u32 as u64;
    let slots = threads(2)?;
    let other_space = 0x7777_0000;
    hooks::park_in(slots[0], other_space, addr);
    hooks::park(slots[1], addr, u32::MAX);
    check!(
        futex(&word, WAKE, 10, 0, 0, 0) == 1,
        "the wake crossed address spaces"
    );
    check!(blocked(slots[0]), "the foreign waiter was woken");
    check!(
        hooks::waiting_in(other_space, addr) == 1,
        "the foreign waiter left its queue"
    );
    cleanup(&slots);
    check!(hooks::total() == 0, "waiters leaked");
    Ok(())
}

/// A wait on a changed word is `EAGAIN`, a timed wait times out, a past
/// absolute deadline is immediate, and bad arguments are refused.
pub fn futex_timeouts_and_errors() -> Result<(), String> {
    fresh()?;
    let word = 1u32;
    check!(
        futex(&word, WAIT, 2, 0, 0, 0) == EAGAIN,
        "mismatch did not EAGAIN"
    );
    let relative = [0i64, 30_000_000];
    let start = task::ticks();
    let got = futex(&word, WAIT, 1, relative.as_ptr() as u64, 0, 0);
    let waited = task::ticks() - start;
    check!(got == ETIMEDOUT, "timed wait returned {got:#x}");
    check!(
        (3..=20).contains(&waited),
        "a 30 ms wait took {waited} ticks"
    );
    let zero = [0i64, 0];
    check!(
        futex(&word, WAIT, 1, zero.as_ptr() as u64, 0, 0) == ETIMEDOUT,
        "zero timeout blocked"
    );
    let past = [0i64, 0];
    check!(
        futex(
            &word,
            WAIT_BITSET,
            1,
            past.as_ptr() as u64,
            0,
            u64::from(u32::MAX)
        ) == ETIMEDOUT,
        "a past absolute deadline blocked"
    );
    let bad = [0i64, 1_000_000_000];
    check!(
        futex(&word, WAIT, 1, bad.as_ptr() as u64, 0, 0) == EINVAL,
        "a bad timespec was accepted"
    );
    let unaligned = sys(FUTEX, &[(&word as *const u32 as u64) + 1, WAKE, 1]);
    check!(unaligned == EINVAL, "an unaligned word was accepted");
    check!(
        sys_checked(FUTEX, &[8, WAIT, 0, 0, 0, 0]) == EFAULT,
        "an unmapped word did not EFAULT"
    );
    check!(hooks::total() == 0, "a finished wait left an entry");
    Ok(())
}

/// The priority-inheritance family and `FUTEX_FD` are `ENOSYS`, never 0.
pub fn futex_unknown_ops_enosys() -> Result<(), String> {
    fresh()?;
    let word = 0u32;
    for op in [2u64, 6, 7, 8, 11, 12, 13, 14, 99] {
        let got = futex(&word, op | PRIVATE, 0, 0, 0, 0);
        check!(got == ENOSYS, "op {op} returned {got:#x}");
    }
    // CLOCK_REALTIME only belongs to the waits.
    check!(
        futex(&word, WAKE | 256, 1, 0, 0, 0) == ENOSYS,
        "WAKE|CLOCK_REALTIME accepted"
    );
    Ok(())
}

/// Many park/wake/requeue rounds over a handful of words and threads: every
/// waiter is accounted for and nothing is left in the table.
pub fn futex_soak() -> Result<(), String> {
    fresh()?;
    let words = [0u32; 4];
    let addr = |i: usize| &words[i] as *const u32 as u64;
    let slots = threads(8)?;
    let mut parked = 0usize;
    for round in 0..4000usize {
        let slot = slots[round % slots.len()];
        if !blocked(slot) {
            hooks::park(slot, addr(round % 4), 1 << (round % 3));
            parked += 1;
        }
        match round % 5 {
            0 => parked -= futex(&words[round % 4], WAKE, 1, 0, 0, 0) as usize,
            1 => parked -= futex(&words[round % 4], WAKE_BITSET, 2, 0, 0, 0b101) as usize,
            2 => {
                let to = addr((round + 1) % 4);
                futex(&words[round % 4], REQUEUE, 0, 3, to, 0);
            }
            _ => {}
        }
    }
    for i in 0..4 {
        parked -= futex(&words[i], WAKE, 64, 0, 0, 0) as usize;
    }
    check!(parked == 0, "{parked} waiters unaccounted for");
    check!(hooks::total() == 0, "{} entries leaked", hooks::total());
    check!(
        slots.iter().all(|&slot| !blocked(slot)),
        "a thread is still blocked"
    );
    cleanup(&slots);
    Ok(())
}

/// The hashed table (P6.5): 48 waiters on 48 distinct words spread over many
/// buckets; a wake of one word finds exactly its own waiter, a requeue moves
/// a waiter between buckets and the moved waiter is then woken by its new
/// word only, and nothing is left behind. 200 rounds.
pub fn futex_hashed_buckets() -> Result<(), String> {
    fresh()?;
    let words = [0u32; 48];
    let slots = threads(words.len())?;
    let mut spread = 0;
    for round in 0..200usize {
        for (slot, word) in slots.iter().zip(&words) {
            hooks::park(*slot, word as *const u32 as u64, u32::MAX);
        }
        spread = spread.max(hooks::buckets_in_use());
        let pick = round % words.len();
        let to = (pick + 7) % words.len();
        // Move `pick`'s waiter onto `to`: `to` now has two, `pick` none.
        check!(
            futex(
                &words[pick],
                REQUEUE,
                0,
                1,
                &words[to] as *const u32 as u64,
                0
            ) == 0,
            "round {round}: requeue woke someone"
        );
        check!(
            hooks::waiting(&words[pick] as *const u32 as u64) == 0
                && hooks::waiting(&words[to] as *const u32 as u64) == 2,
            "round {round}: the requeue did not move the waiter"
        );
        check!(
            futex(&words[pick], WAKE, 64, 0, 0, 0) == 0,
            "round {round}: the old word still woke a waiter"
        );
        for (index, word) in words.iter().enumerate() {
            let want = match index {
                index if index == pick => 0,
                index if index == to => 2,
                _ => 1,
            };
            let woken = futex(word, WAKE, 64, 0, 0, 0);
            check!(
                woken == want,
                "round {round}: word {index} woke {woken}, expected {want}"
            );
        }
        check!(
            hooks::total() == 0,
            "round {round}: {} entries left",
            hooks::total()
        );
    }
    check!(
        spread >= 16,
        "48 distinct words used only {spread} buckets: the hash does not spread"
    );
    cleanup(&slots);
    Ok(())
}
