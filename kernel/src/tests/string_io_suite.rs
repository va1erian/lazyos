//! The emulated `rep insw` fixup (`arch::string_io`) and the ATA driver's
//! restart after an aborted transfer.
//!
//! Under WHPX the hypervisor's instruction emulator occasionally fabricates a
//! `#PF` on `rep insw` into a mapped kernel buffer (RSVD-only error code, CR2
//! holding `rax`); init's ELF load died on it in ring 0. These tests pin the
//! three pieces of the fix: the classifier only accepts a fault at the
//! instruction whose destination is mapped writable, the real ISR-to-fixup
//! path resumes cleanly (stand-in faults via the string_io harness), and an
//! aborted ATA run, including one that lost a data word, is reset and
//! re-issued so callers see exactly the bytes a clean read returns.

use super::*;
use crate::arch::string_io::{self, harness};
use crate::block::{ata, BlockDevice, BlockError, SECTOR_SIZE};
use crate::task::signal::FAULT_RIP_INDEX;

/// ATA alternate status: reading it has no side effect, so a stand-in fault
/// that consumes a word from it disturbs nothing.
const ALT_STATUS: u16 = 0x3F6;
const RDI: usize = 9;
const RCX: usize = 12;
const KERNEL_CS: u64 = 0x08;
const USER_CS: u64 = 0x23;

/// xorshift: deterministic pseudo-random inputs for the soaks.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// A page-fault frame as `page_fault_isr` lays it out: 15 registers, error
/// code, RIP, CS, RFLAGS, RSP, SS.
fn frame(rip: u64, cs: u64, rdi: u64, rcx: u64) -> [u64; 21] {
    let mut words = [0u64; 21];
    words[RDI] = rdi;
    words[RCX] = rcx;
    words[FAULT_RIP_INDEX - 1] = 0x8; // the RSVD-only code WHPX injects
    words[FAULT_RIP_INDEX] = rip;
    words[FAULT_RIP_INDEX + 1] = cs;
    words[FAULT_RIP_INDEX + 2] = 0x6;
    words
}

/// Run `recover` on `words`; returns (accepted, resulting rip).
fn recover(words: &mut [u64; 21]) -> (bool, u64) {
    let accepted = string_io::recover(words.as_mut_ptr() as u64);
    (accepted, words[FAULT_RIP_INDEX])
}

/// Only a ring-0 fault at the `rep insw` whose remaining destination is
/// mapped writable is recovered, and recovery moves RIP off the site.
fn recover_classifies_faults() -> Result<(), String> {
    let site = string_io::site();
    let buffer = vec![0u8; 3 * 4096];
    let dst = buffer.as_ptr() as u64 + 100;
    let before = string_io::recovered();

    let mut spurious = frame(site, KERNEL_CS, dst, 256);
    let (accepted, rip) = recover(&mut spurious);
    check!(
        accepted,
        "a fault with a mapped destination was not recovered"
    );
    check!(rip != site && rip != 0, "recovery left rip at {rip:#x}");
    // A page-crossing destination (two pages of a heap buffer) is fine too.
    let (accepted, _) = recover(&mut frame(site, KERNEL_CS, dst + 4000, 256));
    check!(
        accepted,
        "a page-crossing mapped destination was not recovered"
    );

    let rejected: [(&str, [u64; 21]); 6] = [
        (
            "unmapped destination",
            frame(site, KERNEL_CS, harness::UNMAPPED, 256),
        ),
        ("null destination", frame(site, KERNEL_CS, 0x101, 256)),
        ("other rip", frame(site + 2, KERNEL_CS, dst, 256)),
        ("ring 3", frame(site, USER_CS, dst, 256)),
        ("zero count", frame(site, KERNEL_CS, dst, 0)),
        ("wrapping range", frame(site, KERNEL_CS, u64::MAX - 3, 256)),
    ];
    for (what, mut words) in rejected {
        let original = words;
        let (accepted, _) = recover(&mut words);
        check!(!accepted, "{what}: a genuine fault was recovered");
        check!(words == original, "{what}: the frame was modified");
    }
    let delta = string_io::recovered() - before;
    check!(delta == 2, "recovered counter moved by {delta}, expected 2");
    Ok(())
}

/// The real path: a genuine `#PF` on the `rep insw` (unmapped destination,
/// forced fixup) goes through the ISR and resumes at the fixup, which
/// returns "aborted" to Rust with the stack intact; a normal transfer still
/// completes.
fn fixup_resumes_at_caller() -> Result<(), String> {
    let mut words = [0u8; 64];
    // SAFETY: reading the alternate status register has no side effect.
    let ok = unsafe { string_io::insw(ALT_STATUS, &mut words) };
    check!(ok.is_ok(), "a clean transfer reported {ok:?}");
    let canary = core::hint::black_box(0x5afe_c0de_u64);
    // SAFETY: as above; the destination is unmapped, the fixup is forced.
    let aborted = unsafe { harness::raw_insw_unmapped(ALT_STATUS, 32) };
    check!(aborted == 1, "the fixup returned {aborted}, expected 1");
    check!(
        core::hint::black_box(canary) == 0x5afe_c0de,
        "the caller's frame was corrupted"
    );
    Ok(())
}

fn ata(test: &str) -> Result<Option<&'static dyn BlockDevice>, String> {
    super::block_suite::ata_or_skip(test)
}

fn read(device: &dyn BlockDevice, lba: u64, sectors: usize) -> Result<Vec<u8>, String> {
    let mut buf = vec![0u8; sectors * SECTOR_SIZE];
    device
        .read_sectors(lba, &mut buf)
        .map_err(|error| format!("read {lba}+{sectors}: {error:?}"))?;
    Ok(buf)
}

/// An aborted transfer at the start, middle or end of a run is re-issued
/// and returns exactly the clean bytes; aborting every attempt fails with
/// `Io` and leaves the device usable.
fn ata_aborted_run_is_reissued() -> Result<(), String> {
    let Some(device) = ata("string_io_ata_aborted_run_is_reissued")? else {
        return Ok(());
    };
    let clean = read(device, 0, 40)?;
    for skip in [0u64, 1, 17, 39] {
        let retried = ata::retried_runs();
        harness::inject_aborts(skip, 1);
        let got = read(device, 0, 40);
        let pending = harness::pending_aborts();
        harness::inject_aborts(0, 0);
        check!(
            got? == clean,
            "skip {skip}: data differs after a re-issued run"
        );
        check!(pending == 0, "skip {skip}: the abort was never injected");
        let delta = ata::retried_runs() - retried;
        check!(
            delta == 1,
            "skip {skip}: {delta} runs re-issued, expected 1"
        );
    }

    harness::inject_aborts(0, ata::RUN_ATTEMPTS as u64);
    let mut buf = vec![0u8; 8 * SECTOR_SIZE];
    let result = device.read_sectors(0, &mut buf);
    harness::inject_aborts(0, 0);
    check!(
        result == Err(BlockError::Io),
        "a run aborted on every attempt returned {result:?}"
    );
    check!(
        read(device, 0, 40)? == clean,
        "the device did not recover after giving up"
    );
    Ok(())
}

/// Sustained load: random runs (crossing the 128-sector command limit) with
/// random aborts at random points, each compared against a clean reference.
fn ata_abort_soak() -> Result<(), String> {
    let Some(device) = ata("string_io_ata_abort_soak")? else {
        return Ok(());
    };
    const WINDOW: usize = 400;
    let reference = read(device, 0, WINDOW)?;
    let retried = ata::retried_runs();
    let mut expected_retries = 0u64;
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    // Each abort costs a full 2ms-spec channel reset (20,000 port reads,
    // slow under TCG), so the round count is bounded by that, not by taste.
    for round in 0..16 {
        let sectors = 1 + (rng.next() % 200) as usize;
        let lba = rng.next() % (WINDOW - sectors) as u64;
        let aborts = rng.next() % ata::RUN_ATTEMPTS as u64; // always < attempts
        let skip = rng.next() % sectors as u64;
        harness::inject_aborts(skip, aborts);
        let got = read(device, lba, sectors);
        let pending = harness::pending_aborts();
        harness::inject_aborts(0, 0);
        let got = got.map_err(|error| format!("round {round}: {error}"))?;
        let start = lba as usize * SECTOR_SIZE;
        check!(
            got[..] == reference[start..start + sectors * SECTOR_SIZE],
            "round {round}: {lba}+{sectors} with {aborts} aborts after {skip} differs"
        );
        expected_retries += aborts - pending;
    }
    let delta = ata::retried_runs() - retried;
    check!(
        delta == expected_retries,
        "{delta} runs re-issued, {expected_retries} aborts injected"
    );
    Ok(())
}

/// Sustained load on the fixup path itself: many real faults through the
/// ISR, each resuming at the fixup with the same stack pointer, and many
/// classifier calls on random frames, with the counter exact throughout.
fn fixup_soak() -> Result<(), String> {
    let before = string_io::recovered();
    let rsp = || {
        let value: u64;
        // SAFETY: reads the stack pointer into a register; no side effect.
        unsafe { core::arch::asm!("mov {}, rsp", out(reg) value, options(nomem, nostack)) };
        value
    };
    let start = rsp();
    for round in 0..20_000 {
        // SAFETY: see `fixup_resumes_at_caller`.
        let aborted = unsafe { harness::raw_insw_unmapped(ALT_STATUS, 1 + round % 256) };
        check!(aborted == 1, "round {round}: fixup returned {aborted}");
        check!(rsp() == start, "round {round}: rsp drifted to {:#x}", rsp());
    }
    // Destinations fall wholly inside a heap buffer (mapped), in the top
    // user pages or in the first two pages (never mapped in the kernel task).
    let buffer = vec![0u8; 4096];
    let mapped = buffer.as_ptr() as u64;
    let mut rng = Rng(0x0123_4567_89ab_cdef);
    let mut accepted_count = 0u64;
    for round in 0..100_000 {
        let at_site = rng.next().is_multiple_of(2);
        let rip = if at_site {
            string_io::site()
        } else {
            rng.next() | 1
        };
        let words = 1 + rng.next() % 256;
        let (dst, mapped_dst) = match rng.next() % 3 {
            0 => (mapped + rng.next() % (4096 - words * 2 + 1), true),
            1 => (harness::UNMAPPED + rng.next() % 4096, false),
            _ => (rng.next() & 0xFFF, false),
        };
        let (accepted, _) = recover(&mut frame(rip, KERNEL_CS, dst, words));
        check!(
            accepted == (at_site && mapped_dst),
            "round {round}: rip {rip:#x} dst {dst:#x} words {words}: accepted={accepted}"
        );
        accepted_count += u64::from(accepted);
    }
    let delta = string_io::recovered() - before;
    check!(
        delta == 20_000 + accepted_count,
        "counter moved by {delta}, expected {}",
        20_000 + accepted_count
    );
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "string_io_recover_classifies_faults",
        recover_classifies_faults,
    ),
    ("string_io_fixup_resumes_at_caller", fixup_resumes_at_caller),
    (
        "string_io_ata_aborted_run_is_reissued",
        ata_aborted_run_is_reissued,
    ),
    ("string_io_ata_abort_soak", ata_abort_soak),
    ("string_io_fixup_soak", fixup_soak),
];
