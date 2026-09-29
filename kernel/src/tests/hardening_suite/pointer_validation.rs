//! Native and Linux-ABI syscalls reject kernel pointers, valid
//! user buffers still work, edge cases of the validated
//! primitives, and a copy-validation soak.

use super::*;

/// Native syscalls that copy data out to a caller pointer refuse a kernel
/// address with `-EFAULT` and leave that memory alone. Before the fix every
/// one of them wrote through the pointer: an arbitrary kernel write from
/// ring 3.
pub fn native_syscalls_reject_kernel_pointers() -> Result<(), String> {
    fresh()?;
    let _strict = Strict::on();

    let mut cred = canary(40);
    let code = process::dispatch_for_test(10, cred_op::GET, u64::MAX, cred.as_mut_ptr() as u64);
    check!(code == failed(EFAULT), "creds get -> {code:#x}");
    untouched(&cred, "creds get")?;

    // A credential block *read* from kernel memory is refused too: the
    // caller must not be able to make the gate consume arbitrary kernel
    // bytes as a credential.
    let block = Cred::ROOT.to_words();
    let code = process::dispatch_for_test(10, cred_op::SET, u64::MAX, block.as_ptr() as u64);
    check!(
        code == failed(EFAULT),
        "creds set from kernel memory -> {code:#x}"
    );

    let mut stats = canary(quota::STATS_WORDS * 8);
    let code = process::dispatch_for_test(11, stats.as_mut_ptr() as u64, 0, 0);
    check!(code == failed(EFAULT), "quota -> {code:#x}");
    untouched(&stats, "quota")?;

    let mut tasks = canary(task::introspect::WORDS * 8);
    let code = process::dispatch_for_test(13, tasks.as_mut_ptr() as u64, 0, 0);
    check!(code == failed(EFAULT), "task snapshot -> {code:#x}");
    untouched(&tasks, "task snapshot")?;

    let mut sysinfo = canary(crate::sysinfo::SIZE as usize);
    let code = process::dispatch_for_test(
        14,
        crate::sysinfo::op::SNAPSHOT,
        sysinfo.as_mut_ptr() as u64,
        crate::sysinfo::SIZE,
    );
    check!(code == failed(EFAULT), "sysinfo snapshot -> {code:#x}");
    untouched(&sysinfo, "sysinfo snapshot")?;

    // `write(1)` from a kernel address would print kernel memory.
    let secret = canary(32);
    let code = process::dispatch_for_test(1, secret.as_ptr() as u64, secret.len() as u64, 0);
    check!(code == u64::MAX, "write from kernel memory -> {code:#x}");
    Ok(())
}

/// The positive control: real user buffers (including one that straddles a
/// page boundary) still work through every validated syscall.
pub fn valid_user_buffers_still_work() -> Result<(), String> {
    fresh()?;
    let _strict = Strict::on();
    in_space(|| -> Result<(), String> {
        let block = SPACE + 0xFF8; // crosses the first page boundary
        let code = process::dispatch_for_test(10, cred_op::GET, u64::MAX, block);
        check!(code == 0, "creds get -> {code:#x}");
        let mut words = [0u64; 5];
        // Safety: the scratch pages are mapped readable while installed.
        unsafe { core::ptr::copy_nonoverlapping(block as *const u64, words.as_mut_ptr(), 5) };
        check!(
            Cred::from_words(words) == Cred::ROOT,
            "creds read back as {words:?}"
        );
        let code = process::dispatch_for_test(10, cred_op::SET, u64::MAX, block);
        check!(code == 0, "creds set from user memory -> {code:#x}");

        let code = process::dispatch_for_test(11, SPACE + 0x1000, 0, 0);
        check!(code == 0, "quota -> {code:#x}");
        let code = process::dispatch_for_test(13, SPACE + 0x2000, 0, 0);
        check!(code == 0, "task snapshot -> {code:#x}");
        let code = process::dispatch_for_test(
            14,
            crate::sysinfo::op::SNAPSHOT,
            SPACE + 0x2000,
            crate::sysinfo::SIZE,
        );
        check!(
            code == crate::sysinfo::SIZE,
            "sysinfo snapshot -> {code:#x}"
        );
        // Safety: mapped readable; the syscall just wrote the header word.
        let version = unsafe { core::ptr::read((SPACE + 0x2000) as *const u64) };
        check!(
            version == crate::sysinfo::SYSTEM_STATS_VERSION,
            "sysinfo version word is {version}"
        );

        // An unmapped and a non-canonical pointer are refused, not faulted.
        for bad in [0xdead_0000u64, 0xffff_8000_0000_0000, u64::MAX - 7, 0] {
            let code = process::dispatch_for_test(11, bad, 0, 0);
            check!(code == failed(EFAULT), "quota into {bad:#x} -> {code:#x}");
        }
        Ok(())
    })
}

/// The Linux ABI shim keeps its infallible call sites, but they validate:
/// a kernel buffer is neither written nor read, and the syscalls with an
/// error path report `-EFAULT`.
pub fn linux_abi_kernel_pointers_are_refused() -> Result<(), String> {
    fresh()?;
    let _strict = Strict::on();

    // getcwd used to write "/" into the kernel buffer.
    let mut cwd = canary(64);
    let _ = process::linux::dispatch_for_test(79, cwd.as_mut_ptr() as u64, 64, 0);
    untouched(&cwd, "getcwd")?;

    // write(1, kernel_secret, n) used to copy kernel bytes to the console.
    let secret = canary(48);
    let code = process::linux::dispatch_for_test(1, 1, secret.as_ptr() as u64, secret.len() as u64);
    check!(
        code == failed(EFAULT),
        "write(1) from kernel memory -> {code:#x}"
    );

    // read(file, kernel_ptr, n) copied file contents to the kernel address
    // (and `fd_read` did it under the task-table lock).
    let fd = task::fd_open(task::Fd::File {
        data: alloc::sync::Arc::new(b"secret bytes".to_vec()),
        offset: 0,
    })
    .ok_or("fd_open failed")?;
    let mut sink = canary(16);
    let code = process::linux::dispatch_for_test(
        0,
        fd as u64,
        sink.as_mut_ptr() as u64,
        sink.len() as u64,
    );
    check!(
        code == failed(EFAULT),
        "read(file) into kernel memory -> {code:#x}"
    );
    untouched(&sink, "read(file)")?;
    check!(
        task::fd_offset(fd) == Some(0),
        "a refused read consumed the file: offset {:?}",
        task::fd_offset(fd)
    );
    let _ = task::fd_close(fd);
    Ok(())
}

/// Edge cases of the validated primitives themselves.
pub fn user_ptr_edge_cases() -> Result<(), String> {
    fresh()?;
    let _strict = Strict::on();
    in_space(|| -> Result<(), String> {
        use crate::user_ptr::{
            try_bytes, try_copy_to, try_cstr, try_read, try_write, CStrError, Fault,
        };
        let end = SPACE + SPACE_PAGES * 4096;
        check!(
            try_write::<u64>(SPACE + 4092, 0x0102_0304_0506_0708).is_ok(),
            "a write straddling two mapped pages failed"
        );
        check!(
            try_read::<u64>(SPACE + 4092) == Ok(0x0102_0304_0506_0708),
            "the straddling write did not read back"
        );
        check!(
            try_write::<u64>(end - 4, 1) == Err(Fault),
            "a write running off the end of the mapping was accepted"
        );
        check!(
            try_copy_to(u64::MAX - 2, &[1, 2, 3, 4]) == Err(Fault),
            "an address-space-wrapping copy was accepted"
        );
        check!(
            try_bytes(0x0000_8000_0000_0000 - 4, 8) == Err(Fault),
            "a range crossing the end of the user half was accepted"
        );
        check!(
            try_bytes(SPACE, 0).is_ok(),
            "an empty range must always validate"
        );
        // A NUL-terminated string that ends before an unmapped page is fine;
        // one that runs into it is not.
        let tail = end - 3;
        check!(
            try_copy_to(tail, b"ok\0").is_ok() && try_cstr(tail, 64) == Ok(b"ok".to_vec()),
            "a string ending at the last mapped byte was refused"
        );
        check!(
            try_copy_to(end - 2, b"ab").is_ok() && try_cstr(end - 2, 64) == Err(CStrError::Fault),
            "an unterminated string running off the mapping was accepted"
        );
        // A string with no NUL within `max` is refused, not silently
        // truncated: a caller would otherwise act on a prefix path. It is
        // reported as Unterminated so a path syscall can say ENAMETOOLONG.
        check!(
            try_copy_to(SPACE, &[b'x'; 64]).is_ok()
                && try_cstr(SPACE, 64) == Err(CStrError::Unterminated),
            "an unterminated string was truncated instead of refused"
        );
        // A NUL exactly at the `max` bound terminates; one just past it does
        // not.
        check!(
            try_copy_to(SPACE, b"abc\0").is_ok()
                && try_cstr(SPACE, 4) == Ok(b"abc".to_vec())
                && try_cstr(SPACE, 3) == Err(CStrError::Unterminated),
            "the max-length termination bound is off by one"
        );
        Ok(())
    })
}

/// An unterminated path that reaches the native-string cap is refused by
/// [`process::user_cstr`], the helper `sys_read_file`/`sys_spawn` read their
/// path through. Before the fix it returned the truncated prefix, which the
/// syscalls then resolved as a different path.
pub fn unterminated_path_is_refused() -> Result<(), String> {
    use crate::user_ptr::Fault;
    fresh()?;
    let _strict = Strict::on();
    in_space(|| -> Result<(), String> {
        // 4096 non-NUL bytes fill the whole native-string cap.
        check!(
            crate::user_ptr::try_copy_to(SPACE, &[b'A'; 4096]).is_ok(),
            "failed to seed the unterminated path"
        );
        check!(
            process::user_cstr(SPACE) == Err(Fault),
            "an unterminated path was read as a truncated string"
        );
        // A terminated path still reads back unchanged.
        check!(
            crate::user_ptr::try_copy_to(SPACE, b"HELLO.TXT\0").is_ok()
                && process::user_cstr(SPACE) == Ok(String::from("HELLO.TXT")),
            "a terminated path was refused"
        );
        Ok(())
    })
}

/// Soak: 20 000 validated copies over aligned, straddling and hostile
/// ranges. Nothing faults, the good ones land, the bad ones are refused,
/// and no frames leak (validation must not materialize stray pages).
pub fn soak_user_ptr_validation() -> Result<(), String> {
    fresh()?;
    let _strict = Strict::on();
    in_space(|| -> Result<(), String> {
        use crate::user_ptr::{try_bytes, try_copy_to};
        let frames_before = mem::frame_stats().live();
        let good = SPACE + 4096 - 6;
        for round in 0..20_000u64 {
            let payload = [round as u8; 12];
            check!(
                try_copy_to(good, &payload).is_ok(),
                "round {round}: a valid straddling copy was refused"
            );
            check!(
                try_bytes(good, 12) == Ok(&payload[..]),
                "round {round}: the copy did not read back"
            );
            let hostile = match round % 4 {
                0 => 0xffff_8000_0000_0000 + round * 8,
                1 => u64::MAX - (round % 16),
                2 => 0x0000_7fff_ffff_f000 + (round % 4096),
                _ => 0,
            };
            check!(
                try_copy_to(hostile, &payload).is_err(),
                "round {round}: hostile address {hostile:#x} was accepted"
            );
        }
        check!(
            mem::frame_stats().live() == frames_before,
            "validation leaked frames: {} -> {}",
            frames_before,
            mem::frame_stats().live()
        );
        Ok(())
    })
}
