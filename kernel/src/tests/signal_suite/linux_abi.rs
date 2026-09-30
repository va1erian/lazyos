//! The Linux `rt_sigframe`/`sigset_t` ABI: handler-frame round-trip,
//! sigset bit order, `rt_sigprocmask`/`rt_sigaction` boundary checks,
//! a translation soak, and stop/continue.

use super::*;

/// The Linux `rt_sigframe` layout round-trips: the handler's `pretcode`,
/// `siginfo_t` and `ucontext_t` read back as written, and the native frame
/// parser recovers the interrupted registers.
pub fn handler_frame_roundtrip() -> Result<(), String> {
    fresh()?;
    let mut stack = alloc::vec![0u8; 8192];
    let top = stack.as_mut_ptr() as u64 + stack.len() as u64;
    let regs = signal::UserRegs {
        r15: 0x1515,
        r14: 0x1414,
        r13: 0x1313,
        r12: 0x1212,
        r11: 0x1111,
        r10: 0x1010,
        r9: 0x0909,
        r8: 0x0808,
        rbp: 0xb0b0,
        rdi: 0xd1d1,
        rsi: 0x5151,
        rdx: 0xd2d2,
        rcx: 0xc0c0,
        rbx: 0xb0b1,
        rax: 0xa0a0,
        rip: 0x0040_1000,
        rsp: top - 0x80,
        rflags: 0x202,
    };
    // Both masks are in kernel bit order when building the frame.
    let sa_mask = 1u64 << signal::SIGUSR2;
    let saved_mask = (1u64 << signal::SIGUSR1) | (1u64 << signal::SIGTERM);
    let info = SigInfo::fault(signal::SEGV_ACCERR, 0xdead_beef);
    let result = signal::build_linux_frame(
        top,
        &regs,
        signal::SIGSEGV,
        0x0040_2000,
        signal::SA_SIGINFO,
        0x0040_3000,
        sa_mask,
        saved_mask,
        &info,
    )
    .ok_or("frame does not fit the stack")?;
    check!(
        result.rip == 0x0040_2000,
        "handler rip is {:#x}",
        result.rip
    );
    // The handler is entered as if by `call`: `rsp + 8` is 16-byte aligned.
    check!(
        result.rsp % 16 == 8,
        "handler entry rsp {:#x} is not call-aligned",
        result.rsp
    );
    let pretcode = unsafe { core::ptr::read_volatile(result.rsp as *const u64) };
    check!(pretcode == 0x0040_3000, "pretcode is {pretcode:#x}");
    let signo = unsafe { core::ptr::read_volatile(result.info as *const i32) };
    check!(signo == signal::SIGSEGV as i32, "siginfo signo is {signo}");
    // The frame itself carries the masks in Linux `sigset_t` bit order.
    // Safety: `build_linux_frame` just wrote `uc_sigmask` on this stack.
    let raw =
        unsafe { core::ptr::read_volatile((result.rsp + signal::lf::UC_SIGMASK) as *const u64) };
    check!(
        raw == signal::kernel_to_linux_sigset(saved_mask),
        "uc_sigmask is {raw:#x}, expected {:#x}",
        signal::kernel_to_linux_sigset(saved_mask)
    );
    // Safety: same frame, just written, at a fixed `sigcontext` offset.
    let raw_old = unsafe {
        core::ptr::read_volatile(
            (result.rsp + signal::lf::MCONTEXT + signal::lf::OLDMASK) as *const u64,
        )
    };
    check!(
        raw_old == signal::kernel_to_linux_sigset(sa_mask),
        "sigcontext.oldmask is {raw_old:#x}, expected {:#x}",
        signal::kernel_to_linux_sigset(sa_mask)
    );
    let (restored, mask) = signal::harden::parse_frame(result.rsp + 8).ok_or("frame unreadable")?;
    check!(mask == saved_mask, "saved mask is {mask:#x}");
    check!(restored == regs, "restored registers differ: {restored:?}");

    // Whatever the alignment of the stack top (an `rsp` or a `sigaltstack`
    // end), both handlers are entered with `rsp + 8` 16-aligned: Rust's
    // SIGSEGV handler died of `#GP` on its first `movaps` before this held.
    for skew in 0..16u64 {
        let skewed = top - 64 - skew;
        let linux = signal::build_linux_frame(
            skewed,
            &regs,
            signal::SIGSEGV,
            0x0040_2000,
            0,
            0x0040_3000,
            0,
            0,
            &info,
        )
        .ok_or("skewed frame does not fit")?;
        let native = signal::build_native_frame(skewed, &regs, signal::SIGTERM)
            .ok_or("skewed native frame does not fit")?;
        check!(
            linux.rsp % 16 == 8 && native.rsp % 16 == 8,
            "top {skewed:#x}: handler rsp {:#x} / native {:#x} not call-aligned",
            linux.rsp,
            native.rsp
        );
    }
    let native = signal::build_native_frame(top, &regs, signal::SIGTERM)
        .ok_or("native frame does not fit")?;
    let (native_regs, native_sig) = signal::parse_native_frame(native.rsp);
    check!(
        native_sig == signal::SIGTERM,
        "native frame signal is {native_sig}"
    );
    check!(native_regs == regs, "native frame lost registers");
    signal::harness::reset();
    Ok(())
}

/// The Linux `sigset_t` bit order (`1 << (sig - 1)`) round-trips through
/// the kernel's internal order (`1 << sig`) for every representable
/// signal; `SIGRTMAX` has no kernel bit and must not shift out of range.
pub fn linux_sigset_roundtrip() -> Result<(), String> {
    fresh()?;

    check!(
        signal::linux_sigset_to_kernel(0) == 0 && signal::kernel_to_linux_sigset(0) == 0,
        "the empty set did not translate to 0"
    );
    // Signal 1 is Linux bit 0 and kernel bit 1.
    check!(
        signal::linux_sigset_to_kernel(1) == 1 << signal::SIGHUP,
        "SIGHUP translated to {:#x}",
        signal::linux_sigset_to_kernel(1)
    );
    // Signal 32 is Linux bit 31 and kernel bit 32.
    check!(
        signal::linux_sigset_to_kernel(1 << 31) == 1 << 32,
        "signal 32 translated to {:#x}",
        signal::linux_sigset_to_kernel(1 << 31)
    );
    // Signal 64 (`SIGRTMAX`) is Linux bit 63: it has no kernel bit, so it
    // is dropped rather than shifted out of range.
    check!(
        signal::linux_sigset_to_kernel(1 << 63) == 0,
        "SIGRTMAX translated to {:#x}",
        signal::linux_sigset_to_kernel(1 << 63)
    );
    // Kernel bit 0 is "no signal" and has no Linux bit.
    check!(
        signal::kernel_to_linux_sigset(1) == 0,
        "the kernel's bit 0 leaked into a sigset"
    );
    // Every representable signal round-trips exactly.
    for sig in 1..=63u8 {
        let linux = 1u64 << (sig - 1);
        let kernel = signal::linux_sigset_to_kernel(linux);
        check!(
            kernel == 1u64 << sig,
            "signal {sig} translated to {kernel:#x}"
        );
        check!(
            signal::kernel_to_linux_sigset(kernel) == linux,
            "signal {sig} did not round-trip"
        );
    }
    // Uncatchable bits still translate; the mask filter drops them later.
    let uncatchable = (1u64 << (signal::SIGKILL - 1)) | (1u64 << (signal::SIGSTOP - 1));
    check!(
        signal::linux_sigset_to_kernel(uncatchable)
            == (1u64 << signal::SIGKILL) | (1u64 << signal::SIGSTOP),
        "the uncatchable pair translated to {:#x}",
        signal::linux_sigset_to_kernel(uncatchable)
    );
    signal::harness::reset();
    Ok(())
}

/// `rt_sigprocmask` and `rt_sigaction` cross the Linux ABI boundary with
/// translated masks: a Linux `sigset_t` goes in, the kernel's bit order is
/// stored, and a Linux `sigset_t` comes back out.
pub fn linux_sigprocmask_sigset_boundary() -> Result<(), String> {
    fresh()?;
    let me = task::current();

    // SIGUSR2 = 12: Linux bit 11, kernel bit 12.
    let block = 1u64 << (signal::SIGUSR2 - 1);
    let e = process::linux::dispatch_for_test(
        14,
        signal::SIG_BLOCK,
        core::ptr::addr_of!(block) as u64,
        0,
    );
    check!(e == 0, "rt_sigprocmask(SIG_BLOCK) returned {e:#x}");
    check!(
        signal::blocked(me) == 1 << signal::SIGUSR2,
        "kernel blocked mask is {:#x}",
        signal::blocked(me)
    );
    let mut old = 0u64;
    let e = process::linux::dispatch_for_test(14, 0, 0, core::ptr::addr_of_mut!(old) as u64);
    check!(
        e == 0 && old == block,
        "oldset is {old:#x} (ret {e:#x}), expected {block:#x}"
    );

    // SIGKILL/SIGSTOP (Linux bits 8/18) are dropped, never stored raw.
    let uncatchable = (1u64 << (signal::SIGKILL - 1)) | (1u64 << (signal::SIGSTOP - 1));
    let e = process::linux::dispatch_for_test(
        14,
        signal::SIG_BLOCK,
        core::ptr::addr_of!(uncatchable) as u64,
        0,
    );
    check!(e == 0, "blocking SIGKILL/SIGSTOP returned {e:#x}");
    check!(
        signal::blocked(me) & ((1 << signal::SIGKILL) | (1 << signal::SIGSTOP)) == 0,
        "uncatchable bits entered the kernel mask: {:#x}",
        signal::blocked(me)
    );

    // Unblocking in Linux order clears exactly the requested bit.
    let e = process::linux::dispatch_for_test(
        14,
        signal::SIG_UNBLOCK,
        core::ptr::addr_of!(block) as u64,
        0,
    );
    check!(e == 0, "rt_sigprocmask(SIG_UNBLOCK) returned {e:#x}");
    check!(
        signal::blocked(me) == 0,
        "SIGUSR2 stayed blocked: {:#x}",
        signal::blocked(me)
    );

    // SIG_SETMASK with SIGHUP and SIGRTMAX: only SIGHUP is representable.
    let set = 1u64 | (1u64 << 63);
    let e = process::linux::dispatch_for_test(
        14,
        signal::SIG_SETMASK,
        core::ptr::addr_of!(set) as u64,
        0,
    );
    check!(e == 0, "rt_sigprocmask(SIG_SETMASK) returned {e:#x}");
    check!(
        signal::blocked(me) == 1 << signal::SIGHUP,
        "SIG_SETMASK stored {:#x}",
        signal::blocked(me)
    );

    // `rt_sigaction`: `sa_mask` is stored in kernel order (uncatchable bits
    // filtered) and reported back in Linux order.
    let mut action = [0u64; 4];
    action[0] = 0x40_1000;
    action[2] = 0x40_2000;
    action[3] = (1u64 << (signal::SIGUSR2 - 1)) | (1u64 << (signal::SIGKILL - 1));
    let e =
        process::linux::dispatch_for_test(13, signal::SIGTERM as u64, action.as_ptr() as u64, 0);
    check!(e == 0, "rt_sigaction(SIGTERM) returned {e:#x}");
    check!(
        signal::action(me, signal::SIGTERM)
            == Disposition::Handler {
                handler: 0x40_1000,
                flags: 0,
                restorer: 0x40_2000,
                mask: 1 << signal::SIGUSR2,
            },
        "stored action is {:?}",
        signal::action(me, signal::SIGTERM)
    );
    let mut old = [0u64; 4];
    let e =
        process::linux::dispatch_for_test(13, signal::SIGTERM as u64, 0, old.as_mut_ptr() as u64);
    check!(
        e == 0 && old[3] == 1 << (signal::SIGUSR2 - 1),
        "reported sa_mask is {:#x} (ret {e:#x})",
        old[3]
    );

    signal::set_blocked(me, 0);
    signal::harness::reset();
    Ok(())
}

/// Soak: hundreds of thousands of Linux sets through the translation and
/// the `rt_sigprocmask` handler, then thousands through the frame builder
/// and parser, asserting after every cycle that no bit drifted.
pub fn linux_sigset_translate_soak() -> Result<(), String> {
    fresh()?;
    let me = task::current();
    const ROUNDS: u32 = 500_000;
    let mut seed: u64 = 0x243f_6a88_85a3_08d3;
    let uncatchable = (1u64 << signal::SIGKILL) | (1u64 << signal::SIGSTOP);
    for round in 0..ROUNDS {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let linux = seed;
        let kernel = signal::linux_sigset_to_kernel(linux);
        let back = signal::kernel_to_linux_sigset(kernel);
        // Linux bit 63 (`SIGRTMAX`) has no kernel bit and is dropped.
        let representable = linux & !(1u64 << 63);
        if back != representable {
            return Err(format!(
                "round {round}: {linux:#018x} -> {kernel:#018x} -> {back:#018x}, expected {representable:#018x}"
            ));
        }

        // The same set through the syscall boundary: block, query back,
        // then clear. The kernel mask must be exactly the translated set
        // minus the uncatchable bits.
        let set = linux;
        let e = process::linux::dispatch_for_test(
            14,
            signal::SIG_BLOCK,
            core::ptr::addr_of!(set) as u64,
            0,
        );
        if e != 0 {
            return Err(format!("round {round}: rt_sigprocmask returned {e:#x}"));
        }
        let expected = kernel & !uncatchable;
        let observed = signal::blocked(me);
        if observed != expected {
            return Err(format!(
                "round {round}: kernel mask {observed:#018x}, expected {expected:#018x}"
            ));
        }
        let mut old = 0u64;
        let e = process::linux::dispatch_for_test(14, 0, 0, core::ptr::addr_of_mut!(old) as u64);
        if e != 0 || old != signal::kernel_to_linux_sigset(expected) {
            return Err(format!(
                "round {round}: oldset {old:#018x}, expected {:#018x}",
                signal::kernel_to_linux_sigset(expected)
            ));
        }
        let clear = 0u64;
        let e = process::linux::dispatch_for_test(
            14,
            signal::SIG_SETMASK,
            core::ptr::addr_of!(clear) as u64,
            0,
        );
        if e != 0 || signal::blocked(me) != 0 {
            return Err(format!(
                "round {round}: clear left {:#x}",
                signal::blocked(me)
            ));
        }
    }

    // The frame boundary round-trips the same masks without drift.
    let mut stack = vec![0u8; 8192];
    let top = stack.as_mut_ptr() as u64 + stack.len() as u64;
    let regs = signal::UserRegs::default();
    let info = SigInfo::user(0, signal::SI_USER);
    for round in 0..4096u32 {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let saved = signal::linux_sigset_to_kernel(seed);
        let result = signal::build_linux_frame(
            top,
            &regs,
            signal::SIGUSR1,
            0x0040_1000,
            0,
            0x0040_2000,
            0,
            saved,
            &info,
        )
        .ok_or("frame does not fit the stack")?;
        let (_, parsed) = signal::harden::parse_frame(result.rsp + 8).ok_or("frame unreadable")?;
        // Safety: `build_linux_frame` just wrote `uc_sigmask` on this stack.
        let raw = unsafe {
            core::ptr::read_volatile((result.rsp + signal::lf::UC_SIGMASK) as *const u64)
        };
        if parsed != saved || raw != signal::kernel_to_linux_sigset(saved) {
            return Err(format!(
                "frame round {round}: saved {saved:#018x}, parsed {parsed:#018x}, uc_sigmask {raw:#018x}"
            ));
        }
    }

    signal::harness::reset();
    Ok(())
}

/// A stop signal parks the whole process and only `SIGCONT` resumes it.
pub fn stop_continue() -> Result<(), String> {
    fresh()?;
    let child = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    send(child, signal::SIGSTOP)?;
    check!(
        task::harness::state(child)
            == Some(TaskState::Blocked {
                wait: WaitKind::Signal,
                deadline: None
            }),
        "SIGSTOP did not park the child: {:?}",
        task::harness::state(child)
    );
    send(child, signal::SIGCONT)?;
    check!(
        task::harness::state(child) == Some(TaskState::Runnable),
        "SIGCONT did not resume the child: {:?}",
        task::harness::state(child)
    );
    task::harness::finish(child, 0);
    check!(task::reap_child().is_some(), "child was not reapable");
    task::harness::reset();
    signal::harness::reset();
    Ok(())
}
