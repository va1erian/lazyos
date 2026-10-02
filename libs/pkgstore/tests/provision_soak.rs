//! Soak (issue #509): 200 provisioning passes on one ext2 volume, alternating
//! the shipped set between two image versions, as reboots across image
//! updates would. Between the two images one app is upgraded, one is rebuilt
//! at the same version, one is dropped, one is added, and image B ships a
//! damaged archive. After every pass `/apps` holds exactly one version per
//! installed app, `/docs/apps` matches it, `pkg.log` verifies and the heap
//! growth stays far below `pkgd`'s 32 MiB recycle threshold; every tenth
//! pass, and at the end, the independent ext2 checker finds nothing.

mod provisioning;

use ext2fs::check::fsck;
use ext2fs::memio::MemIo;
use ext2fs::{Ext2, Geometry};
use pkgstore::audit::verify;
use pkgstore::layout;
use pkgstore::provision::Tally;
use pkgstore::tree::TreeFs;
use provisioning::lzp::package;
use provisioning::{measure, Ext2Tree, Heap, Image, Pkgd};

#[global_allocator]
static HEAP: Heap = Heap;

const PASSES: usize = 200;
/// `pkgd` restarts once its heap grew by this much.
const RECYCLE_BYTES: usize = 32 * 1024 * 1024;
const PROGRAM: usize = 96 * 1024;

fn clock() -> i64 {
    1_700_000_000
}

/// Image A: apps 0-4 at 0.1.0.
fn image_a() -> Image {
    Image::new(vec![
        (
            "os.lazy.app0",
            "0.1.0",
            package("os.lazy.app0", "0.1.0", PROGRAM, 0),
        ),
        (
            "os.lazy.app1",
            "0.1.0",
            package("os.lazy.app1", "0.1.0", PROGRAM, 1),
        ),
        (
            "os.lazy.app2",
            "0.1.0",
            package("os.lazy.app2", "0.1.0", PROGRAM, 2),
        ),
        (
            "os.lazy.app3",
            "0.1.0",
            package("os.lazy.app3", "0.1.0", PROGRAM, 3),
        ),
        (
            "os.lazy.app4",
            "0.1.0",
            package("os.lazy.app4", "0.1.0", PROGRAM, 4),
        ),
    ])
}

/// Image B: app0 dropped, app1 upgraded to 0.2.0, app2 rebuilt at 0.1.0, app5
/// added, and a damaged archive for app6.
fn image_b() -> Image {
    let mut broken = package("os.lazy.app6", "0.1.0", PROGRAM, 6);
    let middle = broken.len() / 2;
    broken[middle] ^= 0xff;
    Image::new(vec![
        (
            "os.lazy.app1",
            "0.2.0",
            package("os.lazy.app1", "0.2.0", PROGRAM, 11),
        ),
        (
            "os.lazy.app2",
            "0.1.0",
            package("os.lazy.app2", "0.1.0", PROGRAM, 12),
        ),
        (
            "os.lazy.app3",
            "0.1.0",
            package("os.lazy.app3", "0.1.0", PROGRAM, 3),
        ),
        (
            "os.lazy.app4",
            "0.1.0",
            package("os.lazy.app4", "0.1.0", PROGRAM, 4),
        ),
        (
            "os.lazy.app5",
            "0.1.0",
            package("os.lazy.app5", "0.1.0", PROGRAM, 5),
        ),
        ("os.lazy.app6", "0.1.0", broken),
    ])
}

/// `/apps` holds exactly one version directory per row, at the row's
/// install directory, and `/docs/apps` exactly one directory per row.
fn check_tree(fs: &Ext2, pkgd: &Pkgd, pass: usize) {
    let mut tree_fs = Ext2Tree(fs);
    let mut apps = tree_fs.list(layout::APPS_ROOT).unwrap();
    apps.sort();
    let names: Vec<String> = pkgd.rows.keys().cloned().collect();
    assert_eq!(apps, names, "pass {pass}: /apps");
    for (name, row) in &pkgd.rows {
        let versions = tree_fs.list(&layout::app_dir(name).unwrap()).unwrap();
        let expected = row.install_dir.rsplit('/').next().unwrap();
        assert_eq!(versions, [expected], "pass {pass}: {name}");
    }
    let mut docs = tree_fs.list(layout::DOCS_ROOT).unwrap();
    docs.sort();
    assert_eq!(docs, names, "pass {pass}: /docs/apps");
}

fn log_count(fs: &Ext2) -> u64 {
    let size = fs.lookup(layout::LOG_FILE).unwrap().size as usize;
    let mut log = vec![0u8; size];
    fs.read(layout::LOG_FILE, 0, &mut log).unwrap();
    verify(std::str::from_utf8(&log).unwrap())
        .expect("pkg.log verifies")
        .count
}

#[test]
fn two_hundred_passes_across_two_images_stay_consistent() {
    let io = MemIo::new(64 << 20);
    let geometry = Geometry {
        block_size: 4096,
        blocks_count: (64 << 20) / 4096,
        bytes_per_inode: 16 * 1024,
    };
    ext2fs::format(&io, geometry, "lazyos-root", [5; 16], clock()).unwrap();
    let images = [image_a(), image_b()];
    let mut pkgd = Pkgd::new();
    let mut worst = 0;
    for pass in 0..PASSES {
        // Each pass is a boot: a fresh mount of the same volume.
        let fs = Ext2::open(Box::new(io.clone()), clock).unwrap();
        if pass == 0 {
            for dir in ["/apps", "/docs", "/docs/apps", "/logs"] {
                fs.mkdir_p(dir, 0o755, 0, 0).unwrap();
            }
        }
        let image = &images[pass % 2];
        let (tally, growth) = measure(|| pkgd.provision(&fs, image));
        let tally = tally.expect("a changed image is never up to date");
        worst = worst.max(growth);
        let expected = match pass {
            0 => Tally {
                installed: 5,
                ..Tally::default()
            },
            // A -> B: app1 upgraded, app2 rebuilt, app5 installed, app6 fails.
            1 => Tally {
                installed: 1,
                upgraded: 2,
                failed: 1,
                ..Tally::default()
            },
            // B -> A: app2 back; app1's 0.2.0 is newer than A's, so it stays;
            // app0 was kept (demoted, then marked core again).
            p if p % 2 == 0 => Tally {
                upgraded: 1,
                kept: 1,
                ..Tally::default()
            },
            _ => Tally {
                upgraded: 1,
                failed: 1,
                ..Tally::default()
            },
        };
        assert_eq!(tally, expected, "pass {pass}");
        // The same image again (a plain reboot) is a no-op, except that a
        // package which failed has no row, so it is retried at every boot.
        let reboot = if pass % 2 == 0 {
            None
        } else {
            Some(Tally {
                failed: 1,
                ..Tally::default()
            })
        };
        assert_eq!(pkgd.provision(&fs, image), reboot, "pass {pass}: reboot");
        check_tree(&fs, &pkgd, pass);
        let core: Vec<&str> = pkgd
            .rows
            .iter()
            .filter(|(_, row)| row.core)
            .map(|(name, _)| name.as_str())
            .collect();
        let shipped_ok: Vec<&str> = image
            .packages
            .iter()
            .map(|(s, _)| s.system_name.as_str())
            .filter(|name| *name != "os.lazy.app6")
            .collect();
        assert_eq!(core, shipped_ok, "pass {pass}: the core rows");
        assert!(log_count(&fs) > 0);
        assert!(
            growth < RECYCLE_BYTES / 4,
            "pass {pass}: heap grew by {growth} bytes"
        );
        fs.flush().unwrap();
        drop(fs);
        if pass % 10 == 9 {
            let problems = fsck(&io.snapshot());
            assert!(problems.is_empty(), "pass {pass}: {problems:#?}");
        }
    }
    // The last pass shipped image B: the app it dropped stays installed, as a
    // user app.
    assert!(!pkgd.rows["os.lazy.app0"].core);
    assert!(pkgd.rows["os.lazy.app5"].core);
    assert_eq!(pkgd.rows["os.lazy.app1"].version, "0.2.0");
    let problems = fsck(&io.snapshot());
    assert!(problems.is_empty(), "{problems:#?}");
    println!("worst pass grew the heap by {worst} bytes");
}
