//! Dynamic linking (issue #535, gap 1): a program with `PT_INTERP` is loaded
//! beside its interpreter, which starts and finds the program through
//! `AT_PHDR`/`AT_ENTRY`; `AT_BASE` says where it sits. Covered: the
//! `PT_INTERP` parser's refusals, the layout and auxiliary vector for a PIE
//! and a fixed-address program, static images left alone, every interpreter
//! failure, `/lib` lookups served from `/system/lib`, and a soak.

use super::stream::setup_ramfs;
use super::*;
use crate::fs::vfs::Id;
use crate::process::elfhdr;
use crate::process::linux::{load_image, nul_terminated, LoadError, INTERP_BASE, PIE_BASE};

const PT_INTERP: u32 = 3;
const ET_EXEC: u16 = 2;
const ET_DYN: u16 = 3;
const PAGE: u64 = 4096;
const AT_PHDR: u64 = 3;
const AT_BASE: u64 = 7;
const AT_ENTRY: u64 = 9;

/// A synthetic ELF64 of type `etype`: one `PT_LOAD` over the file's first
/// `0x2000` bytes (headers included) at `vaddr`, and `interp` as `PT_INTERP`.
fn image(etype: u16, vaddr: u64, entry: u64, interp: Option<&[u8]>) -> Vec<u8> {
    let phnum = 1 + usize::from(interp.is_some());
    let interp_at = 0x800u64;
    let mut out = Vec::new();
    out.extend_from_slice(&[0x7f, b'E', b'L', b'F', 2, 1, 1, 0]);
    out.extend_from_slice(&[0; 8]);
    out.extend_from_slice(&etype.to_le_bytes());
    out.extend_from_slice(&0x3eu16.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&entry.to_le_bytes());
    out.extend_from_slice(&64u64.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&64u16.to_le_bytes());
    out.extend_from_slice(&56u16.to_le_bytes());
    out.extend_from_slice(&(phnum as u16).to_le_bytes());
    out.extend_from_slice(&[0; 6]);
    let header = |out: &mut Vec<u8>, kind: u32, flags: u32, off: u64, va: u64, filesz: u64| {
        out.extend_from_slice(&kind.to_le_bytes());
        out.extend_from_slice(&flags.to_le_bytes());
        out.extend_from_slice(&off.to_le_bytes());
        out.extend_from_slice(&va.to_le_bytes());
        out.extend_from_slice(&va.to_le_bytes());
        out.extend_from_slice(&filesz.to_le_bytes());
        out.extend_from_slice(&filesz.to_le_bytes());
        out.extend_from_slice(&0x1000u64.to_le_bytes());
    };
    if let Some(path) = interp {
        header(&mut out, PT_INTERP, PF_R, interp_at, 0, path.len() as u64);
    }
    header(&mut out, 1, PF_R | PF_X, 0, vaddr, 0x2000);
    out.resize(0x2000, 0);
    if let Some(path) = interp {
        out[interp_at as usize..interp_at as usize + path.len()].copy_from_slice(path);
    }
    out
}

/// `/system/lib` with `name` holding `elf` on the test ramfs.
fn install_lib(name: &str, elf: &[u8]) -> Result<(), String> {
    let root = Id::ROOT;
    let error = |e: crate::fs::vfs::FsError| format!("seed {name}: {}", e.message());
    for dir in [fhs::SYSTEM, fhs::SYSTEM_LIB] {
        let _ = crate::fs::abi_mkdir(root, dir, 0o755);
    }
    let path = format!("{}/{name}", fhs::SYSTEM_LIB);
    crate::fs::abi_create(root, &path, 0o755).map_err(error)?;
    crate::fs::abi_write(root, &path, 0, elf).map_err(error)?;
    Ok(())
}

/// Read a little-endian word of the new address space at `va`.
fn word(table: PhysAddr, va: u64) -> Result<u64, String> {
    let frame = frame_of(table, va & !(PAGE - 1))?;
    let ptr = mem::phys_to_virt(PhysAddr::new(frame)).as_ptr::<u64>();
    // SAFETY: the frame is mapped by the physical-memory window and `va` is
    // 8-aligned inside it.
    Ok(unsafe { ptr.add(((va & (PAGE - 1)) / 8) as usize).read_volatile() })
}

/// The auxiliary vector of the start stack at `rsp`, as `(type, value)`.
fn auxv(table: PhysAddr, rsp: u64) -> Result<Vec<(u64, u64)>, String> {
    let argc = word(table, rsp)?;
    let mut at = rsp + 8 * (argc + 1);
    while word(table, at)? != 0 {
        at += 8; // envp
    }
    at += 8;
    let mut out = Vec::new();
    loop {
        let (kind, value) = (word(table, at)?, word(table, at + 8)?);
        if kind == 0 {
            return Ok(out);
        }
        out.push((kind, value));
        at += 16;
    }
}

fn aux(vector: &[(u64, u64)], kind: u64) -> Option<u64> {
    vector.iter().find(|(k, _)| *k == kind).map(|(_, v)| *v)
}

fn start(table: PhysAddr, elf: &[u8]) -> Result<crate::process::linux::Started, LoadError> {
    let argv = nul_terminated(&[b"prog"]);
    let envp = nul_terminated(&[b"A=1"]);
    load_image(table, elf, &argv, &envp, (1000, 1000), 0)
}

/// `PT_INTERP` is read and refused when malformed.
pub fn interpreter_parsing() -> Result<(), String> {
    let good = image(ET_DYN, 0, 0x1000, Some(b"/lib/ld-musl-x86_64.so.1\0"));
    let headers = elfhdr::read(&good[..]).map_err(String::from)?;
    check!(
        elfhdr::interpreter(&good[..], &headers) == Ok(Some(String::from("/lib/ld-musl-x86_64.so.1"))),
        "a valid PT_INTERP was not read"
    );
    check!(headers.etype == ET_DYN, "e_type {}", headers.etype);
    let none = image(ET_DYN, 0, 0x1000, None);
    let headers = elfhdr::read(&none[..]).map_err(String::from)?;
    check!(
        elfhdr::interpreter(&none[..], &headers) == Ok(None),
        "a static image has an interpreter"
    );
    let refusals: [(&str, &[u8]); 3] = [
        ("unterminated", b"/lib/ld.so"),
        ("empty", b"\0\0"),
        ("not UTF-8", b"/lib/\xff\xfe\0"),
    ];
    for (name, path) in refusals {
        let elf = image(ET_DYN, 0, 0x1000, Some(path));
        let headers = elfhdr::read(&elf[..]).map_err(String::from)?;
        check!(
            elfhdr::interpreter(&elf[..], &headers).is_err(),
            "{name}: the interpreter path was accepted"
        );
    }
    // A path longer than PATH_MAX and an offset outside the file.
    let mut long = image(ET_DYN, 0, 0x1000, Some(b"/x\0"));
    let mut headers = elfhdr::read(&long[..]).map_err(String::from)?;
    headers.phdrs[0].filesz = 4097;
    check!(
        elfhdr::interpreter(&long[..], &headers).is_err(),
        "a path over PATH_MAX was accepted"
    );
    headers.phdrs[0].filesz = 8;
    headers.phdrs[0].offset = long.len() as u64 - 4;
    check!(
        elfhdr::interpreter(&long[..], &headers).is_err(),
        "a path outside the file was accepted"
    );
    // Two PT_INTERP headers.
    headers.phdrs.push(headers.phdrs[0]);
    headers.phdrs[0].offset = 0x800;
    headers.phdrs[0].filesz = 3;
    long.truncate(0x2000);
    check!(
        elfhdr::interpreter(&long[..], &headers).is_err(),
        "two PT_INTERP headers were accepted"
    );
    Ok(())
}

/// A dynamically linked PIE: the program at [`PIE_BASE`], the interpreter at
/// [`INTERP_BASE`], the entry in the interpreter and the vector naming both.
pub fn pie_loads_beside_its_interpreter() -> Result<(), String> {
    setup_ramfs();
    install_lib("ld-test.so", &image(ET_DYN, 0, 0x1200, None))?;
    let main = image(ET_DYN, 0, 0x1100, Some(b"/lib/ld-test.so\0"));
    with_table(|table| {
        let started = start(table, &main).map_err(|e| format!("{e:?}"))?;
        check!(
            started.entry == INTERP_BASE + 0x1200,
            "starts at {:#x}, not in the interpreter",
            started.entry
        );
        let vector = auxv(table, started.rsp)?;
        check!(aux(&vector, AT_BASE) == Some(INTERP_BASE), "AT_BASE {vector:x?}");
        check!(
            aux(&vector, AT_ENTRY) == Some(PIE_BASE + 0x1100),
            "AT_ENTRY {vector:x?}"
        );
        check!(
            aux(&vector, AT_PHDR) == Some(PIE_BASE + 64),
            "AT_PHDR {vector:x?}"
        );
        check!(
            started.brk == PIE_BASE + 0x2000,
            "brk {:#x} is not past the program",
            started.brk
        );
        for (va, what) in [(PIE_BASE, "program"), (INTERP_BASE, "interpreter")] {
            let vma = mem::vma::find(table, va).ok_or(format!("{what} not mapped"))?;
            check!(!vma.prot.has_write() && vma.prot.has_exec(), "{what}: {vma:?}");
            check!(raw_entry(table, va).is_some(), "{what}: page not present");
        }
        check!(
            raw_entry(table, 0).is_none(),
            "page zero is mapped: a PIE must move off it"
        );
        Ok(())
    })
}

/// A fixed-address program keeps its addresses; a PIE without an interpreter
/// (static-PIE) loads at zero with `AT_BASE` 0, as before.
pub fn static_images_are_unchanged() -> Result<(), String> {
    setup_ramfs();
    install_lib("ld-test.so", &image(ET_DYN, 0, 0x1200, None))?;
    let fixed = image(ET_EXEC, 0x40_0000, 0x40_1100, Some(b"/lib/ld-test.so\0"));
    with_table(|table| {
        let started = start(table, &fixed).map_err(|e| format!("{e:?}"))?;
        check!(started.entry == INTERP_BASE + 0x1200, "entry {:#x}", started.entry);
        let vector = auxv(table, started.rsp)?;
        check!(
            aux(&vector, AT_ENTRY) == Some(0x40_1100) && aux(&vector, AT_PHDR) == Some(0x40_0040),
            "fixed program moved: {vector:x?}"
        );
        check!(
            aux(&vector, AT_BASE) == Some(INTERP_BASE),
            "AT_BASE {vector:x?}"
        );
        Ok(())
    })?;
    let static_pie = image(ET_DYN, 0, 0x1100, None);
    with_table(|table| {
        let started = start(table, &static_pie).map_err(|e| format!("{e:?}"))?;
        check!(started.entry == 0x1100, "static-PIE entry {:#x}", started.entry);
        let vector = auxv(table, started.rsp)?;
        check!(aux(&vector, AT_BASE) == Some(0), "AT_BASE of a static image");
        check!(raw_entry(table, 0).is_some(), "static-PIE not at zero");
        Ok(())
    })
}

/// Every way the interpreter can be unusable fails the load with the errno
/// Linux gives and leaves no frame behind.
pub fn interpreter_failures() -> Result<(), String> {
    setup_ramfs();
    install_lib("good.so", &image(ET_DYN, 0, 0x1200, None))?;
    install_lib("chained.so", &image(ET_DYN, 0, 0x1200, Some(b"/lib/good.so\0")))?;
    install_lib("garbage.so", &[0x42u8; 300])?;
    install_lib("fixed.so", &image(ET_EXEC, 0x40_0000, 0x40_1000, None))?;
    install_lib("tiny.so", &image(ET_DYN, 0, 0x1200, None)[..64])?;
    let cases: [(&str, &[u8], Option<u64>); 5] = [
        ("/lib/missing.so", b"missing", Some(2)),
        ("/lib/chained.so", b"an interpreter with an interpreter", Some(8)),
        ("/lib/garbage.so", b"garbage", Some(8)),
        ("/lib/tiny.so", b"a truncated header table", Some(8)),
        // At the program's own address: refused as overlapping.
        ("/lib/fixed.so", b"overlap", None),
    ];
    for (path, what, errno) in cases {
        let mut full = Vec::from(path.as_bytes());
        full.push(0);
        // The program is fixed-address at 0x40_0000, so `fixed.so` collides.
        let main = image(ET_EXEC, 0x40_0000, 0x40_1100, Some(&full));
        with_table(|table| {
            let result = start(table, &main);
            let Err(error) = result else {
                return Err(format!("{}: loaded", String::from_utf8_lossy(what)));
            };
            if let Some(errno) = errno {
                check!(
                    error.errno() == errno,
                    "{}: {error:?} is errno {}",
                    String::from_utf8_lossy(what),
                    error.errno()
                );
            }
            Ok(())
        })?;
    }
    Ok(())
}

/// `/lib/<name>` and its siblings are `/system/lib/<name>` when nothing is
/// there; a real file wins; only one path component counts; case matters.
pub fn lib_directories_are_served_from_system_lib() -> Result<(), String> {
    setup_ramfs();
    let root = Id::ROOT;
    install_lib("libx.so", b"from system lib")?;
    // The syscall layer is what programs use: `open` and read through it.
    let read = |path: &str| -> Option<Vec<u8>> {
        let name = format!("{path}\0");
        let fd = process::linux::dispatch_args_for_test(2, name.as_ptr() as u64, 0, 0, 0);
        if (fd as i64) < 0 {
            return None;
        }
        let mut buf = [0u8; 64];
        let n = process::linux::dispatch_args_for_test(0, fd, buf.as_mut_ptr() as u64, 64, 0);
        process::linux::dispatch_args_for_test(3, fd, 0, 0, 0);
        Some(buf[..n as usize].to_vec())
    };
    for dir in ["/lib", "/usr/lib", "/usr/local/lib", "/lib64"] {
        let path = format!("{dir}/libx.so");
        check!(
            read(&path).as_deref() == Some(b"from system lib".as_slice()),
            "{path} is not served from {}",
            fhs::SYSTEM_LIB
        );
    }
    check!(read("/lib/LIBX.so").is_none(), "a different case was found");
    check!(read("/lib/sub/libx.so").is_none(), "a subdirectory was aliased");
    // A file that really exists in /lib wins.
    let _ = crate::fs::abi_mkdir(root, "/lib", 0o755);
    crate::fs::abi_create(root, "/lib/libx.so", 0o644).map_err(|_| "create /lib/libx.so")?;
    crate::fs::abi_write(root, "/lib/libx.so", 0, b"the real one").map_err(|_| "write")?;
    check!(
        read("/lib/libx.so").as_deref() == Some(b"the real one".as_slice()),
        "the alias shadowed a real file"
    );
    Ok(())
}

/// Soak: dynamic programs load and free hundreds of times, failing ones too,
/// and every frame and quota byte comes back.
pub fn dynamic_load_soak() -> Result<(), String> {
    setup_ramfs();
    install_lib("ld-test.so", &image(ET_DYN, 0, 0x1200, None))?;
    let pie = image(ET_DYN, 0, 0x1100, Some(b"/lib/ld-test.so\0"));
    let fixed = image(ET_EXEC, 0x40_0000, 0x40_1100, Some(b"/lib/ld-test.so\0"));
    let broken = image(ET_DYN, 0, 0x1100, Some(b"/lib/none.so\0"));
    for round in 0..150 {
        for (name, elf, ok) in [("pie", &pie, true), ("fixed", &fixed, true), ("broken", &broken, false)] {
            with_table(|table| {
                let loaded = start(table, elf);
                check!(
                    loaded.is_ok() == ok,
                    "round {round}: {name} {:?}",
                    loaded.map(|s| s.entry)
                );
                Ok(())
            })?;
        }
    }
    Ok(())
}

pub(in crate::tests) const CASES: &[(&str, Test)] = &[
    ("loader_interpreter_parsing", interpreter_parsing),
    (
        "loader_pie_loads_beside_its_interpreter",
        pie_loads_beside_its_interpreter,
    ),
    (
        "loader_static_images_are_unchanged",
        static_images_are_unchanged,
    ),
    ("loader_interpreter_failures", interpreter_failures),
    (
        "loader_lib_directories_are_served_from_system_lib",
        lib_directories_are_served_from_system_lib,
    ),
    ("loader_dynamic_load_soak", dynamic_load_soak),
];
