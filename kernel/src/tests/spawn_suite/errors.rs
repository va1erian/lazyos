//! Refused `spawnv` requests: each limit, malformed block, unmapped pointer
//! and unprivileged credential mode returns its errno and leaks nothing.

use super::*;
use crate::ipc::labels;
use crate::process::spawnv::{E2BIG, ENAMETOOLONG};

const EPERM: i64 = 1;
const ENOENT: i64 = 2;
const EFAULT: i64 = 14;
const EINVAL: i64 = 22;

/// Scratch user space for the pointer checks: the request block and every
/// buffer it points at, each in its own region.
const SPACE: u64 = 0x0040_0000;
const SPACE_PAGES: u64 = 5;
const REQ_AT: u64 = SPACE;
const PATH_AT: u64 = SPACE + 0x200;
const LABEL_AT: u64 = SPACE + 0x400;
const ENVP_AT: u64 = SPACE + 0x1000;
const ARGV_AT: u64 = SPACE + 0x3000;
/// The end of the mapped scratch space.
const SPACE_END: u64 = SPACE + SPACE_PAGES * 4096;
/// An address mapped in no test address space.
pub(super) const UNMAPPED: u64 = 0x0090_0000;

/// Issue `request`, expect `-errno`, and check nothing leaked.
fn refused(request: &Req, errno: i64, what: &str) -> Result<(), String> {
    let before = usage();
    let code = request.call();
    check!(
        code == failed(errno),
        "{what}: returned {code:#x}, expected -{errno}"
    );
    no_leak(before, usage(), 4096, what)
}

/// A valid request to mutate.
fn base() -> Req {
    Req::new(NATIVE, &[b"prog", b"arg"], &[b"K=V"], false)
}

/// Lengths and counts over the limits, at the limits' edges.
pub fn limit_errors_leak_nothing() -> Result<(), String> {
    fresh()?;
    let long_path = format!("/tmp/{}", "p".repeat(251));
    check!(
        long_path.len() == 256,
        "long path is {} bytes",
        long_path.len()
    );
    refused(
        &Req {
            path: long_path.into_bytes(),
            ..base()
        },
        ENAMETOOLONG,
        "256-byte path",
    )?;

    let args: Vec<&[u8]> = (0..65).map(|_| b"a".as_slice()).collect();
    refused(&Req::new(NATIVE, &args, &[], false), E2BIG, "argc 65")?;
    let env: Vec<&[u8]> = (0..65).map(|_| b"K=V".as_slice()).collect();
    refused(&Req::new(NATIVE, &[b"p"], &env, false), E2BIG, "envc 65")?;

    // 4097 bytes: one string of 4096 bytes plus its NUL.
    let big = vec![b'x'; 4096];
    refused(
        &Req::new(NATIVE, &[&big], &[], false),
        E2BIG,
        "4097-byte argv",
    )?;
    let mut big_env = vec![b'x'; 4096];
    big_env[0] = b'K';
    big_env[1] = b'=';
    refused(
        &Req::new(NATIVE, &[b"p"], &[&big_env], false),
        E2BIG,
        "4097-byte envp",
    )?;

    // Exactly at the limits is accepted: 64 strings in a 4096-byte block.
    let item = vec![b'y'; 63];
    let full: Vec<&[u8]> = (0..64).map(|_| item.as_slice()).collect();
    let request = Req::new(NATIVE, &full, &[], false);
    check!(
        request.argv.len() == 4096,
        "full block is {}",
        request.argv.len()
    );
    let slot = spawned(request.call()).map_err(|e| format!("at the limits: {e}"))?;
    check!(
        child_block(slot, 0) == request.argv,
        "full block did not round-trip"
    );
    reap(slot)
}

/// Blocks that do not match their counts, bad selectors, bad paths.
pub fn malformed_requests_einval() -> Result<(), String> {
    fresh()?;
    refused(&Req::new(NATIVE, &[], &[], false), EINVAL, "argc 0")?;
    let mut request = base();
    request.argv.pop();
    refused(&request, EINVAL, "argv missing its final NUL")?;
    refused(&Req { argc: 3, ..base() }, EINVAL, "argc 3 for 2 strings")?;
    refused(&Req { argc: 1, ..base() }, EINVAL, "argc 1 for 2 strings")?;
    refused(&Req { envc: 2, ..base() }, EINVAL, "envc 2 for 1 string")?;
    refused(&Req { envc: 0, ..base() }, EINVAL, "envc 0 with a block")?;
    refused(
        &Req::new(NATIVE, &[b"p"], &[b"NOEQUALS"], false),
        EINVAL,
        "env entry without =",
    )?;
    refused(
        &Req::new(NATIVE, &[b"p"], &[b"=v"], false),
        EINVAL,
        "env entry with an empty key",
    )?;
    refused(
        &Req::new(NATIVE, &[b"\xff"], &[], false),
        EINVAL,
        "native non-UTF-8 argv",
    )?;
    refused(
        &Req {
            personality: 2,
            ..base()
        },
        EINVAL,
        "personality 2",
    )?;
    refused(&Req { mode: 3, ..base() }, EINVAL, "cred mode 3")?;
    refused(
        &Req {
            path: b"/tmp/a\0b".to_vec(),
            ..base()
        },
        EINVAL,
        "NUL in path",
    )?;
    refused(
        &Req {
            path: b"/tmp/\xff".to_vec(),
            ..base()
        },
        EINVAL,
        "non-UTF-8 path",
    )?;
    refused(
        &Req {
            path: Vec::new(),
            ..base()
        },
        ENOENT,
        "empty path",
    )?;
    refused(
        &Req {
            path: b"/tmp/spawn suite/missing".to_vec(),
            ..base()
        },
        ENOENT,
        "missing file",
    )?;
    let mut labelled = base();
    labelled.mode = cred_mode::AS_LABELLED;
    refused(&labelled, EINVAL, "labelled spawn with no label")?;
    Ok(())
}

/// Copy at most `room` bytes of `bytes` into the installed scratch space at
/// `va`.
fn poke(va: u64, bytes: &[u8], room: u64) {
    let len = bytes.len().min(room as usize);
    // SAFETY: `va..va + len` lies in one region of the scratch pages
    // `in_space` mapped writable and installed (`room` is that region's size).
    unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), va as *mut u8, len) };
}

/// Run `f` with [`SPACE`] mapped in a fresh, installed address space and
/// pointer validation on, as a real call from a user task would.
pub(super) fn in_space<R>(f: impl FnOnce() -> Result<R, String>) -> Result<R, String> {
    let kernel = mem::kernel_table();
    let table = mem::new_user_table().ok_or("new_user_table failed")?;
    process::map_range(table, SPACE, SPACE_END).map_err(to_string)?;
    mem::switch_to(table);
    let trusted = crate::user_ptr::set_trust_kernel_pointers(false);
    let outcome = f();
    crate::user_ptr::set_trust_kernel_pointers(trusted);
    mem::switch_to(kernel);
    mem::free_user_table(table);
    outcome
}

/// Lay `request` out in user memory (inside [`in_space`]), apply `edit` to
/// the request words, and call.
pub(super) fn call_placed(request: &Req, edit: impl Fn(&mut [u64; REQ_WORDS])) -> u64 {
    poke(PATH_AT, &request.path, LABEL_AT - PATH_AT);
    poke(LABEL_AT, &request.label, ENVP_AT - LABEL_AT);
    poke(ENVP_AT, &request.envp, ARGV_AT - ENVP_AT);
    poke(ARGV_AT, &request.argv, SPACE_END - ARGV_AT);
    let mut words = request.words();
    (words[0], words[2], words[5], words[15]) = (PATH_AT, ARGV_AT, ENVP_AT, LABEL_AT);
    edit(&mut words);
    let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
    poke(REQ_AT, &bytes, PATH_AT - REQ_AT);
    process::dispatch_for_test(SYS_SPAWNV, REQ_AT, 0, 0)
}

/// An unmapped or kernel pointer anywhere in the request is `-EFAULT`; the
/// same request in user memory gets past every pointer check.
pub fn unmapped_memory_efault() -> Result<(), String> {
    fresh()?;
    let kernel_buf = [0u8; 64];
    let kernel_ptr = kernel_buf.as_ptr() as u64;
    let placed = base();
    let before = usage();
    in_space(|| -> Result<(), String> {
        let cases: [(&str, u64); 6] = [
            (
                "request",
                process::dispatch_for_test(SYS_SPAWNV, UNMAPPED, 0, 0),
            ),
            (
                "kernel request",
                process::dispatch_for_test(SYS_SPAWNV, kernel_ptr, 0, 0),
            ),
            (
                "null request",
                process::dispatch_for_test(SYS_SPAWNV, 0, 0, 0),
            ),
            ("path", call_placed(&placed, |w| w[0] = UNMAPPED)),
            ("argv", call_placed(&placed, |w| w[2] = kernel_ptr)),
            ("envp", call_placed(&placed, |w| w[5] = UNMAPPED)),
        ];
        for (what, code) in cases {
            check!(code == failed(EFAULT), "{what}: returned {code:#x}");
        }
        // The straddling case: a block running off the end of the page.
        let code = call_placed(&placed, |w| w[2] = SPACE_END - 2);
        check!(
            code == failed(EFAULT),
            "straddling argv: returned {code:#x}"
        );
        // Control: valid user pointers reach the file lookup (a native
        // program needs the file; it exists, so this spawns).
        let code = call_placed(&placed, |_| {});
        let slot = spawned(code)?;
        reap(slot)
    })?;
    no_leak(before, usage(), 4096, "EFAULT requests")
}

/// The credential modes need `CAP_SETUID` as syscall 10's spawn ops do: an
/// unprivileged caller gets `-EPERM`, and a refused labelled spawn interns
/// nothing.
pub fn cred_without_capability_eperm() -> Result<(), String> {
    fresh()?;
    let me = task::current();
    credentials::set(me, Cred::new(1000, 1000, 0, 0, 0));
    let outcome = (|| -> Result<(), String> {
        let mut request = base();
        request.mode = cred_mode::AS;
        request.cred = Cred::new(1000, 1000, 0, 0, 0);
        refused(&request, EPERM, "As without CAP_SETUID")?;
        request.mode = cred_mode::AS_LABELLED;
        request.label = b"app:com.spawnv.denied".to_vec();
        refused(&request, EPERM, "AsLabelled without CAP_SETUID")?;
        check!(
            labels::lookup("app:com.spawnv.denied").is_none(),
            "an unprivileged caller interned a label"
        );
        // Inherit stays open to the unprivileged caller.
        let slot = spawned(base().call())?;
        check!(credentials::of(slot).uid == 1000, "Inherit changed the uid");
        reap(slot)
    })();
    credentials::reset_for_task(me);
    outcome
}
