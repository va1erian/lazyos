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

/// The row layout, from known values: each word lands where the ABI says, a buffer with room for
/// fewer rows than there are gets the first ones whole and the true total in the header.
pub fn procmem_row_layout() -> Result<(), String> {
    use crate::mem::accounting::Use;
    use crate::task::SpaceOwner;
    let owners = [
        SpaceOwner {
            pid: 7,
            pml4: 0x1000,
            threads: 3,
        },
        SpaceOwner {
            pid: 9,
            pml4: 0x2000,
            threads: 1,
        },
    ];
    let uses = [
        Use {
            rss: 10,
            private: 4,
            pss: 9000,
            tables: 5,
            class_rss: [1, 2, 3, 2, 2],
            class_pss: [100, 200, 300, 400, 500],
        },
        Use {
            rss: 1,
            private: 1,
            pss: 4096,
            tables: 4,
            ..Use::default()
        },
    ];
    let words = procmem::encode(&owners, &uses, &[1 << 20, 2 << 20], 77, 8);
    check!(
        words.len() == procmem::HEADER_WORDS + 2 * procmem::ROW_WORDS,
        "{} words for two rows",
        words.len()
    );
    check!(
        words[..procmem::HEADER_WORDS] == [procmem::VERSION, procmem::ROW_WORDS as u64, 2, 2, 77],
        "header {:?}",
        &words[..procmem::HEADER_WORDS]
    );
    let row = &words[procmem::HEADER_WORDS..procmem::HEADER_WORDS + procmem::ROW_WORDS];
    check!(
        row[..7] == [7, 3, 10, 4, 9000, 5, 1 << 20]
            && row[7..12] == [1, 2, 3, 2, 2]
            && row[12..17] == [100, 200, 300, 400, 500],
        "first row {row:?}"
    );
    let one = procmem::encode(&owners, &uses, &[0, 0], 77, 1);
    check!(
        one[2] == 1 && one[3] == 2 && one.len() == procmem::HEADER_WORDS + procmem::ROW_WORDS,
        "one row of two: header {:?}, {} words",
        &one[..5],
        one.len()
    );
    let none = procmem::encode(&owners, &[], &[], 77, 0);
    check!(
        none.len() == procmem::HEADER_WORDS && none[2] == 0 && none[3] == 2,
        "no room for a row: {none:?}"
    );
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
    ("sysinfo_procmem_row_layout", procmem_row_layout),
    (
        "sysinfo_soak_procmem_reads_leak_nothing",
        soak_procmem_reads_leak_nothing,
    ),
];
