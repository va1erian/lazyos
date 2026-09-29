//! Signal hardening: `rt_sigreturn` frame sanitising (#221), frame arithmetic
//! on hostile stacks (#223), and `kill`/`tkill`/`tgkill` permission checks
//! (#230).

use super::*;
use crate::ipc::credentials::{self, Cred, CAP_KILL};
use crate::task::signal::harden::{self, USER_MAX};
use crate::task::signal::SignalError;

const RFLAGS_IF: u64 = 1 << 9;
const RFLAGS_IOPL: u64 = 0b11 << 12;
const RFLAGS_TF: u64 = 1 << 8;

/// An unprivileged user: no capabilities.
fn alice() -> Cred {
    Cred::new(1000, 1000, 0, 0, 7)
}

fn reset_creds() {
    for slot in 0..task::MAX_TASKS {
        credentials::reset_for_task(slot);
    }
}

/// A frame built on a heap stack, with `regs` inside it, plus the `rsp` a
/// `rt_sigreturn` would be entered with.
fn frame_with(regs: &signal::UserRegs, stack: &mut [u8]) -> Result<u64, String> {
    let top = stack.as_mut_ptr() as u64 + stack.len() as u64;
    let info = SigInfo::user(0, signal::SI_USER);
    let result = signal::build_linux_frame(
        top,
        regs,
        signal::SIGUSR1,
        0x0040_1000,
        0,
        0x0040_2000,
        0,
        0,
        &info,
    )
    .ok_or("frame does not fit the test stack")?;
    Ok(result.rsp + 8)
}

fn sane_regs() -> signal::UserRegs {
    signal::UserRegs {
        rip: 0x0040_1000,
        rsp: 0x0070_0000,
        rflags: 0x202,
        rax: 0xa0a0,
        ..signal::UserRegs::default()
    }
}

/// A forged `rt_sigreturn` frame is refused or neutralised: non-canonical or
/// kernel-half `rip`/`rsp` are rejected (they would fault `sysretq` in ring 0),
/// and the flags can never carry IOPL or clear `IF`.
pub fn sigreturn_frame_is_sanitised() -> Result<(), String> {
    fresh()?;
    let mut stack = vec![0u8; 8192];

    let good = sane_regs();
    let rsp = frame_with(&good, &mut stack)?;
    let (restored, _) = harden::restore_frame(rsp).ok_or("a valid frame was refused")?;
    check!(restored == good, "valid frame changed: {restored:?}");

    for bad_rip in [
        0x0000_8000_0000_0000u64,
        0x0001_0000_0000_0000,
        0xffff_8000_0000_0000,
        u64::MAX,
    ] {
        let forged = signal::UserRegs {
            rip: bad_rip,
            ..good
        };
        let rsp = frame_with(&forged, &mut stack)?;
        check!(
            harden::restore_frame(rsp).is_none(),
            "forged rip {bad_rip:#x} was accepted"
        );
    }
    for bad_rsp in [USER_MAX, 0xffff_8000_dead_0000, u64::MAX] {
        let forged = signal::UserRegs {
            rsp: bad_rsp,
            ..good
        };
        let rsp = frame_with(&forged, &mut stack)?;
        check!(
            harden::restore_frame(rsp).is_none(),
            "forged rsp {bad_rsp:#x} was accepted"
        );
    }

    // IOPL=3, IF clear, NT/VM/ID junk: the flags come back safe, and a
    // legitimate user flag (TF) survives.
    let forged = signal::UserRegs {
        rflags: RFLAGS_IOPL | RFLAGS_TF | 0x0020_0000 | 0x4000 | 0x2_0000,
        ..good
    };
    let rsp = frame_with(&forged, &mut stack)?;
    let (restored, _) = harden::restore_frame(rsp).ok_or("flag-only forgery was refused")?;
    check!(
        restored.rflags & RFLAGS_IOPL == 0,
        "IOPL survived: {:#x}",
        restored.rflags
    );
    check!(
        restored.rflags & RFLAGS_IF != 0,
        "IF was not forced on: {:#x}",
        restored.rflags
    );
    check!(restored.rflags & RFLAGS_TF != 0, "user flag TF was dropped");
    check!(
        restored.rflags & !(RFLAGS_IF | RFLAGS_TF | 0x2) == 0,
        "unexpected flag bits: {:#x}",
        restored.rflags
    );
    check!(
        harden::sanitize_rflags(0) == RFLAGS_IF | 0x2,
        "zero flags did not gain IF and the reserved bit"
    );

    // The frame address itself is untrusted: too low, or running off user space.
    for entry_rsp in [
        0u64,
        4,
        7,
        USER_MAX - 100,
        USER_MAX + 8,
        u64::MAX - 3,
        u64::MAX,
    ] {
        check!(
            harden::restore_frame(entry_rsp).is_none(),
            "frame at rsp {entry_rsp:#x} was accepted"
        );
    }
    signal::harness::reset();
    Ok(())
}

/// Frame layout on a tiny or hostile stack fails closed instead of underflowing
/// (a dev-build panic), and a huge alternate stack that wraps is refused.
pub fn frame_arithmetic_is_checked() -> Result<(), String> {
    fresh()?;
    let regs = sane_regs();
    let info = SigInfo::user(0, signal::SI_USER);
    // None of these reach a write: each is rejected before the frame is laid out.
    for top in [
        0u64,
        1,
        127,
        128,
        640,
        4096,
        4096 + 287,
        USER_MAX + 1,
        u64::MAX,
    ] {
        check!(
            signal::build_linux_frame(top, &regs, signal::SIGUSR1, 0x1000, 0, 0x2000, 0, 0, &info)
                .is_none(),
            "linux frame accepted stack top {top:#x}"
        );
        check!(
            signal::build_native_frame(top, &regs, signal::SIGUSR1).is_none(),
            "native frame accepted stack top {top:#x}"
        );
    }

    let me = task::current();
    let wraps = signal::AltStack {
        sp: 0xFFFF_FFFF_FFFF_F000,
        size: 0x1_0000,
        enabled: true,
    };
    check!(
        signal::set_altstack(me, wraps) == Err(SignalError::Invalid),
        "wrapping altstack was accepted"
    );
    let kernel_half = signal::AltStack {
        sp: USER_MAX - 0x1000,
        size: 0x10_0000,
        enabled: true,
    };
    check!(
        signal::set_altstack(me, kernel_half) == Err(SignalError::Invalid),
        "altstack running into the kernel half was accepted"
    );
    let fine = signal::AltStack {
        sp: 0x0060_0000,
        size: 0x1_0000,
        enabled: true,
    };
    check!(
        signal::set_altstack(me, fine).is_ok(),
        "a valid altstack was refused"
    );
    check!(
        harden::altstack_top(u64::MAX, 1).is_none()
            && harden::altstack_top(0x1000, 0x1000) == Some(0x2000),
        "altstack_top edge cases"
    );
    signal::harness::reset();
    Ok(())
}

/// Sustained load: random stack tops and flag words never panic and always
/// satisfy the frame/flags invariants.
pub fn soak_frame_validation() -> Result<(), String> {
    fresh()?;
    let mut seed = 0x1234_5678_9abc_def0u64;
    for round in 0..200_000u32 {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let top = match round % 4 {
            0 => seed,
            1 => seed & 0x0000_7fff_ffff_ffff,
            2 => seed & 0xfff,
            _ => u64::MAX - (seed & 0xff),
        };
        if let Some(base) = harden::frame_below(top, 640) {
            check!(
                base % 16 == 0 && base >= 0x1000 && base + 640 <= USER_MAX,
                "frame_below({top:#x}) -> {base:#x}"
            );
        }
        let flags = harden::sanitize_rflags(seed);
        check!(
            flags & RFLAGS_IOPL == 0 && flags & RFLAGS_IF != 0 && flags & 0x2 != 0,
            "sanitize_rflags({seed:#x}) -> {flags:#x}"
        );
    }
    signal::harness::reset();
    Ok(())
}

/// A sender may only signal its own uid (or hold `CAP_KILL`): a plain user
/// cannot `SIGKILL`/`SIGSTOP` a root service, by pid, tid, or existence probe.
pub fn kill_needs_matching_uid_or_capability() -> Result<(), String> {
    fresh()?;
    reset_creds();
    let attacker = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    let service = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    let peer = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    credentials::set(attacker, alice());
    credentials::set(peer, alice());
    // `service` keeps the root default it inherited from the kernel task.
    let info = SigInfo::user(attacker, signal::SI_USER);

    for sig in [signal::SIGKILL, signal::SIGSTOP, signal::SIGTERM, 0] {
        check!(
            signal::kill(attacker, service as i64, sig, info) == Err(SignalError::NotPermitted),
            "kill({sig}) on a root service was not refused"
        );
        check!(
            signal::send_tid(attacker, service, sig, info) == Err(SignalError::NotPermitted),
            "tkill({sig}) on a root service was not refused"
        );
    }
    check!(
        task::harness::state(service) == Some(TaskState::Runnable),
        "the refused signals still hit the service: {:?}",
        task::harness::state(service)
    );
    check!(
        signal::pending(service) == 0,
        "a refused signal was queued: {:#x}",
        signal::pending(service)
    );

    // Same uid is fine, and so is signalling oneself.
    check!(
        signal::kill(attacker, attacker as i64, 0, info).is_ok(),
        "self probe refused"
    );
    check!(
        signal::kill(attacker, peer as i64, signal::SIGKILL, info).is_ok(),
        "same-uid kill refused"
    );
    check!(
        task::harness::state(peer) == Some(TaskState::Done),
        "same-uid SIGKILL did not land"
    );

    // Existence still wins over permission for a pid that is not there.
    check!(
        signal::kill(attacker, (task::MAX_TASKS - 1) as i64, 0, info)
            == Err(SignalError::NoSuchProcess),
        "kill of an empty slot is not ESRCH"
    );

    // CAP_KILL lifts the uid check; the kernel task always may.
    credentials::set(attacker, Cred::new(1000, 1000, CAP_KILL, 0, 7));
    check!(
        signal::kill(attacker, service as i64, signal::SIGKILL, info).is_ok(),
        "CAP_KILL holder was refused"
    );
    check!(
        task::harness::state(service) == Some(TaskState::Done),
        "CAP_KILL kill did not land"
    );
    let victim = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    credentials::set(victim, alice());
    check!(
        signal::kill(
            task::KERNEL_TASK,
            victim as i64,
            signal::SIGTERM,
            SigInfo::kernel()
        )
        .is_ok(),
        "the kernel task was refused"
    );

    while task::reap_child().is_some() {}
    reset_creds();
    task::harness::reset();
    signal::harness::reset();
    Ok(())
}

/// `SIGCONT` is allowed across uids inside one login session (job control),
/// nothing else is.
pub fn sigcont_within_session() -> Result<(), String> {
    fresh()?;
    reset_creds();
    let sender = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    let target = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    credentials::set(sender, Cred::new(1000, 1000, 0, 0, 7));
    credentials::set(target, Cred::new(1001, 1001, 0, 0, 7));
    let info = SigInfo::user(sender, signal::SI_USER);
    check!(
        signal::kill(sender, target as i64, signal::SIGCONT, info).is_ok(),
        "SIGCONT in the same session was refused"
    );
    check!(
        signal::kill(sender, target as i64, signal::SIGTERM, info)
            == Err(SignalError::NotPermitted),
        "SIGTERM across uids was allowed"
    );
    credentials::set(target, Cred::new(1001, 1001, 0, 0, 8));
    check!(
        signal::kill(sender, target as i64, signal::SIGCONT, info)
            == Err(SignalError::NotPermitted),
        "SIGCONT across sessions was allowed"
    );
    while task::reap_child().is_some() {}
    reset_creds();
    task::harness::reset();
    signal::harness::reset();
    Ok(())
}

/// `kill(-1)` spares init (slot 1) and the sender's own process, skips targets
/// the sender may not signal, and reports `EPERM` when nothing was permitted.
pub fn kill_all_spares_init_and_respects_permissions() -> Result<(), String> {
    fresh()?;
    reset_creds();
    let init = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    check!(init == 1, "first spawn is slot {init}, expected init at 1");
    let root_a = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    let user_b = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    let sender = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    credentials::set(user_b, alice());
    credentials::set(sender, alice());
    let info = SigInfo::user(sender, signal::SI_USER);

    // Alice may signal only user_b: root_a and init survive, and so does she.
    check!(
        signal::kill(sender, -1, signal::SIGKILL, info).is_ok(),
        "kill(-1) with one permitted target failed"
    );
    check!(
        task::harness::state(user_b) == Some(TaskState::Done),
        "the permitted target survived kill(-1)"
    );
    for (name, slot) in [("init", init), ("root task", root_a), ("sender", sender)] {
        check!(
            task::harness::state(slot) == Some(TaskState::Runnable),
            "{name} was hit by kill(-1): {:?}",
            task::harness::state(slot)
        );
    }
    // Nothing left she may signal (user_b is a zombie): EPERM, not success.
    check!(
        signal::kill(sender, -1, signal::SIGKILL, info) == Err(SignalError::NotPermitted),
        "kill(-1) with no permitted target did not fail with EPERM"
    );

    // Root broadcasting still spares init and itself.
    let root_sender = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    credentials::set(root_sender, Cred::ROOT);
    check!(
        signal::kill(root_sender, -1, signal::SIGKILL, info).is_ok(),
        "root kill(-1) failed"
    );
    check!(
        task::harness::state(root_a) == Some(TaskState::Done),
        "root kill(-1) missed a task"
    );
    for (name, slot) in [("init", init), ("root sender", root_sender)] {
        check!(
            task::harness::state(slot) == Some(TaskState::Runnable),
            "{name} was hit by root kill(-1)"
        );
    }
    // A pid or group that overflows `usize` is just "no such process".
    check!(
        signal::kill(sender, i64::MIN, 0, info) == Err(SignalError::NoSuchProcess),
        "kill(i64::MIN) is not ESRCH"
    );

    while task::reap_child().is_some() {}
    reset_creds();
    task::harness::reset();
    signal::harness::reset();
    Ok(())
}

/// Sustained denied and permitted sends across many task generations leak
/// nothing and never let a denied signal through.
pub fn soak_kill_permissions() -> Result<(), String> {
    fresh()?;
    reset_creds();
    let attacker = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    credentials::set(attacker, alice());
    let info = SigInfo::user(attacker, signal::SI_USER);
    for round in 0..300u32 {
        let victim = task::spawn_fork().map_err(|error| format!("spawn {round}: {error}"))?;
        let own = round % 2 == 0;
        credentials::set(victim, if own { alice() } else { Cred::ROOT });
        let result = signal::kill(attacker, victim as i64, signal::SIGKILL, info);
        let done = task::harness::state(victim) == Some(TaskState::Done);
        if own != result.is_ok() || own != done {
            return Err(format!(
                "round {round}: own={own} result={result:?} done={done}"
            ));
        }
        if !own {
            task::harness::finish(victim, 0);
        }
        while task::reap_child().is_some() {}
    }
    reset_creds();
    task::harness::reset();
    signal::harness::reset();
    Ok(())
}
