//! The seeded fuzz entry point under plain `cargo test`: random scripts, the
//! structured scripts a coverage-guided run starts from, and the checked-in
//! seed files. A failing seed prints its replay command (`fuzzkit`).

use fuzzkit::for_seeds;

use crate::fuzz::run;

#[test]
fn random_scripts_never_panic() {
    for_seeds("random_scripts_never_panic", |_, rng| {
        let len = rng.below(400) as usize;
        run(&rng.bytes(len));
    });
}

#[test]
fn model_scripts_agree_with_the_volume() {
    for_seeds("model_scripts_agree_with_the_volume", |_, rng| {
        // Mode byte with the low bit clear, then real operations: mkdir first so
        // the directories exist, then a random mix.
        let mut script = std::vec![rng.byte() & !1, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0];
        for _ in 0..rng.range(10, 120) {
            script.extend_from_slice(&[rng.byte(), rng.byte(), rng.byte(), rng.byte()]);
        }
        run(&script);
    });
}

#[test]
fn corrupted_images_never_panic_or_loop() {
    for_seeds("corrupted_images_never_panic_or_loop", |_, rng| {
        let mut script = std::vec![rng.byte() | 1];
        for _ in 0..rng.range(1, 24) {
            // Mostly the superblock, descriptors and bitmaps (offsets under ~16 KiB).
            let near = rng.one_in(4);
            let at = if near {
                rng.below(2048)
            } else {
                rng.below(16_000)
            };
            script.extend_from_slice(&[(at >> 8) as u8, at as u8, rng.byte()]);
        }
        run(&script);
    });
}

#[test]
fn bit_flips_of_a_valid_script_never_panic() {
    let valid: &[u8] = &[
        0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 2, 0, 3, 40, 0, 1, 0, 5, 2, 0, 0, 255, 3,
        0, 0, 9, 7, 0, 0, 0,
    ];
    for_seeds("bit_flips_of_a_valid_script_never_panic", |_, rng| {
        let mut script = valid.to_vec();
        let flips = rng.range(1, 6) as usize;
        rng.flip_bits(&mut script, flips);
        run(&script);
    });
}

#[test]
fn empty_and_tiny_scripts_are_fine() {
    run(&[]);
    for head in 0..=255u8 {
        run(&[head]);
    }
}

/// The checked-in seeds (`fuzz/seeds/ext2fs`) and saved crashes
/// (`fuzz/regressions/ext2fs`), so a fixed bug stays fixed under plain
/// `cargo test`.
#[test]
fn the_checked_in_seeds_replay() {
    let fuzz = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz");
    let seeds = fuzz.join("seeds/ext2fs");
    let Ok(entries) = std::fs::read_dir(&seeds) else {
        return; // a source drop without the fuzz/ tree
    };
    let regressions = std::fs::read_dir(fuzz.join("regressions/ext2fs"))
        .into_iter()
        .flatten();
    let mut seen = 0;
    for entry in entries.chain(regressions) {
        let path = entry.unwrap().path();
        run(&std::fs::read(&path).unwrap());
        seen += 1;
    }
    assert!(seen > 0, "{} holds no seeds", seeds.display());
}

/// A superblock whose inodes span fewer groups than its blocks (which `open`
/// allows) made the repair scan underflow `inodes_count - base`
/// (`fuzz/regressions/ext2fs/repair-inode-groups-underflow`).
#[test]
fn repair_with_fewer_inode_groups_than_block_groups() {
    run(&[
        1, 3, 0x64, 0x30, 0, 4, 0x21, 0xff, 4, 0x2d, 0x22, 3, 0, 4, 0x21, 1,
    ]);
}
