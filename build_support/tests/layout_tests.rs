//! The pure parts: the layout table, sizes, the MBR guard and the manifest diff.

use crate::os_disk::{self, add_os_entry, check_boot_fits, mbr_entry, parse_size};
use crate::os_image::{OsFile, Sink, Source};
use crate::os_layout::{dirs, file_mode, parse_passwd, DirSpec};
use crate::os_manifest::{clean_path, Kind, Manifest};

const PASSWD: &str = "root:0:0:toor:/root:sh\nalice:1000:1000:lazy:/home/alice:sh\n";

fn spec<'a>(all: &'a [DirSpec], path: &str) -> &'a DirSpec {
    all.iter()
        .find(|dir| dir.path == path)
        .unwrap_or_else(|| panic!("{path} is not in the layout"))
}

#[test]
fn layout_has_the_mount_points_and_the_transitional_data_tree() {
    let all = dirs(&parse_passwd(PASSWD));
    for path in [
        "/boot",
        "/home",
        "/transient",
        "/system",
        "/apps",
        "/conf",
        "/logs",
        "/data",
    ] {
        let dir = spec(&all, path);
        assert_eq!((dir.mode, dir.uid, dir.gid), (0o755, 0, 0), "{path}");
    }
    let alice = spec(&all, "/data/home/alice");
    assert_eq!((alice.mode, alice.uid, alice.gid), (0o755, 1000, 1000));
    let tmp = spec(&all, "/data/tmp");
    assert_eq!((tmp.mode, tmp.uid, tmp.gid), (0o1777, 0, 0));
    // root's /root is not a home on the data tree, and nothing is listed twice.
    assert!(all.iter().all(|dir| dir.path != "/data/home/root"));
    let mut seen: Vec<&str> = all.iter().map(|dir| dir.path.as_str()).collect();
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), all.len());
}

#[test]
fn parents_come_before_children() {
    let all = dirs(&parse_passwd(PASSWD));
    for (index, dir) in all.iter().enumerate() {
        let parent = dir.path.rsplit_once('/').unwrap().0;
        assert!(
            parent.is_empty() || all[..index].iter().any(|d| d.path == parent),
            "{} precedes its parent",
            dir.path
        );
    }
}

#[test]
fn passwd_parsing_skips_bad_lines_and_hostile_names() {
    let text = "a:1:2:x:/home/a:sh\nbroken\n../evil:3:4:x:/home/e:sh\nb/c:5:6:x:/home/b:sh\n:7:8:x:/h:sh\nz:x:1:x:/h:sh\n";
    let accounts = parse_passwd(text);
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0].name, "a");
}

#[test]
fn modes_follow_the_directory() {
    assert_eq!(file_mode("/system/bin/init"), 0o755);
    assert_eq!(file_mode("system/bin/busybox"), 0o755);
    assert_eq!(file_mode("/docs/os/README.md"), 0o644);
    assert_eq!(file_mode("/system/etc/passwd"), 0o644);
}

#[test]
fn sink_normalises_paths_and_later_entries_win() {
    let mut files = crate::os_image::OsFiles::default();
    files.add_bytes("PASSWD", b"one".to_vec());
    files.add_bytes("/PASSWD", b"two".to_vec());
    files.add_bytes("docs//a/../b.md", b"bad".to_vec());
    files.add_bytes("docs/a.md", b"doc".to_vec());
    let all = files.files();
    assert_eq!(all.len(), 2);
    let passwd = all.iter().find(|f| f.path == "/PASSWD").unwrap();
    assert_eq!(passwd.source, Source::Bytes(b"two".to_vec()));
    assert_eq!(passwd.mode, 0o644);
    assert!(all.iter().any(|f| f.path == "/docs/a.md"));
}

#[test]
fn size_parsing_enforces_the_minimum_and_rounds_to_blocks() {
    assert_eq!(parse_size("512M").unwrap(), 512 << 20);
    assert_eq!(parse_size("128m").unwrap(), 128 << 20);
    assert_eq!(parse_size("2G").unwrap(), 2 << 30);
    assert_eq!(parse_size("134217729").unwrap(), 128 << 20);
    assert!(parse_size("127M").unwrap_err().contains("minimum"));
    assert!(parse_size("lots").is_err());
    assert!(parse_size("").is_err());
    assert!(parse_size("99999999G").is_err());
}

/// A bootloader-shaped MBR: stage 2 at LBA 1, FAT after it.
pub fn fake_mbr(fat_sectors: u32) -> Vec<u8> {
    let mut mbr = vec![0u8; 512];
    let mut entry = |n: usize, kind: u8, start: u32, sectors: u32| {
        let at = 0x1BE + (n - 1) * 16;
        mbr[at + 4] = kind;
        mbr[at + 8..at + 12].copy_from_slice(&start.to_le_bytes());
        mbr[at + 12..at + 16].copy_from_slice(&sectors.to_le_bytes());
    };
    entry(1, 0x20, 1, 4);
    entry(2, 0x0C, 5, fat_sectors);
    mbr[510] = 0x55;
    mbr[511] = 0xAA;
    mbr
}

#[test]
fn the_boot_partition_must_end_before_the_os_volume() {
    let limit = os_disk::OS_START_LBA as u32;
    // Ends exactly at the limit: fine. One sector over: the build fails.
    assert!(check_boot_fits(&fake_mbr(limit - 5)).is_ok());
    let error = check_boot_fits(&fake_mbr(limit - 4)).unwrap_err();
    assert!(error.contains("131072"), "{error}");
    assert!(check_boot_fits(&[0u8; 512]).is_err());
}

#[test]
fn the_os_entry_is_type_83_at_64_mib() {
    let mut mbr = fake_mbr(1000);
    add_os_entry(&mut mbr, 128 << 20);
    assert_eq!(
        mbr_entry(&mbr, 3),
        Some((0x83, 131_072, (128u64 << 20) / 512))
    );
    // The bootloader's two entries are untouched, and entry 3 is now taken.
    assert_eq!(mbr_entry(&mbr, 1), Some((0x20, 1, 4)));
    assert_eq!(mbr_entry(&mbr, 2), Some((0x0C, 5, 1000)));
    assert!(check_boot_fits(&mbr).unwrap_err().contains("entry 3"));
}

fn file(path: &str) -> OsFile {
    OsFile {
        path: path.into(),
        source: Source::Bytes(Vec::new()),
        mode: 0o644,
    }
}

fn manifest(dir_paths: &[&str], files: &[&str]) -> Manifest {
    let dirs: Vec<DirSpec> = dir_paths
        .iter()
        .map(|path| DirSpec {
            path: (*path).into(),
            mode: 0o755,
            uid: 0,
            gid: 0,
        })
        .collect();
    let files: Vec<OsFile> = files.iter().map(|path| file(path)).collect();
    Manifest::of(&dirs, &files).unwrap()
}

#[test]
fn the_manifest_lists_files_dirs_and_implied_parents() {
    let m = manifest(&["/data"], &["/A.ELF", "/docs/x/y.md"]);
    let listed: Vec<_> = m.entries.iter().map(|(p, k)| (p.as_str(), *k)).collect();
    assert_eq!(
        listed,
        [
            ("/A.ELF", Kind::File),
            ("/data", Kind::Dir),
            ("/docs", Kind::Dir),
            ("/docs/x", Kind::Dir),
            ("/docs/x/y.md", Kind::File),
        ]
    );
    assert_eq!(Manifest::parse(&m.to_text()).unwrap(), m);
}

#[test]
fn a_path_cannot_be_both_file_and_directory() {
    let dirs = [DirSpec {
        path: "/docs".into(),
        mode: 0o755,
        uid: 0,
        gid: 0,
    }];
    assert!(Manifest::of(&dirs, &[file("/docs")]).is_err());
}

#[test]
fn manifest_text_from_a_disk_is_not_trusted() {
    for bad in [
        "x /a\n",
        "f a\n",
        "f /a/../b\n",
        "f /a//b\n",
        "d /\n",
        "f /system/.image-manifest\n",
        "nonsense\n",
    ] {
        assert!(Manifest::parse(bad).is_err(), "{bad:?}");
    }
    assert!(Manifest::parse("").unwrap().entries.is_empty());
    assert_eq!(clean_path("a//b/"), Some("/a/b".into()));
    assert_eq!(clean_path("/"), None);
    assert_eq!(clean_path("a\nb"), None);
}

#[test]
fn removed_by_reports_dropped_and_retyped_paths_deepest_first() {
    let old = manifest(
        &["/data", "/old"],
        &["/KEEP.ELF", "/GONE.ELF", "/old/a", "/same"],
    );
    let new = manifest(&["/data"], &["/KEEP.ELF", "/NEW.ELF", "/same/inner"]);
    // `/same` was a file and is now a directory: the old file must go first.
    let removed = old.removed_by(&new);
    let names: Vec<_> = removed.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(names, ["/same", "/old/a", "/old", "/GONE.ELF"]);
    assert_eq!(removed[0].1, Kind::File);
    // Anything in neither manifest never shows up.
    assert!(removed
        .iter()
        .all(|(p, _)| p != "/data" && p != "/KEEP.ELF"));
}
