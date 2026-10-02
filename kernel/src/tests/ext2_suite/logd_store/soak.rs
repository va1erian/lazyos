//! Soak: 200 000 records over 40 sources through the store on ext2.

use super::*;

/// Records appended.
const RECORDS: u64 = 200_000;
/// Distinct sources.
const SOURCES: u64 = 40;
/// Volume size in 1 KiB blocks.
const BLOCKS: u32 = 2048;
/// Scaled limits (the test volume lives in the kernel heap): 40 sources of
/// up to three 6 KiB files would need 720 KiB, so the 160 KiB budget is what
/// binds, and rotations and budget sheds both run thousands of times.
const LIMITS: Limits = Limits {
    file_cap: 6 * 1024,
    budget: 160 * 1024,
};

fn heap_in_use() -> usize {
    crate::mem::slab::stats().live_bytes + crate::mem::heap_stats().used
}

/// After 200 000 records across 40 sources (and a remount half-way): the
/// journals stay within the budget at every check, no file exceeds the cap,
/// every file verifies, the volume's bitmaps and counters agree, and the heap
/// is back where it started once the store and the mount are gone.
pub fn logd_store_soak_200k_records() -> Result<(), String> {
    task::register_kernel();
    let disk = volume(BLOCKS)?;
    let baseline = heap_in_use();
    let mut seq = 0u64;
    for boot_id in 1..=2u64 {
        let (fs, mut store) = boot(disk, boot_id, LIMITS)?;
        for index in 0..RECORDS / 2 {
            seq += 1;
            let topic = format!("system/events/s{:02}/state", index % SOURCES);
            let detail = format!("boot={boot_id} n={index}");
            store
                .append(seq, index, &topic, &detail)
                .map_err(fs_error)?;
            store.tick(index).map_err(fs_error)?;
            check!(
                store.ledger().total() <= LIMITS.budget,
                "record {seq}: the ledger holds {}",
                store.ledger().total()
            );
        }
        check!(
            store.persisted() + store.pending() == RECORDS / 2,
            "boot {boot_id}: {} persisted + {} pending",
            store.persisted(),
            store.pending()
        );
        shut_down(fs, store)?;
        check_volume(disk, BLOCKS)?;
    }
    let after = heap_in_use();
    check!(
        after <= baseline + 16 * 1024,
        "the heap grew from {baseline} to {after}"
    );

    let files = journals(disk)?;
    let mut total = 0u64;
    for (name, data) in &files {
        check!(parse_name(name).is_some(), "stray file {name}");
        check!(
            data.len() as u64 <= LIMITS.file_cap,
            "{name} is {} bytes",
            data.len()
        );
        verified(name, data)?;
        total += data.len() as u64;
    }
    check!(total <= LIMITS.budget, "/logs holds {total} bytes");
    check!(
        files.len() as u64 <= SOURCES * 3,
        "{} files for {SOURCES} sources",
        files.len()
    );
    let newest = files
        .iter()
        .find(|(name, _)| name == "s39.log")
        .map(|(_, data)| {
            core::str::from_utf8(data)
                .unwrap_or("")
                .contains("boot=2 n=99999\t")
        });
    check!(newest == Some(true), "the last record is not in s39.log");
    release(disk);
    Ok(())
}
