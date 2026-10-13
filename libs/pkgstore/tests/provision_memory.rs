//! `pkgd`'s heap while it provisions a whole core set on a fresh image (issue
//! #509): the user heap never reuses a block over 1 MiB, and `pkgd` restarts
//! once it grew by 32 MiB beyond its package buffer (`pkgstore::recycle`, the rule
//! `pkgd` applies), so one first-boot pass must stay under that.
//!
//! The set is the real one when `tools/xui/build.py` has built it
//! (`target/pkg/core/*.lzp`; CI's xui workflow runs this test after building
//! it, with `PKGSTORE_REQUIRE_CORE_SET=1` so a missing set fails instead of
//! falling back), else twelve synthetic packages with programs of 2.8 to
//! 5.8 MB that do not compress.
//!
//! What keeps it bounded: each package is read into the one kept buffer, and
//! every file is unpacked to disk through `lazypkg`'s 1 MiB window, which the
//! heap recycles. So the pass grows the heap by the largest *package* alone,
//! whatever its files expand to. The real set's programs expand about 2.3x
//! (LazyWeb's 6.4 MB package holds a 14.7 MB program): when a file was
//! unpacked whole into a kept scratch buffer, the pass also grew by the
//! largest *file*, 21 MB in all, two thirds of the recycle threshold.

mod provisioning;

use std::path::PathBuf;

use ext2fs::memio::MemIo;
use ext2fs::{Ext2, Geometry};
use provisioning::lzp::package;
use provisioning::{measure, Heap, Image, Pkgd};

#[global_allocator]
static HEAP: Heap = Heap;

const RECYCLE_BYTES: usize = pkgstore::recycle::RECYCLE_BYTES as usize;

fn clock() -> i64 {
    1_700_000_000
}

/// The built core set, `(system_name, version, bytes)`, if there is one.
/// `PKGSTORE_REQUIRE_CORE_SET=1` makes its absence a failure (CI, after
/// building it).
fn built_core_set() -> Option<Vec<(String, String, Vec<u8>)>> {
    let set = read_core_set();
    if set.is_none() && std::env::var_os("PKGSTORE_REQUIRE_CORE_SET").is_some() {
        panic!("PKGSTORE_REQUIRE_CORE_SET is set but target/pkg/core has no readable .lzp");
    }
    set
}

fn read_core_set() -> Option<Vec<(String, String, Vec<u8>)>> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/pkg/core");
    let mut set = Vec::new();
    for entry in std::fs::read_dir(dir).ok()? {
        let path = entry.ok()?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("lzp") {
            continue;
        }
        let bytes = std::fs::read(&path).ok()?;
        let manifest = lazypkg::Package::open(&bytes).ok()?.manifest().app.clone();
        set.push((manifest.system_name, manifest.version, bytes));
    }
    (!set.is_empty()).then_some(set)
}

fn synthetic_core_set() -> Vec<(String, String, Vec<u8>)> {
    (0..12)
        .map(|i| {
            let name = format!("os.lazy.app{i}");
            let program = 2_800_000 + i * 270_000;
            let bytes = package(&name, "0.1.0", program, i as u64);
            (name, String::from("0.1.0"), bytes)
        })
        .collect()
}

#[test]
fn provisioning_the_core_set_stays_under_the_recycle_threshold() {
    let set = built_core_set().unwrap_or_else(synthetic_core_set);
    let total: usize = set.iter().map(|(_, _, b)| b.len()).sum();
    let image = Image::new(
        set.iter()
            .map(|(n, v, b)| (n.as_str(), v.as_str(), b.clone()))
            .collect(),
    );
    let fs = fresh_fs(256 << 20);
    let mut pkgd = Pkgd::new();
    let (tally, growth) = measure(|| pkgd.provision(&fs, &image));
    let tally = tally.unwrap();
    assert_eq!(tally.installed as usize, set.len());
    assert_eq!(tally.failed, 0);
    println!(
        "{} packages, {total} bytes, largest {}: heap grew by {growth} bytes",
        set.len(),
        image.largest()
    );
    // The rule pkgd applies: the threshold counts what the heap grew beyond the kept package
    // buffer (the largest package), so the browser's 44 MB package does not make every fresh
    // pkgd restart after one step.
    let buffer = image.largest() as u64;
    assert!(
        !pkgstore::recycle::due(growth as u64, buffer),
        "provisioning grew the heap by {growth} bytes (allowed {} with a {buffer} byte buffer)",
        pkgstore::recycle::allowed(buffer)
    );
    // The bound is the largest package (the kept buffer) plus a little, not
    // the set and not any file: the slack is for the small blocks of the
    // pass that the heap's classes would recycle anyway.
    let bound = image.largest() + (1 << 20);
    assert!(
        growth < bound,
        "provisioning grew the heap by {growth} bytes (bound {bound})"
    );
    for (_, _, bytes) in &set {
        installed_files_match(&fs, &pkgd, bytes);
    }
}

/// A freshly formatted OS volume of `size` bytes with the directories `pkgd` installs into.
fn fresh_fs(size: usize) -> Ext2 {
    let io = MemIo::new(size);
    let geometry = Geometry {
        block_size: 4096,
        blocks_count: (size / 4096) as u32,
        bytes_per_inode: 16 * 1024,
    };
    ext2fs::format(&io, geometry, "lazyos-root", [7; 16], clock()).unwrap();
    let fs = Ext2::open(Box::new(io), clock).unwrap();
    for dir in ["/apps", "/docs", "/docs/apps", "/logs"] {
        fs.mkdir_p(dir, 0o755, 0, 0).unwrap();
    }
    fs
}

/// A package over the recycle threshold (the WebKit browser's is 44 MB; this one is 36 MiB of
/// program that does not compress) next to small ones: the pass grows the heap by about that
/// package, which is more than the threshold, and `pkgd` must not restart for it. Whether the
/// real set has such a package depends on the build, so the case is made here.
#[test]
fn a_package_over_the_recycle_threshold_does_not_make_pkgd_restart() {
    let mut set = vec![(
        "os.lazy.big".to_owned(),
        "0.1.0".to_owned(),
        package("os.lazy.big", "0.1.0", 36 << 20, 99),
    )];
    set.push((
        "os.lazy.small".to_owned(),
        "0.1.0".to_owned(),
        package("os.lazy.small", "0.1.0", 300_000, 5),
    ));
    let image = Image::new(
        set.iter()
            .map(|(n, v, b)| (n.as_str(), v.as_str(), b.clone()))
            .collect(),
    );
    let fs = fresh_fs(160 << 20);
    let mut pkgd = Pkgd::new();
    let (tally, growth) = measure(|| pkgd.provision(&fs, &image));
    let tally = tally.unwrap();
    assert_eq!((tally.installed, tally.failed), (2, 0));
    let buffer = image.largest() as u64;
    assert!(buffer > pkgstore::recycle::RECYCLE_BYTES);
    println!("a {buffer} byte package: heap grew by {growth} bytes");
    assert!(
        growth > RECYCLE_BYTES,
        "the case is only meaningful when the old rule (growth < {RECYCLE_BYTES}) would restart: {growth}"
    );
    assert!(
        !pkgstore::recycle::due(growth as u64, buffer),
        "pkgd would restart for a package of {buffer} bytes (grew {growth})"
    );
    assert!(growth < image.largest() + (1 << 20), "{growth}");
}

/// Every file of the package `bytes` is on disk, byte for byte, under its
/// install directory: the streamed extraction writes what `read` returns.
fn installed_files_match(fs: &Ext2, pkgd: &Pkgd, bytes: &[u8]) {
    let package = lazypkg::Package::open(bytes).unwrap();
    let row = &pkgd.rows[&package.manifest().app.system_name];
    let root = pkgstore::layout::install_path(&row.install_dir).unwrap();
    for entry in package.entries().filter(|entry| !entry.is_dir) {
        let path = pkgstore::layout::entry_path(&root, entry.name).unwrap();
        let mut disk = vec![0; entry.size as usize];
        assert_eq!(fs.read(&path, 0, &mut disk).unwrap(), disk.len(), "{path}");
        assert!(disk == package.read(entry.name).unwrap(), "{path} differs");
    }
}
