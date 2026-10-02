//! The F3 layout (docs/filesystem-plan.md): programs at their real names in
//! `/system/bin`, data in `/system/etc` and `/system/share`, documentation in
//! `/docs/os`, and an in-place update of an F2-built image that leaves no
//! build-placed file at the root.

use ext2fs::{Ext2, FileKind};

use crate::docs_embed::image_path;
use crate::os_image::{write_volume, OsFile, OsFiles, Sink, Source};
use crate::os_layout::{dirs, file_mode, parse_passwd, DirSpec};

const STAMP: i64 = 1_700_000_000;
/// The account file the image ships (the single copy, issue #508).
const PASSWD: &str = include_str!("../passwd");

fn bytes(path: &str, mode: u16) -> OsFile {
    OsFile {
        path: path.into(),
        source: Source::Bytes(path.as_bytes().to_vec()),
        mode,
    }
}

/// What an F2 build placed: flat uppercase names at the root, the docs at
/// `/docs`, the lazyrad samples at `/LAZYRAD`, and no `/system/*` directories.
fn f2_build() -> (Vec<DirSpec>, Vec<OsFile>) {
    let layout = crate::layout_tests::pre_f4_layout()
        .into_iter()
        .filter(|dir| !dir.path.starts_with("/system/"))
        .collect();
    let files = vec![
        bytes("/SUPER.ELF", 0o755),
        bytes("/KEYD.ELF", 0o755),
        bytes("/RHAI.ELF", 0o755),
        bytes("/BUSYBOX", 0o755),
        bytes("/PASSWD", 0o644),
        bytes("/MIME.TYP", 0o644),
        bytes("/HELLO.TXT", 0o644),
        bytes("/TESTDOC.MD", 0o644),
        bytes("/PKGDEMO.LZP", 0o644),
        bytes("/docs/README.md", 0o644),
        bytes("/docs/architecture/boot.md", 0o644),
        bytes("/LAZYRAD/hello/main.lr", 0o644),
    ];
    (layout, files)
}

/// The same content as an F3 build names it, through the sink so the modes
/// come from the path rule.
fn f3_build() -> (Vec<DirSpec>, Vec<OsFile>) {
    let mut sink = OsFiles::default();
    for program in [
        fhs::bin::INIT,
        fhs::bin::KEYD,
        fhs::bin::RHAI,
        fhs::bin::BUSYBOX,
    ] {
        sink.add_bytes(program, program.as_bytes().to_vec());
    }
    for data in [
        fhs::etc::PASSWD,
        fhs::share::MIME_TYPES,
        fhs::share::TESTDOC,
        fhs::share::PKGDEMO,
    ] {
        sink.add_bytes(data, data.as_bytes().to_vec());
    }
    sink.add_bytes(
        &format!("{}/hello.txt", fhs::share::SAMPLES),
        b"hi".to_vec(),
    );
    for doc in ["docs/README.md", "docs/architecture/boot.md"] {
        sink.add_bytes(&image_path(doc), b"doc".to_vec());
    }
    let sample = format!("{}/hello/main.lr", fhs::share::LAZYRAD_SAMPLES);
    sink.add_bytes(&sample, b"lr".to_vec());
    (dirs(&parse_passwd(PASSWD)), sink.files())
}

fn volume() -> (ext2fs::memio::MemIo, Ext2) {
    use ext2fs::{memio::MemIo, Geometry};
    let io = MemIo::new(8 << 20);
    ext2fs::format(&io, Geometry::for_size(8 << 20), "t", [3; 16], STAMP).unwrap();
    let volume = Ext2::open(Box::new(io.clone()), crate::os_image::now).unwrap();
    (io, volume)
}

fn root_files(volume: &Ext2) -> Vec<String> {
    volume
        .readdir("/")
        .unwrap()
        .into_iter()
        .filter(|entry| entry.kind == FileKind::File)
        .map(|entry| entry.name)
        .collect()
}

#[test]
fn the_layout_creates_the_system_tree() {
    let all = dirs(&parse_passwd(PASSWD));
    for path in [
        fhs::SYSTEM,
        fhs::SYSTEM_BIN,
        fhs::SYSTEM_ETC,
        fhs::SYSTEM_SHARE,
        fhs::SYSTEM_PACKAGES,
    ] {
        let dir = all.iter().find(|dir| dir.path == path).expect(path);
        assert_eq!((dir.mode, dir.uid, dir.gid), (0o755, 0, 0), "{path}");
    }
}

#[test]
fn the_mode_follows_the_directory_not_the_name() {
    for program in fhs::bin::ALL {
        assert_eq!(file_mode(program), 0o755, "{program}");
    }
    for data in [
        fhs::etc::PASSWD,
        fhs::share::MIME_TYPES,
        fhs::docs::README,
        "/SUPER.ELF",
        "/BUSYBOX",
        "/system/binary",
        "/system/bin",
        "/system/bin/",
    ] {
        assert_eq!(file_mode(data), 0o644, "{data}");
    }
}

#[test]
fn docs_go_under_docs_os() {
    assert_eq!(image_path("docs/README.md"), fhs::docs::README);
    assert_eq!(image_path("docs/a/b.md"), "/docs/os/a/b.md");
}

#[test]
fn an_f2_image_updated_by_the_f3_build_has_a_clean_root() {
    let (io, volume) = volume();
    let (old_layout, old_files) = f2_build();
    let old = write_volume(&volume, None, &old_layout, &old_files, STAMP).unwrap();
    // The user's own files: in a home, and one dropped at the root by hand.
    volume
        .write_file(
            "/data/home/user/note.txt",
            b"mine",
            0o644,
            1000,
            1000,
            STAMP,
        )
        .unwrap();
    volume
        .write_file("/USER.TXT", b"by hand", 0o644, 0, 0, STAMP)
        .unwrap();
    assert!(root_files(&volume).len() > 5);

    let (layout, files) = f3_build();
    let new = write_volume(&volume, Some(&old), &layout, &files, STAMP).unwrap();

    // Only the file no manifest ever listed is left at the root.
    assert_eq!(root_files(&volume), ["USER.TXT"]);
    assert_eq!(volume.read_file("/USER.TXT").unwrap(), b"by hand");
    assert_eq!(
        volume.read_file("/data/home/user/note.txt").unwrap(),
        b"mine"
    );
    for entry in volume.readdir(fhs::SYSTEM_BIN).unwrap() {
        let path = format!("{}/{}", fhs::SYSTEM_BIN, entry.name);
        let meta = volume.lookup(&path).unwrap();
        assert_eq!(
            (meta.mode & 0o7777, meta.uid, meta.gid),
            (0o755, 0, 0),
            "{path}"
        );
    }
    let passwd = volume.lookup(fhs::etc::PASSWD).unwrap();
    assert_eq!(passwd.mode & 0o7777, 0o644);
    assert!(volume.read_file(fhs::docs::README).is_ok());
    // The old trees left with their files: `/docs` holds `os` and `apps`.
    assert!(volume.lookup("/docs/architecture").is_err());
    assert!(volume.lookup("/LAZYRAD").is_err());
    let docs: Vec<String> = volume
        .readdir(fhs::docs::DOCS_ROOT)
        .unwrap()
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    assert_eq!(docs, ["apps", "os"]);
    assert!(new
        .entries
        .keys()
        .all(|path| path.matches('/').count() > 1 || old_layout_dir(path)));
    let problems = ext2fs::check::fsck(&io.snapshot());
    assert!(problems.is_empty(), "{problems:#?}");
}

/// The top-level directories of the layout (the only single-component paths
/// an F3 manifest may hold).
fn old_layout_dir(path: &str) -> bool {
    dirs(&parse_passwd(PASSWD))
        .iter()
        .any(|dir| dir.path == path)
        || path == fhs::docs::DOCS_ROOT
}
