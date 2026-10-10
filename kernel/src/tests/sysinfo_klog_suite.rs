//! The boot-log and program-output ops of syscall 14 (`LAZYOS_DBGD=1`
//! kernels only, `cfg(lazyos_dbgd)`): what `dbgd` reads. Correctness of
//! the buffer contract and a soak that reads while the log wraps.

use super::sysinfo_suite::{failed, fresh, in_space, SPACE};
use super::*;
use crate::sysinfo;

/// Read the boot-log ring through op 2: its total and the bytes.
fn klog_read(capacity: u64) -> Result<(u64, alloc::vec::Vec<u8>), String> {
    in_space(|| {
        let code = process::dispatch_for_test(14, sysinfo::op::KLOG, SPACE, capacity);
        check!((code as i64) >= 8, "klog op returned {code:#x}");
        let mut raw = alloc::vec![0u8; code as usize];
        // Safety: the scratch pages are mapped readable while installed.
        unsafe { core::ptr::copy_nonoverlapping(SPACE as *const u8, raw.as_mut_ptr(), raw.len()) };
        let total = u64::from_le_bytes(raw[..8].try_into().unwrap());
        Ok((total, raw[8..].to_vec()))
    })
}

/// op 2 (`dbgd`'s boot-log read): the total and the newest bytes agree with
/// what the ring holds, refuse a null or too-small buffer, and a marker
/// written to the log shows up at the end with the total advanced by it.
pub fn klog_op_contract() -> Result<(), String> {
    fresh();
    check!(
        process::dispatch_for_test(14, sysinfo::op::KLOG, 0, 64) == failed(14),
        "a null klog buffer was not refused with -EFAULT"
    );
    let small = in_space(|| Ok(process::dispatch_for_test(14, sysinfo::op::KLOG, SPACE, 4)))?;
    check!(
        small == failed(22),
        "a 4-byte klog buffer returned {small:#x}"
    );

    let (before, _) = klog_read(4096)?;
    let marker = "KLOGOP:MARKER 0123456789
";
    crate::serial_println!("KLOGOP:MARKER 0123456789");
    let (after, tail) = klog_read(4096)?;
    check!(
        after >= before + marker.len() as u64,
        "total went {before} -> {after}, not past the marker"
    );
    check!(
        tail.windows(marker.len()).any(|w| w == marker.as_bytes()),
        "the marker is not in the newest 4 KiB"
    );
    let (_, small_tail) = klog_read(8 + 16)?;
    check!(
        small_tail.len() == 16,
        "a 24-byte buffer returned {} data bytes",
        small_tail.len()
    );
    Ok(())
}

/// op 3 (what programs wrote): a marker pushed to the program ring is read
/// back, and never lands in the boot log.
pub fn program_log_op_contract() -> Result<(), String> {
    fresh();
    crate::klog::push_program(
        b"PROGOP:MARKER 42
",
    );
    let code = in_space(|| {
        Ok(process::dispatch_for_test(
            14,
            sysinfo::op::PROGRAM_LOG,
            SPACE,
            4096,
        ))
    })?;
    check!((code as i64) > 8, "program log op returned {code:#x}");
    let (_, boot) = klog_read(crate::klog::CAPACITY as u64 + 8)?;
    check!(
        !boot.windows(14).any(|w| w == b"PROGOP:MARKER "),
        "program output leaked into the boot log"
    );
    Ok(())
}

/// Many reads while the log keeps wrapping: the total only grows, and the
/// data never exceeds what was asked for (no allocation leak per read).
pub fn soak_klog_reads() -> Result<(), String> {
    fresh();
    let slab_before = mem::slab::stats().live_bytes;
    let frames_before = mem::frame_stats().live();
    let mut last = 0;
    for round in 0..64 {
        crate::serial_println!("KLOGOP:SOAK {round:04} {}", "x".repeat(900));
        let (total, data) = klog_read(8 + 2048)?;
        check!(total >= last, "round {round}: total went backwards");
        check!(data.len() <= 2048, "round {round}: {} bytes", data.len());
        last = total;
    }
    check!(
        mem::slab::stats().live_bytes == slab_before,
        "slab grew across klog reads"
    );
    check!(
        mem::frame_stats().live() == frames_before,
        "frames grew across klog reads"
    );
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("sysinfo_klog_op_contract", klog_op_contract),
    ("sysinfo_program_log_op_contract", program_log_op_contract),
    ("sysinfo_soak_klog_reads", soak_klog_reads),
];
