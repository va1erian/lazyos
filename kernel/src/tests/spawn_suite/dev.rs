//! Spawning into a `dev:` label (issue #529, `kernel/src/ipc/devspawn.rs`)
//! and handing a child its standard streams (`personality::STDIO`).
//!
//! The caller plays a packaged IDE: the kernel task is stamped
//! `app:org.test.ide`, uid 1000, session 7, and the target is the project's
//! `dev:org.test.proj`, "approved" the way `pkgd` does it (interned and given
//! a rule set by the policy loader).

use super::*;
use crate::ipc::acl::{self, Rule, ANY_INTERFACE, ANY_METHOD};
use crate::ipc::topics::fnv1a32;
use crate::ipc::{audit, devspawn, labels, pipe, policy};
use crate::process::spawnv::{EBADF, STDIO_TERMINAL};

pub(super) const EPERM: i64 = 1;
pub(super) const ENOENT: i64 = 2;
pub(super) const EACCES: i64 = 13;

/// The IDE's label.
pub(super) const IDE: &str = "app:org.test.ide";
/// The project's development label.
pub(super) const DEV: &str = "dev:org.test.proj";
/// The IDE's identity.
pub(super) const UID: u32 = 1000;
pub(super) const SESSION: u64 = 7;
/// A capability the IDE holds, to check the child may drop but not add one.
pub(super) const IDE_CAPS: u32 = credentials::CAP_KILL;

/// Forget every label and label rule (no task outlives a test here).
pub(super) fn fresh_labels() -> Result<(), String> {
    fresh()?;
    acl::reset_labels_for_tests();
    labels::reset_for_tests();
    audit::reset();
    Ok(())
}

/// An allow rule for `label_id`.
fn allow(label_id: u32, interface_id: u64, method: u32) -> Rule {
    Rule {
        actor: label_id,
        interface_id,
        method,
        allow: true,
    }
}

/// Stamp the calling (kernel) task as the IDE; `spawn_rule` is the method of
/// its spawn-scope rule (`ANY_METHOD` is what `develop = true` compiles to),
/// or none.
pub(super) fn become_ide(spawn_rule: Option<u32>) -> Result<Cred, String> {
    let id = labels::intern(IDE).map_err(|_| "intern the IDE label")?;
    let rules: Vec<Rule> = spawn_rule
        .map(|method| allow(id, devspawn::SPAWN_SCOPE, method))
        .into_iter()
        .collect();
    acl::load_label(id, &rules).map_err(|e| format!("load IDE rules: {e:?}"))?;
    let cred = Cred::new(UID, UID, IDE_CAPS, id, SESSION);
    credentials::set(task::current(), cred);
    Ok(cred)
}

/// Approve `label` as `pkgd` does: interned by the loader, with an approved
/// rule set that always holds at least the catch-all deny.
pub(super) fn approve(label: &str) -> Result<u32, String> {
    let id = labels::intern(label).map_err(|_| "intern the dev label")?;
    let sentinel = Rule {
        actor: id,
        interface_id: ANY_INTERFACE,
        method: ANY_METHOD,
        allow: false,
    };
    acl::load_label(id, &[sentinel]).map_err(|e| format!("approve: {e:?}"))?;
    Ok(id)
}

/// A labelled `spawnv` of the suite's native program into `label` with the
/// IDE's own identity (capabilities dropped).
pub(super) fn dev_request(label: &str) -> Req {
    let mut request = Req::new(NATIVE, &[b"proj"], &[b"HOME=/home/dev"], false);
    request.mode = cred_mode::AS_LABELLED;
    request.cred = Cred::new(UID, UID, 0, 0, SESSION);
    request.label = label.as_bytes().to_vec();
    request
}

/// Expect `-errno` with no task created.
fn refuse(request: &Req, errno: i64, what: &str) -> Result<(), String> {
    let before = usage();
    let code = request.call();
    check!(
        code == failed(errno),
        "{what}: returned {code:#x}, expected -{errno}"
    );
    no_leak(before, usage(), 4096, what)
}

/// Run `body` as the IDE, restoring the kernel task's root identity after.
fn as_ide(body: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
    let outcome = body();
    credentials::reset_for_task(task::current());
    outcome
}

/// Every way a spawn into a `dev:` label is refused: no rule, a rule for
/// another label, an unapproved or revoked label, an `app:`/`system:` target,
/// a changed identity or a widened capability set, an unlabelled caller.
pub fn dev_label_refusals() -> Result<(), String> {
    fresh_labels()?;
    as_ide(|| {
        // The IDE without `develop`: refused, audited on the spawn scope.
        become_ide(None)?;
        approve(DEV)?;
        refuse(&dev_request(DEV), EACCES, "no spawn rule")?;
        let event = *audit::recent(1).first().ok_or("no audit record")?;
        check!(
            !event.allow
                && event.interface_id == devspawn::SPAWN_SCOPE
                && event.method == fnv1a32(DEV),
            "the refusal was audited as {event:?}"
        );

        // A rule for one exact other label does not cover this one.
        become_ide(Some(fnv1a32("dev:org.test.other")))?;
        refuse(&dev_request(DEV), EACCES, "a rule for another label")?;

        // The wildcard rule, but a label pkgd never approved: refused, and
        // the spawn path never interns it.
        become_ide(Some(ANY_METHOD))?;
        refuse(
            &dev_request("dev:org.test.never"),
            EACCES,
            "unapproved label",
        )?;
        check!(
            labels::lookup("dev:org.test.never").is_none(),
            "a refused dev spawn interned its label"
        );
        // Revoked (pkgd loads an empty set at logout): refused again.
        let dev = labels::lookup(DEV).ok_or("dev label")?;
        acl::load_label(dev, &[]).map_err(|e| format!("{e:?}"))?;
        refuse(&dev_request(DEV), EACCES, "revoked label")?;
        approve(DEV)?;

        // `app:` and `system:` stay the credential gate's (CAP_SETUID).
        for target in ["app:org.test.proj", "system:netd", IDE] {
            refuse(&dev_request(target), EPERM, target)?;
        }
        // A malformed label is not a dev label: the gate's answer (`-EPERM`
        // without CAP_SETUID, as before), and nothing is interned.
        refuse(&dev_request("dev:Bad"), EPERM, "malformed dev label")?;

        // The child's identity is the caller's, capabilities at most narrowed.
        let changes = [
            ("uid 0", Cred::new(0, UID, 0, 0, SESSION)),
            ("another uid", Cred::new(UID + 1, UID, 0, 0, SESSION)),
            ("another gid", Cred::new(UID, UID + 1, 0, 0, SESSION)),
            ("another session", Cred::new(UID, UID, 0, 0, SESSION + 1)),
            ("no session", Cred::new(UID, UID, 0, 0, 0)),
            (
                "a capability the IDE lacks",
                Cred::new(UID, UID, credentials::CAP_SETUID, 0, SESSION),
            ),
        ];
        for (what, cred) in changes {
            let mut request = dev_request(DEV);
            request.cred = cred;
            refuse(&request, EACCES, what)?;
        }
        Ok(())
    })?;

    // An unlabelled caller without CAP_SETUID keeps the old answer.
    credentials::set(task::current(), Cred::new(UID, UID, 0, 0, SESSION));
    let outcome = refuse(&dev_request(DEV), EPERM, "unlabelled, unprivileged");
    credentials::reset_for_task(task::current());
    outcome
}

/// An allowed spawn stamps the child with the dev label and the caller's
/// identity (a capability may be dropped), audits the transition, and leaves
/// the unlabelled privileged path (`init`) able to assign a `dev:` label as
/// before.
pub fn dev_label_stamps_child() -> Result<(), String> {
    fresh_labels()?;
    let dev = approve(DEV)?;
    let ide = become_ide(Some(fnv1a32(DEV)));
    let outcome = (|| {
        let ide = ide?;
        let slot = spawned(dev_request(DEV).call())?;
        let child = credentials::of(slot);
        check!(
            child == Cred::new(UID, UID, 0, dev, SESSION),
            "the child was stamped {child:?}"
        );
        check!(
            task::process::ppid_of(slot) == task::current(),
            "the child is not the IDE's"
        );
        check!(
            labels::kind_of(child.label_id) == Some(labels::Kind::Dev),
            "the child's label is not a dev label"
        );
        // Keeping a capability the IDE holds is allowed.
        let mut keep = dev_request(DEV);
        keep.cred = Cred::new(UID, UID, IDE_CAPS, 0, SESSION);
        let second = spawned(keep.call())?;
        check!(
            credentials::of(second).caps == IDE_CAPS,
            "the held capability was not kept"
        );
        check!(
            ide.label_id != dev,
            "setup: the IDE is not in the dev label"
        );
        reap(slot)?;
        reap(second)
    })();
    credentials::reset_for_task(task::current());
    outcome?;

    // `init` (unlabelled, CAP_SETUID) assigns any label as it always did.
    let slot = spawned(dev_request("dev:org.test.init").call())?;
    check!(
        labels::name_of(credentials::of(slot).label_id).as_deref() == Some("dev:org.test.init"),
        "the privileged labelled spawn changed"
    );
    reap(slot)
}

/// A `dev:<id>` task owns exactly the installed app's namespace: it may
/// register `app.<id>.<name>` and use `app/<id>/` topics, nothing of another
/// app's or the platform's.
pub fn dev_label_namespace() -> Result<(), String> {
    fresh_labels()?;
    let dev = approve(DEV)?;
    let slot = spawned(Req::new(NATIVE, &[b"proj"], &[], false).call())?;
    credentials::set(slot, Cred::new(UID, UID, 0, dev, SESSION));
    let cred = credentials::of(slot);
    let register = |name| policy::check_name(slot, policy::NameOp::Register, name).is_ok();
    let outcome = (|| {
        check!(register("app.org.test.proj.svc"), "own name refused");
        check!(!register("app.org.test.other.svc"), "another app's name");
        check!(
            !register("app.org.test.proj.sub.svc"),
            "a deeper app's name"
        );
        check!(!register("os.lazy.test"), "a platform name");
        check!(!register("plain"), "a name outside every namespace");
        check!(
            policy::owns_topic(&cred, "app/org.test.proj/state"),
            "own topic refused"
        );
        check!(
            !policy::owns_topic(&cred, "app/org.test.other/state"),
            "another app's topic"
        );
        // Calls stay default-deny beyond the approved rules.
        let (decision, _) = policy::evaluate_labelled(&cred, 0x1234, 1);
        check!(decision.denied(), "an unapproved call was allowed");
        Ok(())
    })();
    reap(slot)?;
    outcome
}

/// `pipe(2)` in the calling task: `(read, write)`.
pub(super) fn make_pipe() -> Result<(usize, usize), String> {
    let mut fds = [0i32; 2];
    let ret = process::linux::dispatch_for_test(22, fds.as_mut_ptr() as u64, 0, 0);
    check!(ret == 0, "pipe returned {ret:#x}");
    Ok((fds[0] as usize, fds[1] as usize))
}

/// `STDIO` gives the child exactly the three named descriptors (shared with
/// the caller) and nothing else; a closed or out-of-range one is `-EBADF`.
pub fn stdio_hands_descriptors() -> Result<(), String> {
    fresh()?;
    let live = pipe::Pipe::live();
    let (read, write) = make_pipe()?;
    let request = Req::new(LINUX, &[b"proj"], &[], true);
    let before = usage();
    for (stdio, what) in [
        ([STDIO_TERMINAL, 15, STDIO_TERMINAL], "a closed descriptor"),
        (
            [STDIO_TERMINAL, write as u64, 99],
            "an out-of-range descriptor",
        ),
        (
            [u64::MAX - 1, write as u64, write as u64],
            "a huge descriptor",
        ),
    ] {
        let code = request.call_stdio(stdio);
        check!(code == failed(EBADF), "{what}: returned {code:#x}");
    }
    no_leak(before, usage(), 4096, "refused stdio")?;
    let slot = spawned(request.call_stdio([STDIO_TERMINAL, write as u64, write as u64]))?;
    let kinds: Vec<task::FdKind> = (0..crate::limits::fd_max())
        .map(|fd| task::harness::fd_kind_at(slot, fd))
        .collect();
    check!(
        kinds[0] == task::FdKind::Terminal
            && kinds[1] == task::FdKind::Pipe
            && kinds[2] == task::FdKind::Pipe,
        "the child's streams are {:?}",
        &kinds[..3]
    );
    check!(
        kinds[3..].iter().all(|kind| *kind == task::FdKind::Closed),
        "the child got more than its three streams: {kinds:?}"
    );
    // Without the flag a child starts on the terminal, as before.
    let plain = spawned(request.call())?;
    check!(
        task::harness::fd_kind_at(plain, 1) == task::FdKind::Terminal,
        "a plain spawn lost its terminal"
    );
    reap(plain)?;
    // The caller closes its write end: the child still holds the pipe open,
    // and reaping the child releases the last writer (the reader sees EOF).
    check!(task::fd_close(write), "close the write end");
    reap(slot)?;
    let mut byte = [0u8; 1];
    let eof = task::fd_stream_read(read, &mut byte);
    check!(matches!(eof, Ok(0)), "the reader did not see EOF: {eof:?}");
    check!(task::fd_close(read), "close the read end");
    check!(
        pipe::Pipe::live() == live,
        "pipes leaked: {} -> {}",
        live,
        pipe::Pipe::live()
    );
    Ok(())
}
