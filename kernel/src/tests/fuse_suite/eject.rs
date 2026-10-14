//! Ejecting a removable volume (`fs::fuse::supervise`, docs/usb-hotplug-plan.md
//! D): the supervisor lists the media providers, asks one to stop, learns who
//! keeps it busy, and the daemon sees the stop from `NEXT` only after what it
//! was owed is answered. Syscall 35's `LIST`, `STOP` and `HOLDERS` ops go
//! through the gate like a real `mountd`'s.

use super::*;
use crate::fs as vfsapi;
use crate::fs::fuse::sys as fusesys;
use crate::fs::fuse::{Next, StopError};
use crate::fs::openfile::OpenFile;
use crate::task::holders;
use fused::wire::{
    sys_op, ProviderInfo, PROVIDER_ALIVE, PROVIDER_STOPPING, PROVIDER_WORDS, STOP_PENDING,
};

const ROOT: Id = Id::ROOT;
const EPERM: i64 = 1;
const ESRCH: i64 = 3;
const EBUSY: i64 = 16;

/// [`prepare`], then mount `/media/<name>` served by the fake in `Normal`.
fn setup_media(name: &str) -> Result<(usize, usize), String> {
    let owner = prepare(true)?;
    let index = fuse::register(owner, name, MountFlags::default(), true)
        .map_err(|e| format!("register media: {e:?}"))?;
    *FAKE.lock() = Some(Fake {
        index,
        owner,
        mode: Mode::Normal,
        fs: MemFs::new(4 << 20, 4100, DAEMON_UID, DAEMON_UID, clock),
        served: 0,
        ops: Vec::new(),
        buffers: Buffers::new(),
    });
    test_hook::set_server(Some(serve));
    Ok((index, owner))
}

fn next_of(index: usize, owner: usize) -> Result<fuse::Next, FuseError> {
    fuse::next(index, owner, 0, &mut |_| Ok(()))
}

fn failed(code: i64) -> u64 {
    (code as u64).wrapping_neg()
}

/// The list, the stop, the refusal of new requests and the unregister that
/// follows it; the slot is clean when it is reused.
pub fn list_and_stop() -> Result<(), String> {
    let (index, owner) = setup_media("STICK")?;
    let result = (|| {
        // A network provider is not a removable volume: not listed, not
        // stoppable.
        let plain = fuse::register(owner, "net", MountFlags::default(), false)
            .map_err(|e| format!("register net: {e:?}"))?;
        let listed = fuse::list();
        check!(listed.len() == 1, "listed {listed:?}");
        let entry = &listed[0];
        check!(
            entry.id == index as u64
                && entry.owner == owner as u64
                && entry.name == "STICK"
                && entry.state == PROVIDER_ALIVE,
            "wrong entry {entry:?}"
        );
        check!(
            fuse::stop(plain, owner, false) == Err(StopError::NoSuchProvider),
            "a network mount was stopped"
        );
        check!(
            fuse::stop(fuse::MAX_PROVIDERS, owner, false) == Err(StopError::NoSuchProvider),
            "an id past the table"
        );
        vfsapi::abi_create(ROOT, "/media/STICK/a", 0o644).map_err(|e| format!("create: {e:?}"))?;
        // Nothing queued and not stopping: an idle NEXT.
        check!(next_of(index, owner) == Ok(Next::Idle), "idle NEXT");

        check!(fuse::stop(index, owner, false).is_ok(), "stop");
        check!(
            fuse::stop(index, owner, false).is_ok(),
            "a second stop is a no-op"
        );
        let listed = fuse::list();
        check!(
            listed.len() == 1 && listed[0].state == PROVIDER_ALIVE | PROVIDER_STOPPING,
            "state after stop: {listed:?}"
        );
        // New requests are refused, visibly. (A name never looked up: the
        // VFS answers a cached one without asking the daemon.)
        expect(
            vfsapi::abi_stat(ROOT, "/media/STICK/never-looked-up"),
            FsError::Io,
            "a request after the stop",
        )?;
        check!(
            next_of(index, owner) == Ok(Next::Stop),
            "NEXT after the stop"
        );
        // The daemon finishes by unregistering; the name is free and the
        // new registration is not stopping.
        check!(fuse::unregister(index, owner).is_ok(), "unregister");
        check!(fuse::list().is_empty(), "listed after unregister");
        let again = fuse::register(owner, "STICK", MountFlags::default(), true)
            .map_err(|e| format!("register again: {e:?}"))?;
        let listed = fuse::list();
        check!(
            listed.len() == 1 && listed[0].state == PROVIDER_ALIVE,
            "stopping leaked into the next registration: {listed:?}"
        );
        check!(fuse::unregister(again, owner).is_ok(), "unregister again");
        Ok(())
    })();
    teardown();
    result
}

/// An open file, a mapping-only holder and a working directory each make the
/// volume busy, are named, and stop mattering once gone; a forced stop goes
/// ahead regardless.
pub fn busy_names_holders() -> Result<(), String> {
    let (index, owner) = setup_media("STICK")?;
    let result = (|| {
        for (path, prefix, want) in [
            ("/media/STICK", "/media/STICK", true),
            ("/media/STICK/a/b", "/media/STICK", true),
            ("/media/STICKY", "/media/STICK", false),
            ("/media/STIC", "/media/STICK", false),
            ("/media", "/media/STICK", false),
        ] {
            check!(
                holders::is_under(path, prefix) == want,
                "is_under({path}, {prefix})"
            );
        }
        vfsapi::abi_create(ROOT, "/media/STICK/log", 0o644)
            .map_err(|e| format!("create: {e:?}"))?;
        check!(
            fuse::holders_report(index, owner).as_deref() == Ok(""),
            "a report for an idle volume"
        );

        // A mapping or a descriptor that no task table shows: the registry.
        let file = OpenFile::open("/media/STICK/log", true, true, false)
            .map_err(|e| format!("open: {e:?}"))?;
        check!(
            fuse::stop(index, owner, false) == Err(StopError::Busy),
            "open file"
        );
        let report = fuse::holders_report(index, owner).map_err(|e| format!("report: {e:?}"))?;
        check!(
            report == "(mapped): /media/STICK/log\n",
            "an unattributed open file: {report:?}"
        );
        // The same file in the task's descriptor table is named by task.
        let fd = task::fd_open(task::Fd::Vfs {
            file: alloc::sync::Arc::clone(&file),
        })
        .ok_or("no descriptor")?;
        let report = fuse::holders_report(index, owner).map_err(|e| format!("report: {e:?}"))?;
        let line = format!("({owner}): /media/STICK/log\n");
        check!(
            report.ends_with(&line) && !report.contains("(mapped)"),
            "an attributed open file: {report:?}"
        );
        check!(
            fuse::stop(index, owner, false) == Err(StopError::Busy),
            "descriptor"
        );
        // The refusal left the provider serving.
        check!(
            fuse::list()[0].state == PROVIDER_ALIVE,
            "a refused stop changed the state"
        );
        check!(
            vfsapi::abi_stat(ROOT, "/media/STICK/log").is_ok(),
            "a refused stop broke the mount"
        );
        check!(task::fd_close(fd), "close");
        drop(file);

        // A shell sitting in the volume.
        task::set_cwd("/media/STICK");
        check!(
            fuse::stop(index, owner, false) == Err(StopError::Busy),
            "cwd"
        );
        let report = fuse::holders_report(index, owner).map_err(|e| format!("report: {e:?}"))?;
        check!(
            report.contains("working directory /media/STICK\n"),
            "cwd not named: {report:?}"
        );
        task::set_cwd("/");
        check!(
            fuse::holders_report(index, owner).as_deref() == Ok(""),
            "holders after letting go"
        );

        // Forced: goes ahead with the file still open.
        let held = OpenFile::open("/media/STICK/log", true, false, false)
            .map_err(|e| format!("reopen: {e:?}"))?;
        check!(fuse::stop(index, owner, true).is_ok(), "forced stop");
        check!(next_of(index, owner) == Ok(Next::Stop), "NEXT after force");
        drop(held);
        check!(fuse::unregister(index, owner).is_ok(), "unregister");
        Ok(())
    })();
    task::set_cwd("/");
    teardown();
    result
}

/// The fake daemon, asking for the stop from inside the requester's wait:
/// the requester has queued its request when this first runs.
fn stop_then_serve(index: usize) {
    use core::sync::atomic::{AtomicBool, Ordering};
    static ASKED: AtomicBool = AtomicBool::new(false);
    if !ASKED.swap(true, Ordering::SeqCst) {
        let owner = with_fake(|f| f.owner);
        assert!(fuse::stop(index, owner, true).is_ok(), "stop while queued");
    }
    serve(index);
}

/// A request already queued when the stop arrives is answered first; only
/// then does the daemon hear `Stop`.
pub fn queued_request_answered_first() -> Result<(), String> {
    let (index, owner) = setup_media("STICK")?;
    let result = (|| {
        // The mount root's attributes are cached, so the lookup below is
        // the one request a path walk costs.
        vfsapi::abi_stat(ROOT, "/media/STICK").map_err(|e| format!("root: {e:?}"))?;
        let before = with_fake(|f| f.served);
        test_hook::set_server(Some(stop_then_serve));
        // One lookup, queued before the stop is asked for (from inside its
        // own wait): answered, with the daemon's own verdict, not refused.
        expect(
            vfsapi::abi_stat(ROOT, "/media/STICK/absent"),
            FsError::NotFound,
            "a request queued before the stop",
        )?;
        let served = with_fake(|f| f.served);
        check!(
            served == before + 1,
            "the daemon served {} requests, wanted 1",
            served - before
        );
        check!(next_of(index, owner) == Ok(Next::Stop), "NEXT after it");
        // Everything after it is refused.
        expect(
            vfsapi::abi_stat(ROOT, "/media/STICK/other"),
            FsError::Io,
            "a request after the stop",
        )?;
        check!(
            with_fake(|f| f.served) == served,
            "a refused request was served"
        );
        check!(fuse::unregister(index, owner).is_ok(), "unregister");
        Ok(())
    })();
    teardown();
    result
}

/// A task with `uid` and `caps`, made current.
fn task_as(uid: u32, caps: u32) -> Result<usize, String> {
    task::harness::switch_current(task::KERNEL_TASK);
    let slot = task::spawn_fork().map_err(|e| format!("spawn: {e}"))?;
    credentials::set(slot, Cred::new(uid, uid, caps, 0, 0));
    task::harness::switch_current(slot);
    Ok(slot)
}

/// Syscall 35: `LIST`, `STOP` and `HOLDERS` through the gate, with the same
/// gate as every other op, short buffers, a name cut inside a character, and
/// an owner that does not match.
pub fn supervise_syscalls() -> Result<(), String> {
    let (index, owner) = setup_media("STICK")?;
    let result = (|| {
        let call = fusesys::dispatch;
        let (id, pid) = (index as u64, owner as u64);
        let mut words = [0u64; PROVIDER_WORDS * 2];
        let cap = (PROVIDER_WORDS * 16) as u64;
        let mut text = [0u8; 64];
        let at = text.as_mut_ptr() as u64;
        // The registrant is a provider daemon (`ftpfuse`, `smbfuse` or
        // `fatfuse` in life): it may not supervise. Nor may a session user
        // holding the capability, or `_mountd`'s uid without it. Only
        // `mountd` by its program (root here) may.
        for op in [sys_op::LIST, sys_op::STOP, sys_op::HOLDERS] {
            check!(
                call(op, id, at, 64, pid) == failed(EPERM),
                "op {op} by the registrant itself"
            );
        }
        for (uid, caps) in [(1000u32, CAP_FS_PROVIDER), (mounttable::MOUNTD_UID, 0)] {
            task_as(uid, caps)?;
            for op in [sys_op::LIST, sys_op::STOP, sys_op::HOLDERS] {
                check!(
                    call(op, id, at, 64, pid) == failed(EPERM),
                    "op {op} by uid {uid} caps {caps:#x}"
                );
            }
        }
        task_as(0, CAP_FS_PROVIDER)?;

        // LIST: one record; a buffer of one word fits none.
        let got = call(sys_op::LIST, words.as_mut_ptr() as u64, cap, 0, 0);
        check!(got == 1, "LIST returned {got:#x}");
        let record: [u64; PROVIDER_WORDS] = words[..PROVIDER_WORDS]
            .try_into()
            .map_err(|_| "record size")?;
        check!(
            ProviderInfo::from_words(&record).is_some_and(|i| i.name == "STICK" && i.owner == pid),
            "LIST record {record:?}"
        );
        check!(
            call(sys_op::LIST, words.as_mut_ptr() as u64, 8, 0, 0) == 0,
            "LIST into a buffer of one word"
        );

        // HOLDERS: empty; then with an open file, then cut to a short buffer.
        let none = call(sys_op::HOLDERS, id, at, 64, pid);
        check!(none == 0, "HOLDERS of an idle volume: {none:#x}");
        vfsapi::abi_create(ROOT, "/media/STICK/log", 0o644)
            .map_err(|e| format!("create: {e:?}"))?;
        let file = OpenFile::open("/media/STICK/log", true, true, false)
            .map_err(|e| format!("open: {e:?}"))?;
        let want = "(mapped): /media/STICK/log\n";
        let got = call(sys_op::HOLDERS, id, at, 64, pid);
        check!(
            got == want.len() as u64 && &text[..want.len()] == want.as_bytes(),
            "HOLDERS: {got}"
        );
        // A buffer too small for the only line returns a cut line, never
        // overruns.
        let got = call(sys_op::HOLDERS, id, at, 5, pid);
        check!(got == 5, "HOLDERS into 5 bytes: {got:#x}");
        check!(
            call(sys_op::HOLDERS, 99, at, 64, pid) == failed(ESRCH),
            "HOLDERS of no provider"
        );
        // An id another daemon has taken since the listing: the owner does
        // not match.
        check!(
            call(sys_op::HOLDERS, id, at, 64, pid + 1) == failed(ESRCH),
            "HOLDERS with the wrong owner"
        );
        check!(
            call(sys_op::STOP, id, 0, pid + 1, 0) == failed(ESRCH),
            "STOP with the wrong owner"
        );
        drop(file);

        // A name from a stick is any Unicode and the cut can fall inside a
        // character: the report is cut at a boundary, never panics.
        let name = format!("/media/STICK/{}.txt", "\u{e9}".repeat(30));
        vfsapi::abi_create(ROOT, &name, 0o644).map_err(|e| format!("create: {e:?}"))?;
        let held = OpenFile::open(&name, true, true, false).map_err(|e| format!("open: {e:?}"))?;
        let got = call(sys_op::HOLDERS, id, at, 64, pid);
        check!(
            got > 0 && got <= 64 && core::str::from_utf8(&text[..got as usize]).is_ok(),
            "HOLDERS cut inside a character: {got}"
        );
        drop(held);

        // STOP: busy, then asked; the registrant's NEXT reports it.
        let file = OpenFile::open("/media/STICK/log", true, true, false)
            .map_err(|e| format!("open: {e:?}"))?;
        check!(
            call(sys_op::STOP, id, 0, pid, 0) == failed(EBUSY),
            "STOP busy"
        );
        check!(
            call(sys_op::STOP, 99, 0, pid, 0) == failed(ESRCH),
            "STOP of no provider"
        );
        drop(file);
        check!(call(sys_op::STOP, id, 0, pid, 0) == 0, "STOP");
        task::harness::switch_current(owner);
        let mut req = [0u64; fused::wire::REQUEST_WORDS];
        let mut data = [0u8; 16];
        let packed = data.len() as u64;
        let got = call(
            sys_op::NEXT,
            id,
            req.as_mut_ptr() as u64,
            data.as_mut_ptr() as u64,
            packed,
        );
        check!(got == STOP_PENDING, "NEXT after STOP: {got:#x}");
        check!(fuse::unregister(index, owner).is_ok(), "unregister");
        Ok(())
    })();
    teardown();
    result
}

/// Many stops, refusals and re-registrations: nothing leaks (`stopping` is
/// cleared at every registration, the listing has no stale entry).
pub fn stop_soak() -> Result<(), String> {
    let (index, owner) = setup_media("STICK")?;
    let result = (|| {
        let mut id = index;
        for round in 0..1000usize {
            check!(
                fuse::list().len() == 1,
                "{round}: listed {:?}",
                fuse::list()
            );
            let file = (round % 2 == 0)
                .then(|| {
                    let path = format!("/media/STICK/f{round}");
                    vfsapi::abi_create(ROOT, &path, 0o644).ok()?;
                    OpenFile::open(&path, true, true, false).ok()
                })
                .flatten();
            let want = if file.is_some() {
                Err(StopError::Busy)
            } else {
                Ok(())
            };
            check!(
                fuse::stop(id, owner, false) == want,
                "{round}: stop, wanted {want:?}"
            );
            if file.is_some() {
                drop(file);
                check!(
                    fuse::stop(id, owner, false).is_ok(),
                    "{round}: stop after close"
                );
            }
            check!(next_of(id, owner) == Ok(Next::Stop), "{round}: NEXT");
            check!(fuse::unregister(id, owner).is_ok(), "{round}: unregister");
            check!(fuse::list().is_empty(), "{round}: still listed");
            id = fuse::register(owner, "STICK", MountFlags::default(), true)
                .map_err(|e| format!("{round}: register: {e:?}"))?;
            with_fake(|f| f.index = id);
        }
        check!(fuse::unregister(id, owner).is_ok(), "final unregister");
        Ok(())
    })();
    teardown();
    result
}
