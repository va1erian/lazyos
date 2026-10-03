//! `execve` of `#!` scripts (issue #491): the line parser, argv rewriting,
//! the recursion bound, the permission checks on every hop, the errors a bad
//! script or a missing interpreter returns through the syscall, and a soak.
//!
//! A successful `execve` replaces the caller's image, which the harness (the
//! kernel task) cannot survive, so the success paths drive
//! `shebang::resolve` directly: it is everything `sys_execve` does before it
//! builds the address space. The failure paths go through the real syscall.

use super::*;
use crate::fs::ramfs::RamFs;
use crate::fs::vfs::{AttrRequest, FsError, Id, MountFlags, Vfs};
use crate::ipc::credentials::{self, Cred};
use crate::process::linux::shebang::{self, Interp, MAX_DEPTH, MAX_LINE};
use alloc::sync::Arc;

const SYS_EXECVE: u64 = 59;
// Positive errnos (`resolve`'s parser reports these); these shadow the
// suite's negative `ENOENT`, and [`neg`] turns them into syscall returns.
const ENOENT: u64 = 2;
const ENOEXEC: u64 = 8;
const EACCES: u64 = 13;
const ELOOP: u64 = 40;

/// Stand-in for an interpreter binary: `resolve` hands back the file, it does
/// not load it, so any non-`#!` content is "the ELF".
const FAKE_ELF: &[u8] = b"\x7fELF fake interpreter";

fn neg(code: u64) -> u64 {
    code.wrapping_neg()
}

fn fs_error(error: FsError) -> String {
    String::from(error.message())
}

/// A clean caller (root) over a fresh ramfs ABI table.
fn setup() -> Result<(), String> {
    fresh()?;
    crate::fs::install_abi_ramfs_for_test();
    credentials::set(task::current(), Cred::ROOT);
    Ok(())
}

fn put(path: &str, mode: u16, data: &[u8]) -> Result<(), String> {
    crate::fs::abi_create(Id::current(), path, mode).map_err(fs_error)?;
    crate::fs::abi_write(Id::current(), path, 0, data).map_err(fs_error)?;
    Ok(())
}

fn c(text: &str) -> Vec<u8> {
    let mut out = Vec::from(text.as_bytes());
    out.push(0);
    out
}

/// `resolve` as `sys_execve` calls it for `path` run with `args`.
fn resolve(path: &str, args: &[&str]) -> Result<shebang::Image, u64> {
    let argv = args.iter().map(|arg| c(arg)).collect();
    shebang::resolve(String::from(path), Vec::from(path.as_bytes()), argv)
}

fn show(argv: &[Vec<u8>]) -> Vec<String> {
    argv.iter()
        .map(|arg| String::from_utf8_lossy(arg.strip_suffix(&[0]).unwrap_or(arg)).into_owned())
        .collect()
}

fn expect_argv(image: &shebang::Image, want: &[&str]) -> Result<(), String> {
    let got = show(&image.argv);
    check!(got == want, "argv {got:?}, want {want:?}");
    use crate::process::image::Image;
    let mut bytes = alloc::vec![0u8; image.file.len() as usize];
    image
        .file
        .read_exact_at(0, &mut bytes)
        .map_err(String::from)?;
    check!(bytes == FAKE_ELF, "resolved to the wrong file");
    Ok(())
}

/// `execve(path, [path], [])` through the syscall; returns its result.
fn execve(path: &str) -> u64 {
    let path = c(path);
    let argv = [path.as_ptr() as u64, 0];
    process::linux::dispatch_for_test(SYS_EXECVE, path.as_ptr() as u64, argv.as_ptr() as u64, 0)
}

/// The parser on its own: interpreter, the single argument, trimming, and
/// every malformed line.
pub fn shebang_parse_lines() -> Result<(), String> {
    let ok = |line: &[u8], path: &str, arg: Option<&[u8]>| -> Result<(), String> {
        let got = shebang::parse(line);
        check!(
            got == Ok(Some(Interp { path, arg })),
            "{:?} parsed to {got:?}",
            String::from_utf8_lossy(line)
        );
        Ok(())
    };
    ok(b"#!/bin/sh\necho hi\n", "/bin/sh", None)?;
    ok(b"#!/bin/rhai", "/bin/rhai", None)?;
    ok(b"#! \t/bin/sh  \t\n", "/bin/sh", None)?;
    ok(b"#!/bin/sh -e\n", "/bin/sh", Some(b"-e"))?;
    ok(
        b"#!/usr/bin/env  rhai -x  \n",
        "/usr/bin/env",
        Some(b"rhai -x"),
    )?;
    ok(b"#!/bin/sh\r\n", "/bin/sh", None)?;
    ok(b"#!/bin/sh \xff\xfe\n", "/bin/sh", Some(b"\xff\xfe"))?;

    for not_script in [&b""[..], b"#", b"\x7fELF", b" #!/bin/sh\n", b"#/bin/sh\n"] {
        check!(
            shebang::parse(not_script) == Ok(None),
            "{not_script:?} taken for a script"
        );
    }
    let mut long = Vec::from(&b"#!/"[..]);
    long.resize(MAX_LINE + 10, b'a');
    long.push(b'\n');
    let mut fits = Vec::from(&b"#!/"[..]);
    fits.resize(MAX_LINE - 1, b'a');
    fits.push(b'\n');
    check!(
        shebang::parse(&fits).is_ok_and(|i| i.is_some()),
        "a {MAX_LINE}-byte line was refused"
    );
    // The same name with no newline is fine when the file ends there.
    check!(
        shebang::parse(&fits[..fits.len() - 1]).is_ok_and(|i| i.is_some()),
        "an unterminated last line"
    );
    for bad in [
        &b"#!\n"[..],
        b"#!   \t\n",
        b"#!",
        b"#!/bin/s\0h\n",
        b"#!/bin/sh \0arg\n",
        b"#!/bin/\xffsh\n",
        &long,
    ] {
        check!(
            shebang::parse(bad) == Err(ENOEXEC),
            "{:?} gave {:?}, want ENOEXEC",
            String::from_utf8_lossy(&bad[..bad.len().min(24)]),
            shebang::parse(bad)
        );
    }
    Ok(())
}

/// A script and a script with an argument rewrite argv the Linux way, and the
/// interpreter gets the script name as the caller spelled it.
pub fn shebang_rewrites_argv() -> Result<(), String> {
    setup()?;
    put("/tmp/interp", 0o755, FAKE_ELF)?;
    put("/tmp/s.sh", 0o755, b"#!/tmp/interp\necho hi\n")?;
    put("/tmp/a.sh", 0o755, b"#!/tmp/interp -e -x\n")?;

    let image =
        resolve("/tmp/s.sh", &["/tmp/s.sh", "one", "two words"]).map_err(|e| format!("{e:#x}"))?;
    expect_argv(&image, &["/tmp/interp", "/tmp/s.sh", "one", "two words"])?;
    let image = resolve("/tmp/a.sh", &["a"]).map_err(|e| format!("{e:#x}"))?;
    expect_argv(&image, &["/tmp/interp", "-e -x", "/tmp/a.sh"])?;
    // An empty argv: the script name still reaches the interpreter.
    let image = resolve("/tmp/s.sh", &[]).map_err(|e| format!("{e:#x}"))?;
    expect_argv(&image, &["/tmp/interp", "/tmp/s.sh"])?;
    // Not a script: argv and the bytes pass through untouched.
    let image = resolve("/tmp/interp", &["x", "y"]).map_err(|e| format!("{e:#x}"))?;
    expect_argv(&image, &["x", "y"])?;
    // A relative interpreter resolves against the working directory.
    put("/tmp/rel.sh", 0o755, b"#!tmp/interp\n")?;
    let image = resolve("/tmp/rel.sh", &["r"]).map_err(|e| format!("{e:#x}"))?;
    expect_argv(&image, &["tmp/interp", "/tmp/rel.sh"])?;
    Ok(())
}

/// Interpreters that are scripts are followed up to `MAX_DEPTH` hops; one more
/// is `ELOOP`, and a script naming itself is too.
pub fn shebang_recursion_limit() -> Result<(), String> {
    setup()?;
    put("/tmp/interp", 0o755, FAKE_ELF)?;
    // l0 -> l1 -> ... -> l{MAX_DEPTH} -> interp: MAX_DEPTH + 1 scripts.
    let mut next = String::from("/tmp/interp");
    for level in (0..=MAX_DEPTH).rev() {
        let name = format!("/tmp/l{level}");
        put(&name, 0o755, format!("#!{next}\n").as_bytes())?;
        next = name;
    }
    // Starting at l1 is MAX_DEPTH hops: allowed, and every script is in argv.
    let image = resolve("/tmp/l1", &["l1", "arg"]).map_err(|e| format!("{e:#x}"))?;
    check!(MAX_DEPTH == 4, "the expected argv below assumes four hops");
    expect_argv(
        &image,
        &[
            "/tmp/interp",
            "/tmp/l4",
            "/tmp/l3",
            "/tmp/l2",
            "/tmp/l1",
            "arg",
        ],
    )?;
    // Starting at l0 is one hop too many.
    check!(
        resolve("/tmp/l0", &["l0"]).err() == Some(neg(ELOOP)),
        "{} script hops did not give ELOOP",
        MAX_DEPTH + 1
    );
    check!(execve("/tmp/l0") == neg(ELOOP), "execve of the chain");
    put("/tmp/self.sh", 0o755, b"#!/tmp/self.sh\n")?;
    check!(execve("/tmp/self.sh") == neg(ELOOP), "a self-naming script");
    Ok(())
}

/// What the caller sees from `execve` for broken scripts.
pub fn shebang_execve_errors() -> Result<(), String> {
    setup()?;
    put("/tmp/missing.sh", 0o755, b"#!/tmp/no-such-interpreter\n")?;
    put("/tmp/empty.sh", 0o755, b"#!  \n")?;
    put("/tmp/nul.sh", 0o755, b"#!/tmp/in\0terp\n")?;
    put("/tmp/utf.sh", 0o755, b"#!/tmp/\xc3\x28\n")?;
    let mut long = Vec::from(&b"#!/tmp/"[..]);
    long.resize(MAX_LINE * 2, b'x');
    long.push(b'\n');
    put("/tmp/long.sh", 0o755, &long)?;
    for (path, want, what) in [
        ("/tmp/missing.sh", ENOENT, "a missing interpreter"),
        ("/tmp/empty.sh", ENOEXEC, "an empty line"),
        ("/tmp/nul.sh", ENOEXEC, "a NUL in the line"),
        ("/tmp/utf.sh", ENOEXEC, "a non-UTF-8 interpreter"),
        ("/tmp/long.sh", ENOEXEC, "an over-long line"),
    ] {
        let got = execve(path);
        check!(got == neg(want), "{what}: execve -> {got:#x}, want -{want}");
    }
    Ok(())
}

/// The execute bit and `noexec` hold for the script *and* the interpreter.
pub fn shebang_permissions_every_hop() -> Result<(), String> {
    setup()?;
    let mut table = Vfs::new();
    let locked = MountFlags {
        noexec: true,
        ..MountFlags::default()
    };
    for point in [fhs::mount::ROOT, fhs::mount::TMP] {
        table
            .mount(point, Arc::new(RamFs::new()), MountFlags::default())
            .map_err(fs_error)?;
    }
    table
        .mount("/mnt", Arc::new(RamFs::new()), locked)
        .map_err(fs_error)?;
    let previous = crate::fs::install_abi_for_test(table);
    let result = permission_cases();
    crate::fs::restore_abi_for_test(previous);
    credentials::set(task::current(), Cred::ROOT);
    result
}

fn permission_cases() -> Result<(), String> {
    put("/tmp/interp", 0o755, FAKE_ELF)?;
    put("/tmp/plain", 0o644, FAKE_ELF)?;
    put("/mnt/interp", 0o755, FAKE_ELF)?;
    put("/tmp/ok.sh", 0o755, b"#!/tmp/interp\n")?;
    put("/tmp/noexec-bit.sh", 0o644, b"#!/tmp/interp\n")?;
    put("/tmp/to-plain.sh", 0o755, b"#!/tmp/plain\n")?;
    put("/tmp/to-mnt.sh", 0o755, b"#!/mnt/interp\n")?;
    put("/mnt/s.sh", 0o755, b"#!/tmp/interp\n")?;

    // noexec binds root too.
    check!(
        execve("/mnt/s.sh") == neg(EACCES),
        "a script on a noexec mount"
    );
    check!(
        execve("/tmp/to-mnt.sh") == neg(EACCES),
        "an interpreter on a noexec mount"
    );

    credentials::set(task::current(), Cred::new(1000, 100, 0, 0, 0));
    check!(
        resolve("/tmp/ok.sh", &["ok"]).is_ok(),
        "an executable script was refused"
    );
    check!(
        execve("/tmp/noexec-bit.sh") == neg(EACCES),
        "a script without +x"
    );
    check!(
        execve("/tmp/to-plain.sh") == neg(EACCES),
        "an interpreter without +x"
    );

    // chmod +x makes the script runnable, as in `chmod +x s.sh; ./s.sh`.
    credentials::set(task::current(), Cred::ROOT);
    crate::fs::abi_setattr(
        Id::current(),
        "/tmp/noexec-bit.sh",
        AttrRequest::Mode(0o755),
    )
    .map_err(fs_error)?;
    credentials::set(task::current(), Cred::new(1000, 100, 0, 0, 0));
    check!(
        resolve("/tmp/noexec-bit.sh", &["s"]).is_ok(),
        "chmod +x did not make it runnable"
    );
    Ok(())
}

/// Many resolutions, good and bad, interleaved: no frames leak from the
/// failing ones and every round gets the same answer.
pub fn shebang_soak() -> Result<(), String> {
    setup()?;
    put("/tmp/interp", 0o755, FAKE_ELF)?;
    put("/tmp/good.sh", 0o755, b"#!/tmp/interp -q\n")?;
    put("/tmp/gone.sh", 0o755, b"#!/tmp/gone\n")?;
    put("/tmp/loop.sh", 0o755, b"#!/tmp/loop.sh\n")?;
    // Warm-up so lazily grown tables are counted in the baseline.
    let _ = resolve("/tmp/good.sh", &["g"]);
    let _ = execve("/tmp/gone.sh");
    let _ = execve("/tmp/loop.sh");
    let frames = crate::mem::frame_stats().live();
    for round in 0..2000 {
        let image =
            resolve("/tmp/good.sh", &["g", "x"]).map_err(|e| format!("round {round}: {e:#x}"))?;
        check!(
            image.argv.len() == 4,
            "round {round}: argv {:?}",
            show(&image.argv)
        );
        check!(
            execve("/tmp/gone.sh") == neg(ENOENT),
            "round {round}: missing interpreter"
        );
        check!(execve("/tmp/loop.sh") == neg(ELOOP), "round {round}: loop");
    }
    let after = crate::mem::frame_stats().live();
    check!(
        after == frames,
        "the soak leaked {} frames",
        after as i64 - frames as i64
    );
    Ok(())
}
