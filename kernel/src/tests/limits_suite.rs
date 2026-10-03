//! Kernel resource limits (`crate::limits`): the `limit.*` lines of
//! `lazyos.cfg` (including hostile values), the machine-derived defaults for
//! every RAM and screen size, and applying a config at runtime.

use super::*;
use crate::limits::{self, parse, parse_size, Id, Limits, Outcome, Override, Source, KEYS, MIB};

const GIB: u64 = 1 << 30;

/// The `Set` outcomes of `text`, in order.
fn sets(text: &str) -> Vec<Override> {
    parse(text)
        .into_iter()
        .filter_map(|outcome| match outcome {
            Outcome::Set(set) => Some(set),
            _ => None,
        })
        .collect()
}

/// Well-formed values of every kind parse exactly, unclamped.
pub fn parses_values() -> Result<(), String> {
    let got = sets(
        "# limits\nlimit.heap_max=512M\nlimit.fd_max = 4096 \nlimit.stack_size=16m # big\n\
         limit.quota_user_memory=2G\nlimit.quota_kernel_memory=262144K\n\
         limit.shared_buffer_max=1t\nroot=UUID=ignored-here\n",
    );
    let want = [
        (Id::HeapMax, 512 * MIB),
        (Id::FdMax, 4096),
        (Id::StackSize, 16 * MIB),
        (Id::QuotaUserMemory, 2 * GIB),
        (Id::QuotaKernelMemory, 256 * MIB),
        (Id::SharedBufferMax, 64 * GIB),
    ];
    check!(got.len() == want.len(), "parsed {got:?}");
    for (set, (id, value)) in got.iter().zip(want) {
        check!(set.id == id, "id {:?}, want {id:?}", set.id);
        check!(set.value == value, "{id:?} = {}, want {value}", set.value);
    }
    // 1 TiB of shared buffers is past the key's range: clamped, and said so.
    check!(got[5].clamped && !got[0].clamped, "clamp flags {got:?}");
    check!(
        parse_size("0") == Some(0) && parse_size("7K") == Some(7 << 10),
        "sizes"
    );
    Ok(())
}

/// Hostile values never panic, never escape a key's range, and never turn a
/// bad line into a value: each is reported and the default kept.
pub fn rejects_hostile_values() -> Result<(), String> {
    let malformed = [
        "limit.heap_max=",
        "limit.heap_max=-1",
        "limit.heap_max=1.5G",
        "limit.heap_max=0x100000",
        "limit.heap_max=12Q",
        "limit.heap_max=G",
        "limit.heap_max=99999999999999999999",
        "limit.heap_max=18446744073709551615G",
        "limit.heap_max=17179869184G",
        "limit.heap_max=\u{ff11}\u{ff12}M",
        "limit.fd_max=1M",
        "limit.fd_max=+5",
        "limit.fd_max",
    ];
    for line in malformed {
        let outcome = parse(line);
        check!(
            matches!(outcome.as_slice(), [Outcome::Malformed(_)]),
            "{line:?} gave {outcome:?}"
        );
    }
    let unknown = parse("limit.nope=5\nlimit.=1\nlimit.HEAP_MAX=1G");
    check!(
        unknown.iter().all(|o| matches!(o, Outcome::Unknown(_))) && unknown.len() == 3,
        "unknown keys gave {unknown:?}"
    );
    // Out of range in both directions: clamped to the key's bounds.
    let got = sets("limit.heap_max=1\nlimit.fd_max=0\nlimit.stack_size=1T\nlimit.fd_max=99999999");
    check!(
        got[0].value == KEYS[Id::HeapMax as usize].min
            && got[1].value == KEYS[Id::FdMax as usize].min
            && got[2].value == KEYS[Id::StackSize as usize].max
            && got[3].value == KEYS[Id::FdMax as usize].max
            && got.iter().all(|set| set.clamped),
        "clamped {got:?}"
    );
    // A repeated key is reported, and the last value wins.
    let outcome = parse("limit.fd_max=100\nlimit.fd_max=200");
    check!(
        matches!(outcome[1], Outcome::Duplicate("fd_max"))
            && matches!(outcome[2], Outcome::Set(Override { value: 200, .. })),
        "duplicate gave {outcome:?}"
    );
    // Pseudo-random lines over a hostile alphabet: no panic, every value in range.
    let alphabet = b"limt.hepa_xfdsckzGMKT0123456789=# -+\t\x7f";
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    for _ in 0..3000 {
        let mut line = String::from("limit.");
        let len = (seed >> 58) as usize;
        for _ in 0..len {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            line.push(alphabet[(seed % alphabet.len() as u64) as usize] as char);
        }
        for set in sets(&line) {
            let key = KEYS[set.id as usize];
            check!(
                (key.min..=key.max).contains(&set.value),
                "{line:?} escaped the range: {set:?}"
            );
        }
    }
    Ok(())
}

/// `bootcfg` skips `limit.` lines, so a bad or repeated limit can never cost
/// the boot its root volume.
pub fn bootcfg_ignores_limit_lines() -> Result<(), String> {
    let text = "root=UUID=0b0c8d3a-5b1e-4a53-9d77-0123456789ab\n\
                limit.heap_max=garbage\nlimit.heap_max=1G\nlimit.what=1\n";
    let cfg = crate::fs::bootcfg::parse(text).map_err(|e| format!("{e}"))?;
    check!(cfg.root.is_some(), "the root was lost");
    Ok(())
}

/// Applying a config changes the live values, records where each came from,
/// clamps, never shrinks the heap ceiling below the mapped heap, and the
/// defaults come back.
pub fn apply_config_and_reset() -> Result<(), String> {
    limits::apply_config("limit.fd_max=100\nlimit.stack_size=1T\nlimit.heap_max=16M\n");
    let fd = limits::fd_max();
    let stack = limits::stack_size();
    let heap = limits::heap_max();
    // 16M is under the heap the boot already mapped on any test machine, so
    // the floor wins and must say so.
    let heap_source = limits::source(Id::HeapMax);
    let sources = (
        limits::source(Id::FdMax),
        limits::source(Id::StackSize),
        limits::source(Id::QuotaUserMemory),
    );
    limits::reset_for_test();
    check!(fd == 100, "fd_max {fd}");
    check!(stack == GIB, "stack_size {stack}");
    check!(
        heap >= mem::heap_stats().total as u64,
        "heap_max {heap} below the mapped heap"
    );
    check!(
        heap == 16 * MIB || heap_source == Source::Clamped,
        "heap_max raised to {heap} but its source is {heap_source:?}"
    );
    check!(
        sources == (Source::Config, Source::Clamped, Source::Default),
        "sources {sources:?}"
    );
    check!(
        limits::fd_max() == 1024 && limits::source(Id::FdMax) == Source::Default,
        "reset left fd_max {}",
        limits::fd_max()
    );
    limits::describe();
    Ok(())
}

/// The derived defaults stay in range for every machine shape, grow with RAM
/// and the screen, and a 4K screen gets room for its buffers.
pub fn derived_defaults_scale() -> Result<(), String> {
    let rams = [
        64 * MIB,
        128 * MIB,
        256 * MIB,
        GIB,
        2 * GIB,
        4 * GIB,
        8 * GIB,
        64 * GIB,
    ];
    let screens = [0u64, 1280 * 720 * 4, 1920 * 1080 * 4, 3840 * 2160 * 4];
    for screen in screens {
        let mut previous: Option<Limits> = None;
        for ram in rams {
            let limits = Limits::for_machine(ram, screen);
            for (value, key) in limits.values().iter().zip(KEYS.iter()) {
                check!(
                    (key.min..=key.max).contains(value),
                    "{} = {value} out of range at {ram} B / {screen} B",
                    key.name
                );
                check!(
                    !key.bytes || value % 4096 == 0,
                    "{} not page aligned",
                    key.name
                );
            }
            check!(
                limits.heap_max >= limits::heap_initial_bytes(ram),
                "heap_max below the initial heap at {ram}"
            );
            if let Some(previous) = previous {
                let grew = previous
                    .values()
                    .iter()
                    .zip(limits.values().iter())
                    .all(|(old, new)| new >= old);
                check!(
                    grew,
                    "a limit shrank as RAM grew to {ram}: {previous:?} -> {limits:?}"
                );
            }
            previous = Some(limits);
            let pool = limits::dma_pool_bytes(ram);
            check!(
                pool <= ram / 8 && pool <= 64 * MIB && pool % 4096 == 0,
                "dma pool {pool} at {ram}"
            );
        }
    }
    let hd = Limits::for_machine(GIB, 1920 * 1080 * 4);
    let uhd = Limits::for_machine(GIB, 3840 * 2160 * 4);
    check!(
        hd.shared_buffer_max >= 2 * 1920 * 1080 * 4,
        "1080p buffers {}",
        hd.shared_buffer_max
    );
    check!(
        uhd.shared_buffer_max >= 3 * 3840 * 2160 * 4,
        "4K buffers {}",
        uhd.shared_buffer_max
    );
    check!(
        Limits::for_machine(256 * MIB, 0).quota_user_memory == 256 * MIB,
        "256 MiB guest quota"
    );
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("limits_parses_values", parses_values),
    ("limits_rejects_hostile_values", rejects_hostile_values),
    (
        "limits_bootcfg_ignores_limit_lines",
        bootcfg_ignores_limit_lines,
    ),
    ("limits_apply_config_and_reset", apply_config_and_reset),
    ("limits_derived_defaults_scale", derived_defaults_scale),
];
