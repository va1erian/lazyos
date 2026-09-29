//! `writev`/`readv`/`poll`/`getrandom` bound their user-supplied counts and
//! refuse bad or wrapping arrays instead of looping over them with interrupts
//! off (#226).

use super::*;

const SYS_POLL: u64 = 7;
const SYS_READV: u64 = 19;
const SYS_WRITEV: u64 = 20;
const SYS_GETRANDOM: u64 = 318;
const EINVAL: i64 = 22;

/// Fill `[addr, addr + len)` (mapped scratch memory) with `value`.
fn fill(addr: u64, len: usize, value: u8) {
    // Safety: callers pass ranges inside the scratch pages `in_space` mapped.
    unsafe { core::ptr::write_bytes(addr as *mut u8, value, len) };
}

/// Write one `iovec` entry at `array[index]`.
fn set_iovec(array: u64, index: u64, base: u64, len: u64) {
    let entry = (array + index * 16) as *mut u64;
    // Safety: `array` lies in the mapped scratch pages and `index` is small.
    unsafe {
        entry.write(base);
        entry.add(1).write(len);
    }
}

/// Counts past the caps are `-EINVAL` before anything is read, and a bad array
/// (unmapped, or wrapping past the address space) is `-EFAULT`: none of them
/// loop 2^64 times or overflow the address arithmetic.
pub fn vectored_io_and_poll_reject_hostile_counts() -> Result<(), String> {
    fresh()?;
    let _strict = Strict::on();
    let call = process::linux::dispatch_for_test;
    let bad = 0xdead_0000u64;

    for count in [1025u64, 1 << 32, u64::MAX] {
        for nr in [SYS_WRITEV, SYS_READV] {
            let code = call(nr, 1, bad, count);
            check!(
                code == failed(EINVAL),
                "syscall {nr} with count {count:#x} -> {code:#x}"
            );
        }
        let code = call(SYS_POLL, bad, count, 0);
        check!(
            code == failed(EINVAL),
            "poll with nfds {count:#x} -> {code:#x}"
        );
    }

    // Within the cap but unreadable: EFAULT on the first entry.
    for count in [1u64, 1024] {
        for nr in [SYS_WRITEV, SYS_READV] {
            let code = call(nr, 1, bad, count);
            check!(
                code == failed(EFAULT),
                "syscall {nr} on an unmapped array -> {code:#x}"
            );
        }
        let code = call(SYS_POLL, bad, count, 0);
        check!(
            code == failed(EFAULT),
            "poll on an unmapped array -> {code:#x}"
        );
    }
    // Arrays whose entries wrap past `u64::MAX` are refused, not overflowed.
    for near_top in [u64::MAX - 8, u64::MAX - 15, u64::MAX] {
        for nr in [SYS_WRITEV, SYS_READV] {
            let code = call(nr, 1, near_top, 2);
            check!(
                code == failed(EFAULT),
                "syscall {nr} on array at {near_top:#x} -> {code:#x}"
            );
        }
        let code = call(SYS_POLL, near_top, 2, 0);
        check!(code == failed(EFAULT), "poll at {near_top:#x} -> {code:#x}");
    }
    Ok(())
}

/// `getrandom` never loops past its cap, reports `-EFAULT` for a buffer that
/// cannot be written, and never wraps the destination address.
pub fn getrandom_is_bounded_and_checks_the_buffer() -> Result<(), String> {
    fresh()?;
    let _strict = Strict::on();
    let call = process::linux::dispatch_for_test;

    for buffer in [0xdead_0000u64, 0, u64::MAX - 7, 0xffff_8000_0000_0000] {
        for len in [1u64, 4096, u64::MAX] {
            let code = call(SYS_GETRANDOM, buffer, len, 0);
            check!(
                code == failed(EFAULT),
                "getrandom({buffer:#x}, {len:#x}) -> {code:#x}"
            );
        }
    }
    in_space(|| -> Result<(), String> {
        fill(SPACE, 0x8000, 0);
        // An absurd length is served as a short read, not 2^56 loop turns.
        let code = call(SYS_GETRANDOM, SPACE, u64::MAX, 0);
        check!(
            (1..=4096).contains(&code),
            "huge getrandom returned {code:#x}"
        );
        // Safety: mapped scratch memory the call just filled.
        let filled = unsafe { core::slice::from_raw_parts(SPACE as *const u8, code as usize) };
        check!(
            filled.iter().any(|byte| *byte != 0),
            "the buffer stayed zero"
        );
        check!(
            call(SYS_GETRANDOM, SPACE, 0, 0) == 0,
            "zero-length getrandom"
        );
        check!(call(SYS_GETRANDOM, SPACE, 17, 0) == 17, "short getrandom");
        Ok(())
    })
}

/// The positive controls: real iovec and pollfd arrays, up to the caps, still
/// work and add up.
pub fn vectored_io_within_limits_still_works() -> Result<(), String> {
    fresh()?;
    let _strict = Strict::on();
    let call = process::linux::dispatch_for_test;
    in_space(|| -> Result<(), String> {
        fill(SPACE, 0x8000, 0);
        // Newlines: `writev` to fd 1 lands on the serial log, and anything else
        // could glue itself onto a `TEST:` line.
        fill(SPACE + 0x4000, 0x20, 0x0a);
        set_iovec(SPACE, 0, SPACE + 0x4000, 5);
        set_iovec(SPACE, 1, SPACE + 0x4010, 3);
        let code = call(SYS_WRITEV, 1, SPACE, 2);
        check!(code == 8, "writev of 5+3 bytes -> {code:#x}");
        // The full cap of empty segments is legal and totals zero.
        let code = call(SYS_WRITEV, 1, SPACE, 0);
        check!(code == 0, "writev with no segments -> {code:#x}");
        for index in 0..1024 {
            set_iovec(SPACE, index, SPACE + 0x4000, 0);
        }
        let code = call(SYS_WRITEV, 1, SPACE, 1024);
        check!(code == 0, "writev with 1024 empty segments -> {code:#x}");

        // 1024 `pollfd`s with negative descriptors are ignored: zero ready.
        for index in 0..1024u64 {
            let entry = (SPACE + 0x4000 + index * 8) as *mut i32;
            // Safety: inside the mapped scratch pages (8 KiB from +0x4000).
            unsafe { entry.write(-1) };
        }
        let code = call(SYS_POLL, SPACE + 0x4000, 1024, 0);
        check!(code == 0, "poll of 1024 ignored fds -> {code:#x}");
        Ok(())
    })
}

/// Sustained load: many hostile and valid calls in a row stay bounded and
/// consistent.
pub fn soak_bounded_user_counts() -> Result<(), String> {
    fresh()?;
    let _strict = Strict::on();
    let call = process::linux::dispatch_for_test;
    in_space(|| -> Result<(), String> {
        fill(SPACE, 0x8000, 0);
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        for round in 0..20_000u32 {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let huge = (seed | 0x400).max(1025);
            let code = call(SYS_WRITEV, 1, SPACE, huge);
            check!(code == failed(EINVAL), "round {round}: writev -> {code:#x}");
            let code = call(SYS_POLL, SPACE, huge, 0);
            check!(code == failed(EINVAL), "round {round}: poll -> {code:#x}");
            if round % 64 == 0 {
                let code = call(SYS_GETRANDOM, SPACE, seed, 0);
                check!(
                    (1..=4096).contains(&code),
                    "round {round}: getrandom -> {code:#x}"
                );
            }
        }
        Ok(())
    })
}
