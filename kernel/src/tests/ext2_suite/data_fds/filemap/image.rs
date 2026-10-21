//! A program's read-only segments are shared through the page cache of its
//! file (issue #942, `process::loader`): instances of one program hold one
//! copy of the text, a running program's file cannot be written (`ETXTBSY`)
//! but can be replaced, and loading, running and reaping many instances
//! leaks no frame, no cache page and no registered file.

use super::*;
use crate::fs::vfs::{FsError, Id};
use crate::process::image::VfsFile;
use crate::tests::loader_suite::{build_elf_with, payload_byte, Ph, PF_R, PF_W, PF_X, RESERVED};

pub(super) const TEXT: u64 = 0x40_0000;
pub(super) const TEXT_PAGES: u64 = 16;
pub(super) const DATA: u64 = 0x50_0000;
/// Where the loader suite's images keep the first payload byte.
pub(super) const PAYLOAD: u64 = 0x1000;
const ETXTBSY: u64 = 26;
pub(super) const PATH: &str = "/data/prog";

/// A program: `TEXT_PAGES` of read-execute text, then 0x100 file bytes of
/// writable data (0x2000 in memory). Every payload byte is XORed with
/// `version`, so two versions never share a byte of text.
pub(super) fn program(version: u8) -> Vec<u8> {
    let text = TEXT_PAGES * PAGE;
    let phdrs = [
        Ph {
            vaddr: TEXT,
            memsz: text,
            filesz: text,
            offset: PAYLOAD,
            flags: PF_R | PF_X,
        },
        Ph {
            vaddr: DATA,
            memsz: 0x2000,
            filesz: 0x100,
            offset: PAYLOAD + text,
            flags: PF_R | PF_W,
        },
    ];
    let mut elf = build_elf_with(TEXT, &phdrs, (text + 0x100) as usize);
    elf[PAYLOAD as usize..]
        .iter_mut()
        .for_each(|byte| *byte ^= version);
    elf
}

/// Byte `i` of the text of `program(version)`.
fn text_byte(i: u64, version: u8) -> u8 {
    payload_byte(i as usize) ^ version
}

pub(super) fn text_bytes(from: u64, len: u64, version: u8) -> Vec<u8> {
    (from..from + len).map(|i| text_byte(i, version)).collect()
}

/// Frames in use right now.
fn frames_used() -> usize {
    let stats = mem::frame_stats();
    stats.total - stats.free
}

/// Load the program at `path` into a new address space, the way `execve` does.
pub(super) fn start(path: &str) -> Result<Space, String> {
    let space = Space::new()?;
    let file = VfsFile::abi(Id::ROOT, path).map_err(fs_error)?;
    process::load_segments(space.table, &file, RESERVED, 0).map_err(String::from)?;
    Ok(space)
}

fn kind_at(space: &Space, va: u64) -> Result<Kind, String> {
    Ok(mem::vma::find(space.table, va)
        .ok_or_else(|| format!("no VMA at {va:#x}"))?
        .kind)
}

/// Run the program once and reap it, so the heap and the block cache have
/// grown to what a run needs before the frames are counted.
fn warm_up() -> Result<(), String> {
    let space = start(PATH)?;
    touch_text(&space)?;
    space.load(DATA, 1)?;
    Ok(())
}

/// Touch every text page of `space`.
fn touch_text(space: &Space) -> Result<(), String> {
    (0..TEXT_PAGES).try_for_each(|page| space.touch(TEXT + page * PAGE, false))
}

/// Two instances of a program map one set of text frames, read from the file
/// once; the data segment is theirs alone; text cannot be stored to; the
/// per-instance cost of a copying load is what sharing saves.
pub fn instances_share_text() -> Result<(), String> {
    let data = Data::new(0)?;
    put(PATH, &program(0))?;
    warm_up()?;
    let baseline = frames_used();
    let held = quota::usage(0, Resource::UserMemory);

    let first = start(PATH)?;
    check!(
        kind_at(&first, TEXT)? == Kind::FileImage,
        "the text is {:?}",
        kind_at(&first, TEXT)?
    );
    check!(kind_at(&first, DATA)? == Kind::File, "the data is shared");
    check!(
        first.entry(TEXT).is_none(),
        "text was mapped before it was touched"
    );
    check!(
        quota::usage(0, Resource::UserMemory) == held + 0x2000,
        "only the data segment is charged to the loader's user"
    );
    check!(
        first.load(TEXT + 100, 64)? == text_bytes(100, 64, 0),
        "text differs from the file"
    );
    let data_start = (TEXT_PAGES * PAGE) as usize;
    let data_want: Vec<u8> = (0..16).map(|i| payload_byte(data_start + i)).collect();
    check!(
        first.load(DATA, 16)? == data_want,
        "data differs from the file"
    );
    touch_text(&first)?;
    let one_instance = frames_used() - baseline;

    let before = frames_used();
    let second = start(PATH)?;
    touch_text(&second)?;
    let extra = frames_used() - before;
    for page in 0..TEXT_PAGES {
        let va = TEXT + page * PAGE;
        let frame = frame_of(first.table, va)?;
        check!(
            frame == frame_of(second.table, va)?,
            "text page {page} is not shared"
        );
        let holders = mem::frame_refcount(PhysAddr::new(frame));
        check!(
            holders == 3,
            "text page {page}: the cache and two mappings hold it, not {holders}"
        );
    }
    check!(
        extra + (TEXT_PAGES as usize) < one_instance + 2,
        "a second instance took {extra} frames; the first took {one_instance} with {TEXT_PAGES} of text"
    );
    check!(
        frame_of(first.table, DATA)? != frame_of(second.table, DATA)?,
        "the data segment is shared"
    );
    first.store(DATA, b"mine")?;
    check!(
        second.load(DATA, 4)? != b"mine",
        "a data store crossed processes"
    );

    // Text is read-only: a store is a fault nobody resolves, and the file
    // keeps its bytes.
    check!(
        !first.can_touch(TEXT + PAGE, true),
        "a store to text succeeded"
    );
    check!(bytes_of(PATH)? == program(0), "the program file changed");

    // A copying load of the same program, for the price of sharing.
    let before = frames_used();
    let copied = Space::new()?;
    process::load_segments(copied.table, &program(0)[..], RESERVED, 0).map_err(String::from)?;
    let copy_cost = frames_used() - before;
    check!(
        copy_cost + 4 >= TEXT_PAGES as usize + extra,
        "a copy cost {copy_cost} frames, sharing {extra} (+{TEXT_PAGES} text pages)"
    );
    drop((first, second, copied));
    check!(
        frames_used() == baseline,
        "frames leaked: {baseline} in use before, {} after",
        frames_used()
    );
    check!(
        quota::usage(0, Resource::UserMemory) == held,
        "the quota was not given back"
    );
    data.check_clean()
}

/// A file a running program was started from cannot be written, truncated or
/// opened for writing; it can be replaced or unlinked, and the instance keeps
/// reading the bytes it started with, including pages it had not yet touched.
pub fn running_program_is_protected() -> Result<(), String> {
    let data = Data::new(0)?;
    put(PATH, &program(1))?;
    let old = start(PATH)?;
    old.touch(TEXT, false)?;

    check!(
        open(PATH, O_WRONLY) == errno(ETXTBSY),
        "O_WRONLY on a running program"
    );
    check!(
        open(PATH, O_RDWR) == errno(ETXTBSY),
        "O_RDWR on a running program"
    );
    check!(
        open(PATH, O_WRONLY | O_TRUNC) == errno(ETXTBSY),
        "O_TRUNC on a running program"
    );
    check!(
        path_call(SYS_TRUNCATE, PATH, 10) == errno(ETXTBSY),
        "truncate(2)"
    );
    check!(
        matches!(
            crate::fs::abi_write(Id::ROOT, PATH, 0, b"patch"),
            Err(FsError::TextBusy)
        ),
        "a write by name"
    );
    check!(
        bytes_of(PATH)? == program(1),
        "a refused write changed the file"
    );
    let reader = open_ro(PATH)?;
    check!(close(reader) == 0, "a program can still be read");

    // Replaced by rename: the running instance keeps the old inode.
    put("/data/prog.new", &program(2))?;
    let (from, to) = (cstr("/data/prog.new"), cstr(PATH));
    check!(
        syscall(SYS_RENAME, from.as_ptr() as u64, to.as_ptr() as u64, 0, 0) == 0,
        "renaming over a running program"
    );
    check!(
        old.load(TEXT + 5 * PAGE, 32)? == text_bytes(5 * PAGE, 32, 1),
        "an untouched page of the replaced program changed"
    );
    let new = start(PATH)?;
    check!(
        new.load(TEXT + 5 * PAGE, 32)? == text_bytes(5 * PAGE, 32, 2),
        "the new instance did not get the new program"
    );
    check!(
        frame_of(old.table, TEXT + 5 * PAGE)? != frame_of(new.table, TEXT + 5 * PAGE)?,
        "two versions share a frame"
    );

    // Unlinked while running: the name goes, the data stays.
    check!(
        path_call(SYS_UNLINK, PATH, 0) == 0,
        "unlinking a running program"
    );
    check!(
        new.load(TEXT + 9 * PAGE, 32)? == text_bytes(9 * PAGE, 32, 2),
        "an untouched page of an unlinked program changed"
    );
    drop((old, new));
    let names = data_names()?;
    check!(
        names
            .iter()
            .all(|name| !crate::fs::hidden::is_reserved(name)),
        "hidden entries outlived the last instance: {names:?}"
    );
    // Nothing runs it any more: a new file of the name is writable.
    put(PATH, &program(3))?;
    let again = start(PATH)?;
    drop(again);
    let fd = open(PATH, O_WRONLY);
    check!(
        fd < 64,
        "a program nobody runs could not be opened for writing: {fd:#x}"
    );
    check!(close(fd) == 0, "close");
    data.check_clean()
}

/// A file open for writing is not shared (nothing could stop the writer):
/// the loader copies it. Once the writer closes, the next load shares.
pub fn file_open_for_writing_is_copied() -> Result<(), String> {
    let data = Data::new(0)?;
    put(PATH, &program(0))?;
    let writer = open(PATH, O_WRONLY);
    check!(writer < 64, "open for writing: {writer:#x}");
    let copy = start(PATH)?;
    check!(
        kind_at(&copy, TEXT)? == Kind::File,
        "a file open for writing was shared"
    );
    check!(
        copy.load(TEXT + 3 * PAGE, 16)? == text_bytes(3 * PAGE, 16, 0),
        "the copy differs from the file"
    );
    check!(close(writer) == 0, "close");
    let shared = start(PATH)?;
    check!(
        kind_at(&shared, TEXT)? == Kind::FileImage,
        "not shared once the writer closed"
    );
    drop((copy, shared));
    data.check_clean()
}

/// One read-only segment of `size` bytes at `offset` in the file.
fn segment(size: u64, offset: u64, flags: u32) -> Ph {
    Ph {
        vaddr: TEXT,
        memsz: size,
        filesz: size,
        offset,
        flags,
    }
}

/// Segments the page cache cannot back are copied exactly as before: a
/// writable one, one with a `.bss` tail, one at another offset within the
/// page than in the file, and one whose last page another segment shares.
pub fn unshareable_segments_are_copied() -> Result<(), String> {
    let data = Data::new(0)?;
    let cases: [(&str, Vec<Ph>); 4] = [
        (
            "bss tail",
            vec![Ph {
                memsz: 0x2000,
                ..segment(0x1000, PAYLOAD, PF_R)
            }],
        ),
        ("misaligned", vec![segment(0x1000, PAYLOAD + 0x10, PF_R)]),
        ("writable", vec![segment(0x1000, PAYLOAD, PF_R | PF_W)]),
        (
            "shared boundary page",
            vec![
                segment(0x1800, PAYLOAD, PF_R),
                Ph {
                    vaddr: TEXT + 0x1800,
                    ..segment(0x100, PAYLOAD + 0x1800, PF_R | PF_W)
                },
            ],
        ),
    ];
    for (name, phdrs) in cases {
        put(PATH, &build_elf_with(TEXT, &phdrs, 0x3000))?;
        let space = start(PATH)?;
        check!(kind_at(&space, TEXT)? == Kind::File, "{name}: not copied");
        let first = (phdrs[0].offset - PAYLOAD) as usize;
        let want: Vec<u8> = (0..32).map(|i| payload_byte(first + i)).collect();
        check!(space.load(TEXT, 32)? == want, "{name}: bytes differ");
        drop(space);
        check!(path_call(SYS_UNLINK, PATH, 0) == 0, "{name}: unlink");
    }
    data.check_clean()
}

/// Many instances at once cost only their private pages, and starting and
/// reaping hundreds of them returns every frame, cache page and registered
/// file.
pub fn image_soak() -> Result<(), String> {
    const LIVE: usize = 24;
    const ROUNDS: usize = 300;
    let data = Data::new(0)?;
    put(PATH, &program(0))?;
    warm_up()?;
    let baseline = frames_used();

    let first = start(PATH)?;
    touch_text(&first)?;
    let private = frames_used() - baseline - TEXT_PAGES as usize;
    let mut alive = Vec::new();
    for _ in 0..LIVE {
        let space = start(PATH)?;
        touch_text(&space)?;
        alive.push(space);
    }
    let grown = frames_used() - baseline;
    check!(
        grown <= TEXT_PAGES as usize + private * (LIVE + 1) + LIVE,
        "{LIVE} more instances took {grown} frames; one costs {private} private + {TEXT_PAGES} shared"
    );
    // One instance exiting leaves the others their text: the cache page stays
    // while anyone maps it.
    drop(alive.swap_remove(LIVE / 2));
    for space in &alive {
        check!(
            space.load(TEXT + 7 * PAGE, 8)? == text_bytes(7 * PAGE, 8, 0),
            "an instance lost its text when another exited"
        );
    }
    drop(alive);
    drop(first);
    check!(
        frames_used() == baseline,
        "frames leaked after the live set"
    );

    for round in 0..ROUNDS {
        let space = start(PATH)?;
        let page = (round as u64 * 5) % TEXT_PAGES;
        check!(
            space.load(TEXT + page * PAGE + 3, 8)? == text_bytes(page * PAGE + 3, 8, 0),
            "round {round}: page {page} differs"
        );
        drop(space);
        check!(
            frames_used() == baseline,
            "round {round}: {} frames leaked",
            frames_used() - baseline
        );
    }
    data.check_clean()
}

/// Shared text is charged to no one; write access (`mprotect`) turns it into
/// a private mapping that is, and the quota survives `mremap` and `munmap`
/// of it.
pub fn text_quota_follows_write_access() -> Result<(), String> {
    let data = Data::new(0)?;
    put(PATH, &program(0))?;
    let space = start(PATH)?;
    space.enter();
    let held = quota::usage(0, Resource::UserMemory);

    // Shrinking and moving shared text frees nothing it was not charged.
    let dest = BASE;
    check!(
        sys6(
            SYS_MREMAP,
            [
                TEXT + 8 * PAGE,
                8 * PAGE,
                PAGE,
                MREMAP_MAYMOVE | MREMAP_FIXED,
                dest,
                0
            ]
        ) == dest,
        "mremap of shared text"
    );
    check!(
        quota::usage(0, Resource::UserMemory) == held,
        "mremap of shared text moved the quota"
    );
    check!(munmap(dest, PAGE) == 0, "munmap");

    // Writable text is private memory and is charged for it.
    check!(
        mprotect(TEXT, 4 * PAGE, RW) == 0,
        "mprotect(PROT_WRITE) of text"
    );
    check!(
        quota::usage(0, Resource::UserMemory) == held + 4 * PAGE,
        "writable text was not charged"
    );
    check!(
        kind_at(&space, TEXT)? == Kind::FilePrivate,
        "writable text is {:?}",
        kind_at(&space, TEXT)?
    );
    space.store(TEXT + 3, b"patched")?;
    check!(
        space.load(TEXT + 3, 7)? == b"patched",
        "the private copy lost the store"
    );
    check!(
        bytes_of(PATH)? == program(0),
        "a store reached the program file"
    );
    check!(munmap(TEXT, 4 * PAGE) == 0, "munmap of writable text");
    check!(
        quota::usage(0, Resource::UserMemory) == held,
        "unmapping writable text kept its charge"
    );
    drop(space);
    data.check_clean()
}
