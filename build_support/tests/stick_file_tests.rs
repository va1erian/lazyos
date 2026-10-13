//! The stick built from a boot part that is already a file (`compose_file`,
//! `tidy_file`): the same image as the in-memory path, without reading or
//! writing the boot part twice.

use std::io::Cursor;

use ext2fs::check::fsck;

use crate::layout_tests::fake_mbr;
use crate::os_disk::{mbr_entry, SECTOR};
use crate::os_layout::parse_passwd;
use crate::usb_fat;
use crate::usb_ramdisk::fat_read;
use crate::usb_stick::{compose_file, home_dirs, Settings};
use crate::usb_tests::fatfs_boot_volume;

const MIB: u64 = 1 << 20;

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("lazyos-stickfile-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn open_rw(path: &std::path::Path) -> std::fs::File {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap()
}

/// A bootloader-shaped head: stage 2 at LBA 1, FAT from LBA 5, 3000 sectors.
fn boot_head() -> Vec<u8> {
    let mut boot = fake_mbr(3000);
    boot.resize(3005 * 512, 0xC3);
    boot[..512].copy_from_slice(&fake_mbr(3000));
    boot
}

#[test]
fn tidy_file_fixes_a_partition_in_place_like_tidy_does_in_memory() {
    let dir = scratch("tidy");
    let volume = fatfs_boot_volume();
    // The partition sits after 3 sectors of other things, as in a disk image.
    let start = 3 * SECTOR;
    let mut disk = vec![0xA5u8; start as usize];
    disk.extend_from_slice(&volume);
    let path = dir.join("disk.img");
    std::fs::write(&path, &disk).unwrap();

    let fixed = usb_fat::tidy_file(&open_rw(&path), start, volume.len() as u64).unwrap();
    assert_eq!(fixed, 2, "efi and efi/boot");

    let mut expected = volume.clone();
    usb_fat::tidy(&mut expected).unwrap();
    let after = std::fs::read(&path).unwrap();
    assert_eq!(
        &after[..start as usize],
        &disk[..start as usize],
        "outside it"
    );
    assert_eq!(&after[start as usize..], &expected[..]);
    assert_eq!(
        fat_read(
            Cursor::new(after[start as usize..].to_vec()),
            "efi/boot/bootx64.efi"
        )
        .unwrap(),
        b"MZ loader"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn tidy_file_works_on_a_partition_larger_than_its_first_window() {
    let dir = scratch("wide");
    let mut volume = fatfs_boot_volume();
    volume.resize(20 * MIB as usize, 0);
    let path = dir.join("big.img");
    std::fs::write(&path, &volume).unwrap();
    let fixed = usb_fat::tidy_file(&open_rw(&path), 0, volume.len() as u64).unwrap();
    assert_eq!(fixed, 2);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn tidy_file_refuses_what_is_not_a_fat_partition() {
    let dir = scratch("junk");
    let path = dir.join("junk.img");
    std::fs::write(&path, vec![0x5Au8; 64 << 10]).unwrap();
    assert!(usb_fat::tidy_file(&open_rw(&path), 0, 64 << 10).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_boot_part_is_tidied_in_place_and_the_home_volume_follows() {
    let dir = scratch("compose");
    let boot = boot_head();
    let home = home_dirs(&parse_passwd(
        "alice:1000:1000:lazy:/home/alice:sh
",
    ));
    let settings = Settings {
        home_size: 24 * MIB,
    };

    let boot_file = dir.join("lazyos-usb.img.tmp");
    std::fs::write(&boot_file, &boot).unwrap();
    let image = dir.join("lazyos-usb.img");
    let mut tidied = None;
    let stick = compose_file(
        &image,
        &boot_file,
        open_rw(&boot_file),
        &settings,
        &home,
        |_, start, len| {
            tidied = Some((start, len));
            Ok(0)
        },
    )
    .unwrap();

    assert_eq!(tidied, Some((5 * SECTOR, 3000 * SECTOR)));
    assert!(!boot_file.exists(), "renamed into place");
    let bytes = std::fs::read(&image).unwrap();
    assert_eq!(bytes.len() as u64, stick.bytes);
    // The boot part is as it was (but for entry 3).
    assert_eq!(&bytes[512..3005 * 512], &boot[512..]);
    assert_eq!(mbr_entry(&bytes, 1), mbr_entry(&boot, 1));
    assert_eq!(mbr_entry(&bytes, 2), mbr_entry(&boot, 2));
    let (kind, start, sectors) = mbr_entry(&bytes, 3).unwrap();
    assert_eq!((kind, start, sectors), (0x83, 4096, 24 * MIB / SECTOR));
    assert_eq!(start, stick.home_start_lba);
    assert_eq!((start + sectors) * SECTOR, stick.bytes, "home is last");
    let volume = &bytes[(start * SECTOR) as usize..((start + sectors) * SECTOR) as usize];
    assert_eq!(fsck(volume), Vec::<String>::new());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_failed_stick_from_a_file_leaves_neither_image_nor_temp() {
    let dir = scratch("refuse");
    let boot_file = dir.join("lazyos-usb.img.tmp");
    let image = dir.join("lazyos-usb.img");
    let settings = Settings {
        home_size: 16 * MIB,
    };

    // No MBR in it.
    std::fs::write(&boot_file, [0u8; 1024]).unwrap();
    let open = open_rw(&boot_file);
    assert!(compose_file(&image, &boot_file, open, &settings, &[], |_, _, _| Ok(0)).is_err());
    assert!(!image.exists());
    assert!(!boot_file.exists());

    // A tidy that fails stops the stick too.
    std::fs::write(&boot_file, boot_head()).unwrap();
    let open = open_rw(&boot_file);
    let error = compose_file(&image, &boot_file, open, &settings, &[], |_, _, _| {
        Err("odd".into())
    })
    .unwrap_err();
    assert!(error.contains("odd"), "{error}");
    assert!(!image.exists());
    assert!(!boot_file.exists());
    let _ = std::fs::remove_dir_all(&dir);
}
