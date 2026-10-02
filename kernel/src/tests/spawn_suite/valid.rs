//! Well-formed `spawnv` requests: what each personality's child receives,
//! the credential stamps, and the legacy spawn's equivalent `argv`.

use super::*;
use crate::ipc::labels;

/// Arguments with spaces, an empty string and UTF-8.
const ARGV: [&[u8]; 6] = [
    b"prog",
    b"two words",
    b"",
    "\u{fc}n\u{ef}c\u{f8}d\u{e9} \u{2713}".as_bytes(),
    b"  padded  ",
    b"--opt=a=b",
];
/// An environment with a space in a value and an empty value.
const ENVP: [&[u8]; 3] = [b"HOME=/home/a b", b"EMPTY=", b"LANG=C.UTF-8"];

/// A native child reads exactly the blocks it was given through syscall 9,
/// is the caller's child, and reaping it frees its blocks.
pub fn native_argv_env_reach_child() -> Result<(), String> {
    fresh()?;
    let request = Req::new(NATIVE, &ARGV, &ENVP, false);
    let slot = spawned(request.call())?;
    check!(
        task::process::ppid_of(slot) == task::KERNEL_TASK,
        "the child is not the caller's"
    );
    let argv = child_block(slot, 0);
    check!(argv == request.argv, "argv block was {argv:?}");
    let envp = child_block(slot, 1);
    check!(envp == request.envp, "envp block was {envp:?}");
    // A short buffer still reports the full length, copying a prefix.
    task::harness::switch_current(slot);
    let mut small = [0u8; 4];
    let len = process::dispatch_for_test(SYS_ARGS, small.as_mut_ptr() as u64, 4, 0);
    let bad = process::dispatch_for_test(SYS_ARGS, 0, 0, 2);
    task::harness::switch_current(task::KERNEL_TASK);
    check!(
        len == request.argv.len() as u64 && small == request.argv[..4],
        "short read returned {len} / {small:?}"
    );
    check!(bad == failed(22), "an unknown selector returned {bad:#x}");
    reap(slot)?;
    check!(
        process::task_args_live_for_test() == 0,
        "the reaped child's blocks were kept"
    );
    Ok(())
}

/// A Linux child finds `argv` and `envp` on its start stack exactly as given
/// (no re-split, any bytes, a string across a page boundary), and syscall 9
/// holds nothing for it.
pub fn linux_argv_env_reach_child() -> Result<(), String> {
    fresh()?;
    let mut argv: Vec<&[u8]> = ARGV.to_vec();
    argv.push(b"\xff\xfe not utf-8");
    // Together over a page, so a string straddles a stack page boundary: the
    // loader must continue it in the next page's frame (it used to write
    // past the end of the first one).
    let long: Vec<u8> = (0..3000u32).map(|i| b'a' + (i % 26) as u8).collect();
    argv.push(&long);
    let pad: Vec<u8> = b"PAD=".iter().copied().chain([b'p'; 2000]).collect();
    let mut envp: Vec<&[u8]> = ENVP.to_vec();
    envp.push(&pad);
    let request = Req::new(LINUX, &argv, &envp, true);
    let slot = spawned(request.call())?;
    let (got_argv, got_envp) = linux_start_stack(slot)?;
    check!(
        got_argv.iter().map(Vec::as_slice).eq(argv.iter().copied()),
        "start-stack argv was {got_argv:?}"
    );
    check!(
        got_envp.iter().map(Vec::as_slice).eq(envp.iter().copied()),
        "start-stack envp was {got_envp:?}"
    );
    check!(
        process::task_args_live_for_test() == 0,
        "a Linux child got a syscall 9 block"
    );
    reap(slot)?;
    Ok(())
}

/// A path with spaces spawns under both personalities and is not split: the
/// same path without its tail is not a program.
pub fn path_with_space_spawns() -> Result<(), String> {
    fresh()?;
    for (path, linux) in [(NATIVE, false), (LINUX, true)] {
        let slot = spawned(Req::new(path, &[b"x"], &[], linux).call())
            .map_err(|e| format!("{path}: {e}"))?;
        reap(slot)?;
    }
    let head = Req::new("/tmp/spawn", &[b"x"], &[], false).call();
    check!(head == failed(2), "a path prefix spawned: {head:#x}");
    Ok(())
}

/// `As` stamps the requested credential, `AsLabelled` also assigns the label,
/// for a caller holding `CAP_SETUID` (the root kernel task).
pub fn cred_stamps_child() -> Result<(), String> {
    fresh()?;
    let mut request = Req::new(NATIVE, &[b"svc"], &[], false);
    request.mode = cred_mode::AS;
    request.cred = Cred::new(1000, 1001, 0, 0, 7);
    let slot = spawned(request.call())?;
    let cred = credentials::of(slot);
    check!(
        (cred.uid, cred.gid, cred.session) == (1000, 1001, 7),
        "As stamped {cred:?}"
    );
    reap(slot)?;

    request.mode = cred_mode::AS_LABELLED;
    request.label = b"app:com.spawnv.ok".to_vec();
    let slot = spawned(request.call())?;
    let label = labels::lookup("app:com.spawnv.ok").ok_or("label not interned")?;
    check!(
        credentials::of(slot).label_id == label && credentials::of(slot).uid == 1000,
        "AsLabelled stamped {:?}",
        credentials::of(slot)
    );
    reap(slot)?;
    Ok(())
}

/// The command-line spawn (syscall 6) and a kernel boot spawn's argument
/// string produce the `argv` block a `spawnv` caller would have sent:
/// `argv[0]` then the whitespace-split arguments, and no environment.
pub fn legacy_spawn_equivalent_argv() -> Result<(), String> {
    fresh()?;
    let line = format!("{LEGACY}  -v   key=value \0");
    let slot = spawned(process::dispatch_for_test(6, line.as_ptr() as u64, 0, 0))?;
    let want = block(&[LEGACY.as_bytes(), b"-v", b"key=value"]);
    let argv = child_block(slot, 0);
    check!(argv == want, "legacy argv block was {argv:?}");
    check!(
        child_block(slot, 1).is_empty(),
        "legacy spawn had an environment"
    );
    // A boot spawn's string: `argv[0]` is the task name.
    process::set_service_args(slot, b"demo=1");
    let name = task::process::name_of(slot).ok_or("no task name")?;
    let want = block(&[name.as_bytes(), b"demo=1"]);
    let argv = child_block(slot, 0);
    check!(argv == want, "boot-spawn argv block was {argv:?}");
    reap(slot)?;
    Ok(())
}
