//! Create versus update of the image file, against real files in a temp dir
//! and the independent fsck-style checker from `libs/ext2fs`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use ext2fs::Ext2;

use crate::layout_tests::fake_mbr;
use crate::os_disk::{FileIo, OS_START_LBA, SECTOR};
use crate::os_image::{
    compose, plan, validate, write_volume, Action, OsFile, Plan, Settings, Source,
};
use crate::os_layout::{dirs, parse_passwd, DirSpec, MANIFEST_PATH};

const SIZE: u64 = 8 << 20;
pub(crate) const STAMP: i64 = 1_700_000_000;

pub(crate) fn settings() -> Settings {
    Settings {
        os_size: SIZE,
        reset: false,
        update_damaged: false,
    }
}

/// A scratch directory unique to one test, removed on drop.
pub(crate) struct Scratch(PathBuf);

impl Scratch {
    pub(crate) fn new() -> Scratch {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "lazyos-image-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    pub(crate) fn image(&self) -> PathBuf {
        self.0.join("lazyos.img")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A bootloader-shaped image head: the MBR plus some FAT-ish bytes.
fn bios(fill: u8) -> Vec<u8> {
    let mut bytes = fake_mbr(20);
    bytes.resize(25 * 512, fill);
    bytes[..512].copy_from_slice(&fake_mbr(20));
    bytes
}

fn layout() -> Vec<DirSpec> {
    dirs(&parse_passwd("user:1000:1000:x:/home/user:sh\n"))
}

fn file(path: &str, bytes: &[u8], mode: u16) -> OsFile {
    OsFile {
        path: path.into(),
        source: Source::Bytes(bytes.to_vec()),
        mode,
    }
}

pub(crate) fn first_files() -> Vec<OsFile> {
    vec![
        file("/SUPER.ELF", b"super v1", 0o755),
        file("/PASSWD", b"admin:0:0\n", 0o644),
        file("/OLD.ELF", b"old", 0o755),
        file("/docs/README.md", b"# readme", 0o644),
        file("/docs/gone/page.md", b"page", 0o644),
    ]
}

pub(crate) fn build(dir: &Scratch, files: &[OsFile], settings: &Settings) -> Result<Plan, String> {
    let planned = plan(&dir.image(), settings)?;
    compose(&planned, &dir.image(), &bios(1), settings, &layout(), files)?;
    Ok(planned)
}

pub(crate) fn partition_bytes(image: &Path) -> Vec<u8> {
    std::fs::read(image).unwrap()[(OS_START_LBA * SECTOR) as usize..].to_vec()
}

pub(crate) fn assert_fsck_clean(image: &Path) {
    let problems = ext2fs::check::fsck(&partition_bytes(image));
    assert!(problems.is_empty(), "fsck: {problems:#?}");
}

/// Open the image's volume writable, for a test to act as a user would.
pub(crate) fn open_rw(image: &Path) -> Ext2 {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(image)
        .unwrap();
    let io = FileIo::new(file, OS_START_LBA, SIZE / SECTOR, true);
    Ext2::open(Box::new(io), crate::os_image::now).unwrap()
}

#[test]
fn a_missing_image_is_created_with_three_mbr_entries_and_a_clean_volume() {
    let dir = Scratch::new();
    let planned = build(&dir, &first_files(), &settings()).unwrap();
    assert_eq!(planned.action, Action::Create);
    assert!(planned.warning.is_none());

    let image = std::fs::read(dir.image()).unwrap();
    assert_eq!(image.len() as u64, OS_START_LBA * SECTOR + SIZE);
    assert_eq!(
        crate::os_disk::mbr_entry(&image, 3),
        Some((0x83, OS_START_LBA, SIZE / SECTOR))
    );
    assert!(!dir.0.join("lazyos.img.tmp").exists());
    assert_fsck_clean(&dir.image());

    let (uuid, manifest, sectors) = validate(&dir.image()).unwrap();
    assert_eq!((uuid, sectors), (planned.uuid, SIZE / SECTOR));
    assert!(manifest.entries.contains_key("/docs/gone/page.md"));

    let volume = open_rw(&dir.image());
    assert_eq!(volume.read_file("/SUPER.ELF").unwrap(), b"super v1");
    let meta = volume.lookup("/SUPER.ELF").unwrap();
    assert_eq!((meta.mode & 0o7777, meta.uid, meta.gid), (0o755, 0, 0));
    for (path, mode, owner) in [
        ("/home/user", 0o700, 1000),
        ("/conf", 0o700, 0),
        ("/logs", 0o750, 0),
        ("/apps", 0o755, 0),
        ("/docs/apps", 0o755, 0),
    ] {
        let meta = volume.lookup(path).unwrap();
        assert_eq!(
            (meta.mode & 0o7777, meta.uid, meta.gid),
            (mode, owner, owner),
            "{path}"
        );
    }
    assert!(
        volume.lookup("/data/tmp").is_err(),
        "the build seeds nothing under /data"
    );
    assert!(volume.read_file(MANIFEST_PATH).is_ok());
}

#[test]
fn an_update_keeps_the_uuid_and_user_files_and_applies_the_manifest_diff() {
    let dir = Scratch::new();
    let first = build(&dir, &first_files(), &settings()).unwrap();

    // The user installs something, writes into a home and /conf, and drops a
    // file into a directory the build will stop shipping.
    let volume = open_rw(&dir.image());
    volume.mkdir_p("/apps/demo", 0o755, 1000, 1000).unwrap();
    volume
        .write_file("/apps/demo/app.bin", b"installed", 0o755, 1000, 1000, STAMP)
        .unwrap();
    volume
        .write_file("/home/user/note.txt", b"mine", 0o644, 1000, 1000, STAMP)
        .unwrap();
    volume
        .write_file("/conf/settings", b"k=v", 0o600, 0, 0, STAMP)
        .unwrap();
    volume
        .write_file("/docs/gone/mine.txt", b"keep me", 0o644, 1000, 1000, STAMP)
        .unwrap();
    volume.flush().unwrap();
    drop(volume);

    // Second build: SUPER.ELF replaced, OLD.ELF and docs/gone/page.md dropped,
    // NEW.ELF added.
    let second_files = vec![
        file("/SUPER.ELF", b"super v2 is a bit longer", 0o755),
        file("/PASSWD", b"admin:0:0\n", 0o644),
        file("/NEW.ELF", b"new", 0o755),
        file("/docs/README.md", b"# readme", 0o644),
    ];
    let second = plan(&dir.image(), &settings()).unwrap();
    assert!(second.warning.is_none());
    assert!(matches!(second.action, Action::Update { .. }));
    assert_eq!(second.uuid, first.uuid, "an update keeps the UUID");
    compose(
        &second,
        &dir.image(),
        &bios(2),
        &settings(),
        &layout(),
        &second_files,
    )
    .unwrap();

    let volume = open_rw(&dir.image());
    assert_eq!(volume.uuid(), first.uuid);
    assert_eq!(
        volume.read_file("/SUPER.ELF").unwrap(),
        b"super v2 is a bit longer"
    );
    assert_eq!(volume.read_file("/NEW.ELF").unwrap(), b"new");
    assert!(volume.lookup("/OLD.ELF").is_err(), "manifest-removed file");
    assert!(volume.lookup("/docs/gone/page.md").is_err());
    // Files in neither manifest survive, and so does the directory holding one.
    for (path, bytes) in [
        ("/apps/demo/app.bin", &b"installed"[..]),
        ("/home/user/note.txt", b"mine"),
        ("/conf/settings", b"k=v"),
        ("/docs/gone/mine.txt", b"keep me"),
    ] {
        assert_eq!(volume.read_file(path).unwrap(), bytes, "{path}");
    }
    let meta = volume.lookup("/apps/demo/app.bin").unwrap();
    assert_eq!((meta.uid, meta.gid), (1000, 1000));
    drop(volume);

    let (_, manifest, _) = validate(&dir.image()).unwrap();
    assert!(manifest.entries.contains_key("/NEW.ELF"));
    assert!(!manifest.entries.contains_key("/OLD.ELF"));
    assert!(!manifest.entries.contains_key("/apps/demo/app.bin"));
    assert_fsck_clean(&dir.image());
    // The boot area was rewritten with the new BIOS image.
    let head = std::fs::read(dir.image()).unwrap();
    assert_eq!(head[600], 2);
}

#[test]
fn an_empty_directory_that_left_the_manifest_is_removed() {
    let dir = Scratch::new();
    build(&dir, &first_files(), &settings()).unwrap();
    // `/docs/gone` held only page.md, so dropping it empties and removes it.
    let files = vec![file("/SUPER.ELF", b"s", 0o755)];
    build(&dir, &files, &settings()).unwrap();
    let volume = open_rw(&dir.image());
    assert!(volume.lookup("/docs/gone").is_err());
    assert!(volume.lookup("/docs/apps").is_ok(), "layout dirs stay");
    assert!(volume.lookup("/data").is_ok(), "layout dirs stay");
}

#[test]
fn validation_failures_lead_to_a_create_with_a_reason() {
    let dir = Scratch::new();
    build(&dir, &first_files(), &settings()).unwrap();
    let pristine = std::fs::read(dir.image()).unwrap();
    let corrupt = |edit: &dyn Fn(&mut Vec<u8>)| {
        let mut bytes = pristine.clone();
        edit(&mut bytes);
        std::fs::write(dir.image(), bytes).unwrap();
        let planned = plan(&dir.image(), &settings()).unwrap();
        assert_eq!(planned.action, Action::Create);
        planned.warning.expect("a reason")
    };
    assert!(corrupt(&|b| b[0x1BE + 32 + 4] = 0x0B).contains("entry 3"));
    assert!(corrupt(&|b| b[0x1BE + 32 + 8] ^= 1).contains("entry 3"));
    assert!(corrupt(&|b| b[510] = 0).contains("signature"));
    let magic = (OS_START_LBA * SECTOR) as usize + 1024 + 0x38;
    assert!(corrupt(&|b| b[magic] = 0).contains("not ext2"));
    assert!(corrupt(&|b| b.truncate(1000)).contains("MBR entry 3"));
    assert!(corrupt(&|b| b.truncate(b.len() - 512)).contains("size"));
    assert!(corrupt(&|b| b.clear()).contains("MBR"));

    // No manifest (e.g. an ext2 image some other tool made): create.
    std::fs::write(dir.image(), &pristine).unwrap();
    let volume = open_rw(&dir.image());
    volume.unlink(MANIFEST_PATH).unwrap();
    volume.flush().unwrap();
    drop(volume);
    let planned = plan(&dir.image(), &settings()).unwrap();
    assert_eq!(planned.action, Action::Create);
    assert!(planned.warning.unwrap().contains("manifest"));

    // The old 10 MiB FAT-only image of earlier builds: create, with a reason.
    std::fs::write(dir.image(), bios(0)).unwrap();
    let planned = plan(&dir.image(), &settings()).unwrap();
    assert_eq!(planned.action, Action::Create);
    assert!(planned.warning.is_some());
}

#[test]
fn reset_recreates_a_valid_image_with_a_new_uuid() {
    let dir = Scratch::new();
    let first = build(&dir, &first_files(), &settings()).unwrap();
    let reset = Settings {
        reset: true,
        ..settings()
    };
    let second = build(&dir, &[file("/SUPER.ELF", b"s", 0o755)], &reset).unwrap();
    assert_eq!(second.action, Action::Create);
    assert!(second.warning.unwrap().contains("LAZYOS_RESET_OS"));
    assert_ne!(second.uuid, first.uuid);
    let volume = open_rw(&dir.image());
    assert!(volume.lookup("/PASSWD").is_err(), "the old tree is gone");
    assert_fsck_clean(&dir.image());
}

#[test]
fn a_size_change_of_an_existing_image_is_refused() {
    let dir = Scratch::new();
    build(&dir, &first_files(), &settings()).unwrap();
    let bigger = Settings {
        os_size: SIZE * 2,
        ..settings()
    };
    let error = plan(&dir.image(), &bigger).unwrap_err();
    assert!(error.contains("LAZYOS_RESET_OS=1"), "{error}");
    assert!(error.contains("resizing"), "{error}");
}

#[test]
fn a_boot_partition_past_64_mib_fails_the_build_and_writes_nothing() {
    let dir = Scratch::new();
    let planned = plan(&dir.image(), &settings()).unwrap();
    let mut huge = fake_mbr(OS_START_LBA as u32);
    huge.resize(4096, 0);
    let error = compose(&planned, &dir.image(), &huge, &settings(), &layout(), &[]).unwrap_err();
    assert!(error.contains("131072"), "{error}");
    assert!(!dir.image().exists());
    assert!(!dir.0.join("lazyos.img.tmp").exists());
}

#[test]
fn a_failed_create_leaves_no_temp_file_and_the_old_image_alone() {
    let dir = Scratch::new();
    std::fs::write(dir.image(), b"not an image").unwrap();
    let planned = plan(&dir.image(), &settings()).unwrap();
    assert_eq!(planned.action, Action::Create);
    let missing = OsFile {
        path: "/X.ELF".into(),
        source: Source::Path(dir.0.join("does-not-exist")),
        mode: 0o755,
    };
    let result = compose(
        &planned,
        &dir.image(),
        &bios(1),
        &settings(),
        &layout(),
        &[missing],
    );
    assert!(result.unwrap_err().contains("does-not-exist"));
    assert_eq!(std::fs::read(dir.image()).unwrap(), b"not an image");
    assert!(!dir.0.join("lazyos.img.tmp").exists());
}

#[test]
fn updating_an_image_another_process_holds_fails_clearly() {
    let dir = Scratch::new();
    build(&dir, &first_files(), &settings()).unwrap();
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true);
    // Like QEMU on Windows: open for writing, without sharing delete access.
    #[cfg(windows)]
    std::os::windows::fs::OpenOptionsExt::share_mode(&mut options, 3);
    let held = options.open(dir.image()).unwrap();
    held.lock().unwrap();
    let planned = plan(&dir.image(), &settings());
    // Validation reads the file; on Windows the exclusive lock may already
    // block that, which is the same clear failure one step earlier.
    if let Ok(planned) = planned {
        let result = compose(
            &planned,
            &dir.image(),
            &bios(1),
            &settings(),
            &layout(),
            &first_files(),
        );
        let error = result.unwrap_err();
        assert!(
            error.contains("QEMU") || error.contains("locked"),
            "{error}"
        );
    }
    drop(held);
}

#[test]
fn write_volume_never_touches_paths_outside_both_manifests() {
    use ext2fs::{memio::MemIo, Geometry};
    let io = MemIo::new(8 << 20);
    ext2fs::format(&io, Geometry::for_size(8 << 20), "t", [7; 16], STAMP).unwrap();
    let volume = Ext2::open(Box::new(io.clone()), crate::os_image::now).unwrap();
    let all = layout();
    let old_files = vec![file("/A.ELF", b"a", 0o755), file("/B.ELF", b"b", 0o755)];
    let old = write_volume(&volume, None, &all, &old_files, STAMP).unwrap();
    volume
        .write_file("/USER.DAT", b"u", 0o644, 0, 0, STAMP)
        .unwrap();
    volume.mkdir_p("/mine/deep", 0o700, 5, 5).unwrap();

    let new_files = vec![file("/A.ELF", b"a2", 0o755)];
    write_volume(&volume, Some(&old), &all, &new_files, STAMP).unwrap();
    assert_eq!(volume.read_file("/A.ELF").unwrap(), b"a2");
    assert!(volume.lookup("/B.ELF").is_err());
    assert_eq!(volume.read_file("/USER.DAT").unwrap(), b"u");
    assert!(volume.lookup("/mine/deep").is_ok());
    let problems = ext2fs::check::fsck(&io.snapshot());
    assert!(problems.is_empty(), "{problems:#?}");
}

/// Check a real `target/lazyos.img` (CI and the F2 verification run this with
/// `LAZYOS_CHECK_IMAGE=target/lazyos.img cargo test -p build-support-tests
/// -- --ignored --nocapture`): the OS volume validates and the independent
/// checker finds nothing wrong, on a fresh and on an updated image alike.
#[test]
#[ignore = "needs LAZYOS_CHECK_IMAGE"]
fn a_built_image_is_consistent() {
    let image = PathBuf::from(std::env::var("LAZYOS_CHECK_IMAGE").expect("LAZYOS_CHECK_IMAGE"));
    let (uuid, manifest, sectors) = validate(&image).unwrap();
    println!(
        "{}: uuid {}, {} MiB, {} manifest entries",
        image.display(),
        crate::os_image::format_uuid(uuid),
        (sectors * SECTOR) >> 20,
        manifest.entries.len()
    );
    assert_fsck_clean(&image);
    // F3: every build-placed file is below a directory, so nothing the
    // manifest lists sits at the root.
    let at_root: Vec<&String> = manifest
        .entries
        .iter()
        .filter(|(path, kind)| {
            **kind == crate::os_manifest::Kind::File && path.rfind('/') == Some(0)
        })
        .map(|(path, _)| path)
        .collect();
    assert!(
        at_root.is_empty(),
        "build-placed files at the root: {at_root:?}"
    );
}
