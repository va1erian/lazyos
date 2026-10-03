//! `spawnv` soaks: many spawn/exit cycles with random vectors, and many
//! refusals, leave frames, kernel memory, argument blocks, task slots and the
//! interned task names where they started.

use super::*;
use crate::process::spawnv::{E2BIG, ENAMETOOLONG};

const CYCLES: u32 = 10_000;
/// Allocator slack over a soak: slab caches keep partial pages.
const SLACK: usize = 16 * 1024;

/// xorshift64*: deterministic, so a failure replays.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// A value in `0..bound` (`bound > 0`).
    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

/// Up to `max_count` random strings whose block (one NUL each) fits
/// `budget` bytes: letters, spaces (leading, trailing, runs) and a two-byte
/// UTF-8 letter. `env` strings are `K<i>=<value>`.
fn random_strings(rng: &mut Rng, min_count: u64, max_count: u64, env: bool) -> Vec<Vec<u8>> {
    const ALPHABET: &[&[u8]] = &[
        b"a",
        b"Z",
        b"0",
        b" ",
        b"  ",
        b"-",
        b"=",
        "\u{e9}".as_bytes(),
    ];
    let count = min_count + rng.below(max_count - min_count + 1);
    let budget = 1 + rng.below(4096) as usize;
    let mut used = 0;
    let mut items = Vec::new();
    for index in 0..count {
        let mut item = if env {
            format!("K{index}=").into_bytes()
        } else {
            Vec::new()
        };
        let target = rng.below(128) as usize;
        while item.len() < target {
            let piece = ALPHABET[rng.below(ALPHABET.len() as u64) as usize];
            item.extend_from_slice(piece);
        }
        if used + item.len() + 1 > budget {
            if index >= min_count {
                break;
            }
            // The required first item still fits: shrink it.
            item.truncate(budget.saturating_sub(used + 1).min(item.len()));
            // Never split the two-byte letter: a native child needs UTF-8.
            while core::str::from_utf8(&item).is_err() {
                item.pop();
            }
            if env && !item.contains(&b'=') {
                break;
            }
        }
        used += item.len() + 1;
        items.push(item);
    }
    items
}

/// The first differing item: `(index, got, want)` as text, for a failure.
fn first_diff(got: &[Vec<u8>], want: &[Vec<u8>]) -> Option<(usize, String, String)> {
    let text = |item: Option<&Vec<u8>>| {
        String::from_utf8_lossy(item.map_or(&[][..], Vec::as_slice)).into_owned()
    };
    (0..got.len().max(want.len()))
        .find(|index| got.get(*index) != want.get(*index))
        .map(|index| (index, text(got.get(index)), text(want.get(index))))
}

/// One spawn/check/exit cycle with random vectors under `personality`.
fn cycle(rng: &mut Rng, linux: bool, round: u32) -> Result<(), String> {
    let argv = random_strings(rng, 1, 64, false);
    let envp = random_strings(rng, 0, 64, true);
    let argv_refs: Vec<&[u8]> = argv.iter().map(Vec::as_slice).collect();
    let envp_refs: Vec<&[u8]> = envp.iter().map(Vec::as_slice).collect();
    let path = if linux { LINUX } else { NATIVE };
    let request = Req::new(path, &argv_refs, &envp_refs, linux);
    check!(
        request.argv.len() <= 4096 && request.envp.len() <= 4096,
        "round {round}: generator overflowed"
    );
    let slot = spawned(request.call()).map_err(|e| format!("round {round}: {e}"))?;
    if linux {
        let (got_argv, got_envp) = linux_start_stack(slot)?;
        check!(
            got_argv == argv && got_envp == envp,
            "round {round}: the Linux start stack differs: argv {}/{} envp {}/{}, first argv diff {:?}, first envp diff {:?}",
            got_argv.len(),
            argv.len(),
            got_envp.len(),
            envp.len(),
            first_diff(&got_argv, &argv),
            first_diff(&got_envp, &envp)
        );
    } else {
        check!(
            child_block(slot, 0) == request.argv && child_block(slot, 1) == request.envp,
            "round {round}: the native blocks differ"
        );
    }
    reap(slot)
}

/// 10 000 `spawnv`/exit cycles alternating native and Linux, with random
/// `argv`/`envp` up to the limits.
pub fn soak_spawnv_exit_cycles() -> Result<(), String> {
    fresh()?;
    let mut rng = Rng(0x5eed_f3_5a_a4_0507);
    // Warm-up absorbs one-time allocations (interned names, caches).
    cycle(&mut rng, false, 0)?;
    cycle(&mut rng, true, 0)?;
    let names = (
        process::intern_service_name_for_test(NATIVE),
        process::intern_service_name_for_test(LINUX),
    );
    let before = usage();
    for round in 0..CYCLES {
        cycle(&mut rng, round % 2 == 1, round)?;
    }
    let after = usage();
    let names_after = (
        process::intern_service_name_for_test(NATIVE),
        process::intern_service_name_for_test(LINUX),
    );
    check!(
        core::ptr::eq(names.0, names_after.0) && core::ptr::eq(names.1, names_after.1),
        "the task names were re-interned"
    );
    no_leak(before, after, SLACK, "spawn/exit soak")
}

/// One refused request, cycling through the refusal kinds; the request is in
/// user memory and validation is on.
fn denied(round: u32, noexec: Option<&str>) -> Result<(), String> {
    let base = Req::new(NATIVE, &[b"prog", b"a b"], &[b"K=V"], false);
    let (what, errno, code) = match round % 6 {
        0 => (
            "EFAULT",
            14,
            call_placed(&base, |w| w[2] = errors::UNMAPPED),
        ),
        1 => ("E2BIG", E2BIG, call_placed(&base, |w| w[3] = 4097)),
        2 => ("EINVAL", 22, call_placed(&base, |w| w[4] = 3)),
        3 => (
            "ENAMETOOLONG",
            ENAMETOOLONG,
            call_placed(&base, |w| w[1] = 256),
        ),
        4 => {
            let mut request = base.clone();
            request.mode = cred_mode::AS;
            request.cred = Cred::new(0, 0, credentials::CAP_ALL, 0, 0);
            ("EPERM", 1, call_placed(&request, |_| {}))
        }
        _ => match noexec {
            Some(path) => {
                let request = Req {
                    path: path.as_bytes().to_vec(),
                    ..base
                };
                ("EACCES", 13, call_placed(&request, |_| {}))
            }
            None => {
                let request = Req {
                    path: b"/tmp/spawn suite/none".to_vec(),
                    ..base
                };
                ("ENOENT", 2, call_placed(&request, |_| {}))
            }
        },
    };
    check!(
        code == failed(errno),
        "round {round}: {what} case returned {code:#x}"
    );
    Ok(())
}

/// 10 000 refused spawns (`EFAULT`, `E2BIG`, `EINVAL`, `ENAMETOOLONG`,
/// `EPERM`, and `EACCES` from a `noexec` mount when the image has one, else
/// `ENOENT`) leak nothing.
pub fn soak_denied_spawns() -> Result<(), String> {
    fresh()?;
    // `/boot` is `noexec` on an image with the FHS layout.
    let probe = "/boot/spawnv-probe";
    let noexec = crate::fs::mount_flags(probe).noexec.then_some(probe);
    let me = task::current();
    // Unprivileged, so the credential mode is refused.
    credentials::set(me, Cred::new(1000, 1000, 0, 0, 0));
    let outcome = errors::in_space(|| -> Result<(), String> {
        for round in 0..12 {
            denied(round, noexec)?;
        }
        let before = usage();
        for round in 0..CYCLES {
            denied(round, noexec)?;
        }
        no_leak(before, usage(), SLACK, "denied soak")
    });
    credentials::reset_for_task(me);
    outcome
}
