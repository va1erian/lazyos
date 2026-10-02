//! The USB stick image's pure parts (docs/usb-stick.md): the ramdisk (MBR,
//! FAT `lazyos.cfg`, ext2 OS volume sized to its files) and the stick
//! composition (the bootloader part plus the `lazyhome` partition), checked
//! with the independent fsck-style checker from `libs/ext2fs`.

use std::io::Cursor;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use ext2fs::check::fsck;
use ext2fs::memio::MemIo;
use ext2fs::{BlockIo, Ext2};

use crate::layout_tests::fake_mbr;
use crate::os_disk::{mbr_entry, SECTOR};
use crate::os_image::{format_uuid, OsFile, Source};
use crate::os_layout::{dirs, parse_passwd};
use crate::usb_fat;
use crate::usb_ramdisk::{self, fat_read, fat_volume, Settings, FAT_START_LBA, OS_START_LBA};
use crate::usb_stick::{self, compose, home_dirs, parse_size, HOME_LABEL};

const PASSWD: &str = "root:0:0:toor:/root:sh\nalice:1000:1000:lazy:/home/alice:sh\n";
const MIB: u64 = 1 << 20;

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Scratch {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "lazyos-usb-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn file(path: &str, bytes: Vec<u8>) -> OsFile {
    OsFile {
        path: path.into(),
        source: Source::Bytes(bytes),
        mode: 0o644,
    }
}

/// The ext2 volume in `image` at `lba` for `sectors`, as bytes.
fn partition(image: &[u8], lba: u64, sectors: u64) -> Vec<u8> {
    image[(lba * SECTOR) as usize..((lba + sectors) * SECTOR) as usize].to_vec()
}

fn open(bytes: Vec<u8>) -> Ext2 {
    let io = MemIo::new(bytes.len());
    io.write_sectors(0, &bytes).unwrap();
    Ext2::open(Box::new(io), || 0).unwrap()
}

#[test]
fn ramdisk_carries_the_config_and_the_os_volume() {
    let scratch = Scratch::new();
    let path = scratch.0.join("ramdisk.img");
    let uuid = [0x5A; 16];
    let files = vec![
        file("/system/bin/hello", vec![0x7F; 300_000]),
        file("/system/share/samples/hello.txt", b"hi\n".to_vec()),
    ];
    let all_dirs = dirs(&parse_passwd(PASSWD));
    let settings = Settings {
        uuid,
        root_free: 8 * MIB,
    };
    let written = usb_ramdisk::write(&path, &settings, &all_dirs, &files).unwrap();
    let image = std::fs::read(&path).unwrap();
    assert_eq!(image.len() as u64, written.bytes);

    let (fat_kind, fat_lba, fat_sectors) = mbr_entry(&image, 1).unwrap();
    let (os_kind, os_lba, os_sectors) = mbr_entry(&image, 2).unwrap();
    assert_eq!(
        (fat_kind, fat_lba),
        (usb_ramdisk::FAT12_TYPE, FAT_START_LBA)
    );
    assert_eq!((os_kind, os_lba), (usb_ramdisk::LINUX_TYPE, OS_START_LBA));
    assert_eq!(os_sectors * SECTOR, written.os_bytes);
    assert_eq!(mbr_entry(&image, 3).unwrap().0, 0);
    assert_eq!(&image[510..512], &[0x55, 0xAA]);

    let fat = partition(&image, fat_lba, fat_sectors);
    let cfg = String::from_utf8(fat_read(Cursor::new(fat), "lazyos.cfg").unwrap()).unwrap();
    assert!(
        cfg.contains(&format!("root=UUID={}", format_uuid(uuid))),
        "{cfg}"
    );
    assert!(cfg.contains("home=LABEL=lazyhome"), "{cfg}");

    let os = partition(&image, os_lba, os_sectors);
    assert_eq!(fsck(&os), Vec::<String>::new());
    let volume = open(os);
    assert_eq!(volume.uuid(), uuid);
    assert_eq!(
        volume.read_file("/system/bin/hello").unwrap().len(),
        300_000
    );
    assert_eq!(volume.lookup("/home/alice").unwrap().uid, 1000);
    // The free space asked for is there (give or take the formatter's slack).
    let stats = volume.statfs().unwrap();
    assert!(
        stats.blocks_free * u64::from(stats.block_size) >= 8 * MIB,
        "{} free blocks",
        stats.blocks_free
    );
    // Sized to the contents, not the 512 MiB disk default.
    assert!(
        written.os_bytes < 32 * MIB,
        "{} MiB",
        written.os_bytes >> 20
    );
}

#[test]
fn ramdisk_estimate_covers_many_small_files_and_a_big_one() {
    let scratch = Scratch::new();
    let path = scratch.0.join("ramdisk.img");
    let mut files: Vec<OsFile> = (0..400)
        .map(|n| file(&format!("/docs/os/d{}/f{n}.md", n % 20), vec![b'x'; 5000]))
        .collect();
    files.push(file("/system/bin/big", vec![1; 9 * MIB as usize]));
    let settings = Settings {
        uuid: [1; 16],
        root_free: 0,
    };
    let written = usb_ramdisk::write(&path, &settings, &[], &files).unwrap();
    let image = std::fs::read(&path).unwrap();
    let os = partition(&image, OS_START_LBA, written.os_bytes / SECTOR);
    assert_eq!(fsck(&os), Vec::<String>::new());
    let volume = open(os);
    assert_eq!(volume.read_file("/docs/os/d7/f387.md").unwrap().len(), 5000);
    assert_eq!(
        volume.read_file("/system/bin/big").unwrap().len() as u64,
        9 * MIB
    );
}

#[test]
fn the_boot_fat_volume_is_fat12_and_holds_only_the_config() {
    let volume = fat_volume(b"root=UUID=x\n").unwrap();
    assert_eq!(volume.len() as u64, usb_ramdisk::FAT_SECTORS * SECTOR);
    // FAT12/16 keep the type string at 54; the kernel reads FAT12/16 only.
    assert_eq!(&volume[54..59], b"FAT12");
    assert_eq!(
        fat_read(Cursor::new(volume), "lazyos.cfg").unwrap(),
        b"root=UUID=x\n"
    );
}

#[test]
fn the_stick_appends_an_aligned_valid_home_partition() {
    let scratch = Scratch::new();
    let image = scratch.0.join("lazyos-usb.img");
    // A bootloader-shaped head: stage 2 at LBA 1, FAT from LBA 5, 3000 sectors.
    let mut boot = fake_mbr(3000);
    boot.resize(3005 * 512, 0xC3);
    boot[..512].copy_from_slice(&fake_mbr(3000));
    let home = home_dirs(&parse_passwd(PASSWD));
    let settings = usb_stick::Settings {
        home_size: 24 * MIB,
    };
    let stick = compose(&image, &boot, &settings, &home).unwrap();
    let bytes = std::fs::read(&image).unwrap();
    assert_eq!(bytes.len() as u64, stick.bytes);
    // The boot part is copied verbatim (but for entry 3).
    assert_eq!(&bytes[512..3005 * 512], &boot[512..]);
    assert_eq!(mbr_entry(&bytes, 1), mbr_entry(&boot, 1));
    assert_eq!(mbr_entry(&bytes, 2), mbr_entry(&boot, 2));
    let (kind, start, sectors) = mbr_entry(&bytes, 3).unwrap();
    assert_eq!((kind, start, sectors), (0x83, 4096, 24 * MIB / SECTOR));
    assert_eq!(start, stick.home_start_lba);
    assert_eq!((start + sectors) * SECTOR, stick.bytes, "home is last");

    let volume_bytes = partition(&bytes, start, sectors);
    assert_eq!(fsck(&volume_bytes), Vec::<String>::new());
    let volume = open(volume_bytes);
    assert_eq!(&volume.label()[..HOME_LABEL.len()], HOME_LABEL.as_bytes());
    assert_eq!(volume.uuid(), stick.home_uuid);
    let alice = volume.lookup("/alice").unwrap();
    assert_eq!(
        (alice.mode & 0o7777, alice.uid, alice.gid),
        (0o700, 1000, 1000)
    );
    assert!(
        volume.lookup("/root").is_err(),
        "root's home is not on /home"
    );
    assert!(volume.lookup("/home").is_err(), "the volume root is /home");
    assert!(!scratch.0.join("lazyos-usb.img.tmp").exists());
}

#[test]
fn the_stick_refuses_a_boot_image_it_cannot_extend() {
    let scratch = Scratch::new();
    let image = scratch.0.join("lazyos-usb.img");
    let settings = usb_stick::Settings {
        home_size: 16 * MIB,
    };
    // No MBR signature.
    assert!(compose(&image, &[0u8; 1024], &settings, &[]).is_err());
    // Shorter than its FAT partition claims.
    assert!(compose(&image, &fake_mbr(3000), &settings, &[]).is_err());
    // Entry 3 already taken.
    let mut taken = fake_mbr(4);
    taken.resize(9 * 512, 0);
    usb_ramdisk::set_entry(&mut taken[..512], 3, 0x83, 100, 10);
    assert!(compose(&image, &taken, &settings, &[]).is_err());
    assert!(!image.exists());
}

#[test]
fn sizes_parse_with_suffixes_and_a_floor() {
    let min = usb_stick::MIN_HOME_SIZE;
    assert_eq!(parse_size("V", "1G", min).unwrap(), 1 << 30);
    assert_eq!(parse_size("V", "64m", min).unwrap(), 64 * MIB);
    assert_eq!(parse_size("V", "20000K", min).unwrap(), 20000 * 1024);
    assert_eq!(parse_size("V", "0", 0).unwrap(), 0);
    assert!(parse_size("V", "1M", min).unwrap_err().contains("minimum"));
    assert!(parse_size("V", "lots", min).is_err());
    assert!(parse_size("V", "99999999G", min)
        .unwrap_err()
        .contains("too large"));
}

#[test]
fn home_dirs_follow_the_passwd_homes() {
    let home = home_dirs(&parse_passwd(
        "root:0:0:x:/root:sh\nalice:1000:1000:x:/home/alice:sh\nsvc:7:7:x:/var/svc:sh\n",
    ));
    assert_eq!(home.len(), 1);
    assert_eq!(
        (home[0].path.as_str(), home[0].mode, home[0].uid),
        ("/alice", 0o700, 1000)
    );
}

/// A FAT volume shaped like the bootloader's: `fatfs` 0.3 with `efi/boot/`
/// and a file in it, and a lowercase label.
fn fatfs_boot_volume() -> Vec<u8> {
    let mut disk = Cursor::new(vec![0u8; 4 << 20]);
    let options = fatfs::FormatVolumeOptions::new().volume_label(*b"kernel     ");
    fatfs::format_volume(&mut disk, options).unwrap();
    {
        let fs = fatfs::FileSystem::new(&mut disk, fatfs::FsOptions::new()).unwrap();
        fs.root_dir().create_dir("efi").unwrap();
        fs.root_dir().create_dir("efi/boot").unwrap();
        let mut file = fs.root_dir().create_file("efi/boot/bootx64.efi").unwrap();
        std::io::Write::write_all(&mut file, b"MZ loader").unwrap();
    }
    disk.into_inner()
}

/// The first two slots of the directory whose first cluster is `cluster`.
fn dir_slots(volume: &[u8], cluster: usize) -> (Vec<u8>, Vec<u8>) {
    let le16 = |at: usize| usize::from(u16::from_le_bytes([volume[at], volume[at + 1]]));
    let sector = le16(11);
    let root = (le16(14) + usize::from(volume[16]) * le16(22)) * sector;
    let data = root + le16(17) * 32;
    let at = data + (cluster - 2) * sector * usize::from(volume[13]);
    (
        volume[at..at + 11].to_vec(),
        volume[at + 32..at + 43].to_vec(),
    )
}

#[test]
fn tidy_puts_dot_entries_first_and_relabels() {
    let mut volume = fatfs_boot_volume();
    // fatfs 0.3 puts a long-name slot before `.` (the bug being fixed).
    assert_ne!(dir_slots(&volume, 2).0, b".          ".to_vec());
    assert_eq!(usb_fat::tidy(&mut volume).unwrap(), 2, "efi and efi/boot");
    for cluster in [2, 3] {
        let (first, second) = dir_slots(&volume, cluster);
        assert_eq!(first, b".          ".to_vec(), "cluster {cluster}");
        assert_eq!(second, b"..         ".to_vec(), "cluster {cluster}");
    }
    assert_eq!(&volume[43..54], usb_fat::LABEL);
    // Still readable, the file intact; a second pass changes nothing.
    let bytes = fat_read(Cursor::new(volume.clone()), "efi/boot/bootx64.efi").unwrap();
    assert_eq!(bytes, b"MZ loader");
    assert_eq!(usb_fat::tidy(&mut volume).unwrap(), 0);
    let fs = fatfs::FileSystem::new(Cursor::new(volume), fatfs::FsOptions::new()).unwrap();
    assert_eq!(fs.volume_label().trim(), "LAZYOS");
}

#[test]
fn tidy_refuses_garbage_without_panicking() {
    assert!(usb_fat::tidy(&mut []).is_err());
    assert!(usb_fat::tidy(&mut vec![0u8; 512]).is_err());
    let mut state = 0x1234_5678_9ABC_DEF1u64;
    for _ in 0..2000 {
        let mut volume = fatfs_boot_volume_prefix();
        for _ in 0..8 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let at = (state as usize) % volume.len();
            volume[at] = (state >> 32) as u8;
        }
        let _ = usb_fat::tidy(&mut volume);
    }
}

/// The first 64 KiB of a bootloader-shaped volume: the boot sector, the FATs
/// and the directories, cut short so corrupted offsets run off the end.
fn fatfs_boot_volume_prefix() -> Vec<u8> {
    static VOLUME: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    VOLUME.get_or_init(fatfs_boot_volume)[..64 << 10].to_vec()
}

#[test]
fn the_stick_requires_usb_input_and_init() {
    let usbd = file(fhs::bin::USBD, b"\x7fELF".to_vec());
    let other = file(fhs::bin::INIT, b"\x7fELF".to_vec());
    assert!(usb_stick::check_profile(true, true, &[other.clone(), usbd.clone()]).is_ok());
    let missing = usb_stick::check_profile(false, true, std::slice::from_ref(&usbd)).unwrap_err();
    assert!(missing.contains("LAZYOS_USB=1"), "{missing}");
    assert!(usb_stick::check_profile(true, false, &[usbd]).is_err());
    let absent = usb_stick::check_profile(true, true, &[other]).unwrap_err();
    assert!(absent.contains("usbd"), "{absent}");
}
