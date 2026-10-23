//! The per-process memory op of syscall 14 (`sysinfo::procmem`, op 5): the
//! buffer contract, the header and the rows it leaves out, and a soak that
//! reads it again and again, where a scratch table that is never freed would
//! show as frame growth.

use super::sysinfo_suite::{failed, fresh, in_space, SPACE};
use super::*;
use crate::sysinfo::{self, procmem};

/// The block through the syscall entry, as words.
fn read_block(capacity: u64) -> Result<alloc::vec::Vec<u64>, String> {
    in_space(|| {
        let code = process::dispatch_for_test(14, sysinfo::op::PROC_MEM, SPACE, capacity);
        check!((code as i64) >= 0, "op 5 returned {code:#x}");
        let words = code as usize / 8;
        let mut out = alloc::vec![0u64; words];
        for (index, word) in out.iter_mut().enumerate() {
            // Safety: the scratch pages are mapped readable while installed.
            *word = unsafe { core::ptr::read_volatile((SPACE as *const u64).add(index)) };
        }
        Ok(out)
    })
}

/// A null buffer is `-EFAULT`, one too small for the header `-E2BIG`; the
/// header names the version and the row size, and a buffer with room for no
/// row gets the header alone, with the number of processes that exist.
pub fn procmem_abi_contract() -> Result<(), String> {
    fresh();
    check!(
        process::dispatch_for_test(14, sysinfo::op::PROC_MEM, 0, 4096) == failed(14),
        "a null buffer was not refused with -EFAULT"
    );
    let short = in_space(|| {
        Ok(process::dispatch_for_test(
            14,
            sysinfo::op::PROC_MEM,
            SPACE,
            (procmem::HEADER_WORDS * 8 - 8) as u64,
        ))
    })?;
    check!(short == failed(7), "a short buffer returned {short:#x}");

    let header_only = read_block((procmem::HEADER_WORDS * 8) as u64)?;
    check!(
        header_only.len() == procmem::HEADER_WORDS,
        "a buffer for no row got {} words",
        header_only.len()
    );
    check!(
        header_only[0] == procmem::VERSION && header_only[1] == procmem::ROW_WORDS as u64,
        "header says version {} with {} words a row",
        header_only[0],
        header_only[1]
    );
    check!(
        header_only[2] == 0 && header_only[3] == task::space_owners().len() as u64,
        "header says {} rows written of {}",
        header_only[2],
        header_only[3]
    );
    Ok(())
}

/// The rows written are the rows asked for, whole: a block is the header plus
/// `rows` complete rows, no row holds more private pages than resident ones or
/// more proportional bytes than its resident pages, and its classes add up.
pub fn procmem_rows_are_whole() -> Result<(), String> {
    fresh();
    let owners = task::space_owners().len();
    let block = read_block(((procmem::HEADER_WORDS + owners * procmem::ROW_WORDS) * 8) as u64)?;
    let rows = block[2] as usize;
    check!(
        rows == owners && block.len() == procmem::HEADER_WORDS + rows * procmem::ROW_WORDS,
        "{rows} rows written, {owners} processes, {} words",
        block.len()
    );
    for row in block[procmem::HEADER_WORDS..].chunks(procmem::ROW_WORDS) {
        let (resident, private, pss) = (row[2], row[3], row[4]);
        check!(
            private <= resident && pss <= resident * 4096,
            "pid {}: resident {resident}, private {private}, pss {pss}",
            row[0]
        );
        let classes: u64 = row[7..7 + crate::mem::accounting::CLASSES].iter().sum();
        check!(
            classes == resident,
            "pid {}: the classes hold {classes} of {resident} pages",
            row[0]
        );
    }
    Ok(())
}

/// Soak: 300 reads in a row leak neither frames nor task slots.
pub fn soak_procmem_reads_leak_nothing() -> Result<(), String> {
    fresh();
    let capacity = ((procmem::HEADER_WORDS + 32 * procmem::ROW_WORDS) * 8) as u64;
    read_block(capacity)?;
    let before = mem::frame_stats().live();
    for round in 0..300 {
        let block = read_block(capacity).map_err(|error| format!("round {round}: {error}"))?;
        check!(block[0] == procmem::VERSION, "round {round}: bad header");
    }
    let after = mem::frame_stats().live();
    check!(
        after <= before,
        "frames live went {before} -> {after} over 300 reads"
    );
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("sysinfo_procmem_abi_contract", procmem_abi_contract),
    ("sysinfo_procmem_rows_are_whole", procmem_rows_are_whole),
    (
        "sysinfo_soak_procmem_reads_leak_nothing",
        soak_procmem_reads_leak_nothing,
    ),
];
