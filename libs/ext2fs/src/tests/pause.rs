//! The pause hook (`Ext2::set_pause`): long operations call it between their
//! steps, so a kernel can let interrupts in, and nothing changes in what they
//! do.

use super::*;
use crate::Owner;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicUsize, Ordering};

/// Deleting a large file and committing its frees pause along the way, and
/// the volume is exactly as without the hook.
#[test]
fn deleting_a_large_file_pauses_between_steps() {
    let io = formatted(64 << 20, 4096);
    let mut fs = Ext2::open_cached(Box::new(io.clone()), clock, crate::CacheConfig::heap(2048))
        .expect("open cached");
    let pauses = Arc::new(AtomicUsize::new(0));
    let counter = pauses.clone();
    fs.set_pause(Box::new(move || {
        counter.fetch_add(1, Ordering::Relaxed);
    }));
    fs.create("/big", 0o644, Owner::ROOT).unwrap();
    let data = std::vec![0xA5u8; 24 << 20];
    fs.write("/big", 0, &data).unwrap();
    fs.flush().unwrap();
    let free_before = fs.free_blocks().unwrap();
    let after_write = pauses.load(Ordering::Relaxed);
    // 6144 blocks written, a pause every 16.
    assert!(
        after_write >= 6144 / 16 - 1,
        "writing paused {after_write} times"
    );
    fs.unlink("/big").unwrap();
    let after_unlink = pauses.load(Ordering::Relaxed);
    // 6144 data blocks: six single-indirect tables' worth, one pause each.
    assert!(
        after_unlink >= after_write + 5,
        "unlink paused {} times",
        after_unlink - after_write
    );
    fs.flush().unwrap();
    let after_commit = pauses.load(Ordering::Relaxed);
    assert!(
        after_commit >= after_unlink + 6144 / 256,
        "the commit paused {} times",
        after_commit - after_unlink
    );
    assert!(fs.free_blocks().unwrap() > free_before + 6000);
    drop(fs);
    assert_clean(&io);
}
