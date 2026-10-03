//! Epoll self/cyclic and bottom-up registration limits, the
//! bounded service-name intern table, the VFS parent search bit,
//! and ext2 short-write/failed-write integrity (including a
//! soak).

use super::*;

/// An epoll instance may not watch itself, close a cycle, or stack past
/// `EPOLL_MAX_NESTS`; readiness recurses through nested instances, so any of
/// those used to overflow the kernel stack (and leaked the cycle).
pub fn epoll_rejects_self_and_cyclic_registration() -> Result<(), String> {
    const EPOLL_CTL_ADD: u64 = 1;
    const EPOLLIN: u32 = 1;
    const EINVAL_RET: u64 = (-22i64) as u64;
    const ELOOP_RET: u64 = (-40i64) as u64;
    fn event() -> [u8; 12] {
        let mut buf = [0u8; 12];
        buf[..4].copy_from_slice(&EPOLLIN.to_le_bytes());
        buf
    }
    fn create() -> u64 {
        process::linux::dispatch_for_test(291, 0, 0, 0)
    }
    fn add(epfd: u64, fd: u64) -> u64 {
        process::linux::dispatch_args_for_test(
            233,
            epfd,
            EPOLL_CTL_ADD,
            fd,
            event().as_ptr() as u64,
        )
    }
    fresh()?;
    for fd in 3..task::harness::fd_table_len() {
        let _ = task::fd_close(fd);
    }

    let a = create();
    let b = create();
    check!((a as i64) > 0 && (b as i64) > 0, "epoll_create1 failed");
    check!(
        add(a, a) == EINVAL_RET,
        "an epoll registered itself (kernel would recurse forever)"
    );
    check!(add(a, b) == 0, "nesting one epoll in another failed");
    check!(
        add(b, a) == ELOOP_RET,
        "closing an epoll cycle was accepted"
    );
    // With the cycle refused, waiting terminates.
    let mut out = [0u8; 12];
    let ready = process::linux::dispatch_args_for_test(232, a, out.as_mut_ptr() as u64, 1, 0);
    check!(
        ready == 0,
        "epoll_wait on the nested pair returned {ready:#x}"
    );

    // A chain of six is the limit; a seventh instance on top is refused.
    let mut chain = Vec::new();
    for _ in 0..7 {
        let fd = create();
        check!((fd as i64) > 0, "epoll_create1 failed");
        chain.push(fd);
    }
    for pair in (0..5).rev() {
        check!(
            add(chain[pair], chain[pair + 1]) == 0,
            "nesting level {pair} was refused too early"
        );
    }
    check!(
        add(chain[6], chain[0]) == EINVAL_RET,
        "an epoll chain deeper than EPOLL_MAX_NESTS was accepted"
    );
    for fd in 3..task::harness::fd_table_len() {
        let _ = task::fd_close(fd);
    }
    Ok(())
}

/// The depth check must bound the *total* chain length, not just how far
/// it extends below the candidate being added: growing the chain by
/// always appending a fresh, empty instance at the tail
/// (`epoll_ctl(chain[i], ADD, chain[i+1])` for increasing `i`) presents
/// `check_nest` with an empty candidate every single time, so a check that
/// only walks downward from the candidate never sees how deep the chain
/// above it already is and never rejects the growth (CodeRabbit feedback
/// on this PR: unbounded kernel stack recursion, CWE-674).
pub fn epoll_rejects_bottom_up_growth() -> Result<(), String> {
    const EPOLL_CTL_ADD: u64 = 1;
    const EPOLLIN: u32 = 1;
    const EINVAL_RET: u64 = (-22i64) as u64;
    fn event() -> [u8; 12] {
        let mut buf = [0u8; 12];
        buf[..4].copy_from_slice(&EPOLLIN.to_le_bytes());
        buf
    }
    fn create() -> u64 {
        process::linux::dispatch_for_test(291, 0, 0, 0)
    }
    fn add(epfd: u64, fd: u64) -> u64 {
        process::linux::dispatch_args_for_test(
            233,
            epfd,
            EPOLL_CTL_ADD,
            fd,
            event().as_ptr() as u64,
        )
    }
    fresh()?;
    for fd in 3..task::harness::fd_table_len() {
        let _ = task::fd_close(fd);
    }

    // chain[0] watches chain[1], chain[1] watches chain[2], ...: each
    // `add` call is presented with a brand new, empty `fd` (nothing below
    // it yet), exactly the shape that only checking "below" always allows.
    let mut chain = Vec::new();
    for _ in 0..7 {
        let fd = create();
        check!((fd as i64) > 0, "epoll_create1 failed");
        chain.push(fd);
    }
    for i in 0..5 {
        check!(
            add(chain[i], chain[i + 1]) == 0,
            "growing the chain to depth {} was refused too early",
            i + 1
        );
    }
    check!(
        add(chain[5], chain[6]) == EINVAL_RET,
        "a bottom-up chain grew past EPOLL_MAX_NESTS unchecked"
    );
    // With the growth refused, the existing (legal) 5-edge chain still
    // terminates instead of recursing.
    let mut out = [0u8; 12];
    let ready =
        process::linux::dispatch_args_for_test(232, chain[0], out.as_mut_ptr() as u64, 1, 0);
    check!(
        ready == 0,
        "epoll_wait on the legal chain returned {ready:#x}"
    );
    for fd in 3..task::harness::fd_table_len() {
        let _ = task::fd_close(fd);
    }
    Ok(())
}

/// Two clients calling one service at the same time are not a callback
/// cycle: only a call in the *opposite* direction of an open transaction is
/// `Deadlock`. The channel-wide check used to refuse every second
/// simultaneous client (`clippaste: fatal: the call would deadlock`).
pub fn concurrent_clients_are_not_a_deadlock() -> Result<(), String> {
    fresh()?;
    let (a, b) = channels::create().map_err(|e| e.message().to_string())?;
    let request = {
        let parcel = Parcel {
            header: Header {
                version: VERSION,
                flags: flags::SYNC,
                interface_id: 0x77,
                method: 7,
                txn_id: 0,
                reply_to: 0,
                deadline_ns: 0,
            },
            body: Vec::new(),
            handles: Vec::new(),
            buffers: Vec::new(),
        };
        let mut bytes = Vec::new();
        parcel.encode(&mut bytes).map_err(|e| e.message())?;
        bytes
    };
    let object = handles::get_for_task(task::KERNEL_TASK, a)
        .map_err(|e| e.message().to_string())?
        .object_id;
    let first = channels::begin_call(a, 7, &request, None)
        .map_err(|e| format!("the first call failed: {}", e.message()))?;
    task::wake_task(task::KERNEL_TASK);
    let _ = task::harness::take_wake_reason(task::KERNEL_TASK);

    // A second client resolved the same endpoint side.
    let client = task::spawn_fork().map_err(to_string)?;
    let same_side = handles::open_for_task(
        client,
        handles::HandleKind::Channel,
        handles::rights::ALL,
        object,
    )
    .map_err(|e| e.message().to_string())?;
    task::harness::switch_current(client);
    let second = channels::begin_call(same_side, 7, &request, None);
    task::wake_task(client);
    let _ = task::harness::take_wake_reason(client);
    task::harness::switch_current(task::KERNEL_TASK);
    check!(
        second.is_ok(),
        "a second concurrent client was refused: {:?}",
        second.err().map(|e| e.message())
    );

    // The genuine cycle is still refused: the service calling back into a
    // client whose call is open.
    check!(
        channels::begin_call(b, 7, &request, None) == Err(channels::Error::Deadlock),
        "a callback cycle was not refused"
    );
    let _ = first;
    fresh()
}

/// The service name intern table is case-sensitive and bounded: `a` and `A`
/// are two programs, one basename reached through different directories is
/// one name, and invented names stop leaking a string each at the cap.
pub fn intern_service_names_are_bounded() -> Result<(), String> {
    // Before the flood below fills the table.
    let lower = process::intern_service_name_for_test("/system/bin/a");
    let upper = process::intern_service_name_for_test("A");
    check!(
        lower == "a" && upper == "A",
        "`a` and `A` interned as {lower:?} and {upper:?}"
    );
    check!(
        core::ptr::eq(lower, process::intern_service_name_for_test("./a")),
        "one basename interned twice"
    );
    let mut leaked = 0usize;
    let mut fallback = 0usize;
    for i in 0..400 {
        let name = format!("./spelling{i}.elf");
        let interned = process::intern_service_name_for_test(&name);
        if interned == &name[2..] {
            leaked += 1;
        } else {
            fallback += 1;
            check!(
                interned == "service",
                "an overflow name was {interned:?}, not the shared fallback"
            );
        }
    }
    check!(
        leaked <= 64 && fallback >= 400 - 64,
        "{leaked} of 400 distinct names were leaked (cap is 64)"
    );
    Ok(())
}

/// Creating, removing or renaming an entry needs write *and* search
/// permission on the parent directory, as on Linux.
pub fn vfs_parent_directory_needs_search_bit() -> Result<(), String> {
    let root = Id::ROOT;
    let user = Id::new(1000, 1000);
    let mut vfs = Vfs::new();
    vfs.mount(
        "/",
        Arc::new(RamFs::new()),
        crate::fs::vfs::MountFlags::default(),
    )
    .map_err(|e| e.message())?;
    vfs.set_umask(0);
    // Others may write but not search `/wo`; they may do both in `/wx`.
    vfs.mkdir(root, "/wo", 0o722).map_err(|e| e.message())?;
    vfs.mkdir(root, "/wx", 0o733).map_err(|e| e.message())?;
    vfs.create(root, "/wo/g", 0o666).map_err(|e| e.message())?;

    check!(
        matches!(vfs.create(user, "/wo/f", 0o644), Err(FsError::Access)),
        "create in a write-only directory was allowed"
    );
    check!(
        vfs.mkdir(user, "/wo/d", 0o755).is_err(),
        "mkdir in a write-only directory was allowed"
    );
    check!(
        vfs.unlink(user, "/wo/g") == Err(FsError::Access),
        "unlink in a write-only directory was allowed"
    );
    check!(
        vfs.rename(user, "/wo/g", "/wx/h") == Err(FsError::Access),
        "rename out of a write-only directory was allowed"
    );
    check!(
        vfs.create(user, "/wx/f", 0o644).is_ok(),
        "create in a write+search directory was refused"
    );
    check!(
        vfs.unlink(user, "/wx/f").is_ok(),
        "unlink in a write+search directory was refused"
    );
    Ok(())
}

/// A write that runs out of blocks part-way persists what landed (the inode
/// is written back) instead of leaking every block it allocated,
/// and an owner whose ids do not fit ext2's 16-bit fields is refused rather
/// than truncated (uid 65536 would become root).
pub fn ext2_short_write_persists_and_owner_is_checked() -> Result<(), String> {
    task::register_kernel();
    let (fs, mut vfs, _disk) = ext2_suite::mounted(1024, 512)?;
    let root = Id::ROOT;
    vfs.create(root, "/big", 0o644).map_err(|e| e.message())?;
    let free_before = fs.free_blocks().map_err(|e| e.message())?;

    // The 512 KiB volume cannot hold 600 KiB, so the write runs the volume dry
    // well past the single-indirect range (268 KiB at 1 KiB blocks).
    let data = vec![0x5Au8; 600 * 1024];
    let written = vfs
        .write(root, "/big", 0, &data)
        .map_err(|e| format!("a short write failed outright: {}", e.message()))?;
    check!(
        written > 268 * 1024 && written < data.len() && written % 1024 == 0,
        "the write reported {written} bytes, expected a whole-block short write"
    );
    check!(
        fs.free_blocks().map_err(|e| e.message())? == 0,
        "the short write stopped with blocks still free"
    );
    let meta = vfs.stat(root, "/big").map_err(|e| e.message())?;
    check!(
        meta.size == written as u64,
        "the inode size is {} after a {written}-byte write",
        meta.size
    );
    let back = vfs.read_file(root, "/big").map_err(|e| e.message())?;
    check!(
        back.len() == written && back.iter().all(|byte| *byte == 0x5A),
        "the persisted bytes do not read back"
    );
    check!(
        vfs.write(root, "/big", written as u64, &data[..1024]) == Err(FsError::NoSpace),
        "a write on a full volume did not report NoSpace"
    );
    vfs.unlink(root, "/big").map_err(|e| e.message())?;
    let free_after = fs.free_blocks().map_err(|e| e.message())?;
    check!(
        free_after == free_before,
        "blocks leaked across a short write: {free_before} -> {free_after}"
    );

    // 16-bit owner ids.
    let owner = Id::new(65536, 100);
    check!(
        Filesystem::create(&*fs, "evil", 0o644, owner) == Err(FsError::Invalid),
        "uid 65536 was truncated into uid 0"
    );
    check!(
        Filesystem::mkdir(&*fs, "evildir", 0o755, Id::new(0, 70_000)) == Err(FsError::Invalid),
        "gid 70000 was truncated"
    );
    Ok(())
}

/// A write-side I/O error on a freshly allocated block must not leave the
/// file able to read that block's *previous* owner's bytes as though it
/// were a still-unwritten hole (CWE-200: found while addressing CodeRabbit
/// feedback on this PR).
///
/// Sequence: poison two blocks with a recognizable pattern, free them, then
/// have a fresh file reuse the same two blocks (ext2's allocator always
/// grabs the lowest free bit, so this is deterministic) across two writes:
/// the first spans both blocks and its second block's device write fails;
/// the second, independent write lands two blocks further out, which only
/// raises the file's declared size -- it does not revisit the first
/// write's failed block. Unfixed, that block's pointer was linked into the
/// inode before its data write was attempted, so once the size covers it,
/// reading it returns the poison pattern instead of zeros. Fixed
/// (`Ext2::ensure_block` zeroes a fresh block on disk before linking it,
/// and never links it at all if that zero-write fails), the block is never
/// linked, so it reads as a zero-filled hole like any other gap.
pub fn ext2_failed_write_does_not_expose_a_stale_block() -> Result<(), String> {
    task::register_kernel();
    let (_fs, mut vfs, disk) = ext2_suite::mounted(1024, 512)?;
    let root = Id::ROOT;

    // Poison one block, then free it. ext2's allocator always grabs the
    // lowest free bit, so /victim's first block deterministically reuses
    // this exact (now poisoned) block.
    vfs.create(root, "/poison", 0o644)
        .map_err(|e| e.message())?;
    vfs.write(root, "/poison", 0, &[0x77u8; 1024])
        .map_err(|e| e.message())?;
    vfs.unlink(root, "/poison").map_err(|e| e.message())?;

    vfs.create(root, "/victim", 0o644)
        .map_err(|e| e.message())?;

    // `alloc_block` itself issues three writes (block bitmap, group
    // descriptor, superblock) before the freshly allocated block is
    // touched at all; the fourth write is the first one that names the
    // block itself -- the (only) data write with no fix, the added
    // zero-fill with the fix. Failing that one is what actually
    // distinguishes the two builds: failing writes 1-3 instead fails
    // allocation itself cleanly in both and proves nothing (an earlier
    // version of this test did that, by mistake, and passed on both).
    disk.fail_nth_write(4);
    check!(
        vfs.write(root, "/victim", 0, &[0xAAu8; 10]).is_err(),
        "a write whose data write failed reported success"
    );

    // Second, independent write: one block further out. It only extends
    // the file's declared size; it does not touch block 0 again.
    vfs.write(root, "/victim", 2 * 1024, b"end")
        .map_err(|e| e.message())?;

    // Block 0's whole byte range must now read as zero: a hole, not the
    // poison pattern left over from the block's previous owner. Unfixed,
    // `ensure_block` had already linked the (untouched, still poisoned)
    // block into the inode before the failed write ran, and `write_inode`
    // persisted that link regardless of the failure.
    let contents = vfs.read_file(root, "/victim").map_err(|e| e.message())?;
    let hole = &contents[..1024];
    check!(
        hole.iter().all(|byte| *byte == 0),
        "block 0 reads as {:?}.. instead of a zero hole",
        &hole[..hole.len().min(8)]
    );
    Ok(())
}

/// Soak: 200 short writes / unlinks leave the block bitmap exactly as it
/// started.
pub fn soak_ext2_short_writes_do_not_leak() -> Result<(), String> {
    task::register_kernel();
    let (fs, mut vfs, _disk) = ext2_suite::mounted(1024, 512)?;
    let root = Id::ROOT;
    let free_before = fs.free_blocks().map_err(|e| e.message())?;
    let data = vec![0xC3u8; 300 * 1024];
    for round in 0..200 {
        vfs.create(root, "/soak", 0o644)
            .map_err(|e| format!("round {round}: create {}", e.message()))?;
        let _ = vfs.write(root, "/soak", 0, &data);
        vfs.unlink(root, "/soak")
            .map_err(|e| format!("round {round}: unlink {}", e.message()))?;
        let free = fs.free_blocks().map_err(|e| e.message())?;
        check!(
            free == free_before,
            "round {round}: {free_before} free blocks became {free}"
        );
    }
    Ok(())
}
