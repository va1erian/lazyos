//! A stick and a Linux-shaped disk image in a temporary directory: the
//! stick's ramdisk is written by the build's own `usb_ramdisk::write`, the
//! disk carries a GPT (ESP, a Linux root, `lazyos-system`, `lazyos-home`)
//! and a FAT32 ESP.

use std::path::{Path, PathBuf};

use crate::os_image::{OsFile, Placement, Source};
use crate::os_layout;
use crate::usb_ramdisk;

pub const PASSWD: &str = "user:1000:1000:x:/home/user:/system/bin/sh\n\
                          admin:1001:1001:x:/home/admin:/system/bin/sh\n";

pub struct Rig {
    pub dir: PathBuf,
    pub stick: PathBuf,
    pub image: PathBuf,
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A fresh rig named `name`, its stick carrying `files` (see [`files`]).
pub fn rig(name: &str, files: &[OsFile], kernel: &[u8]) -> Rig {
    let dir = std::env::temp_dir().join(format!("lazyos-install-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let rig = Rig {
        stick: dir.join("stick"),
        image: dir.join("disk.img"),
        dir,
    };
    write_stick(&rig.stick, files, kernel);
    write_disk(&rig.image);
    rig
}

/// The stick's OS files: passwd, two programs, a larger data file and the
/// account database (a seed), plus `extra`.
pub fn files(extra: &[(&str, &[u8])]) -> Vec<OsFile> {
    let bytes = |b: &[u8]| Source::Bytes(b.to_vec());
    let mut out = vec![
        OsFile {
            path: fhs::etc::PASSWD.into(),
            source: bytes(PASSWD.as_bytes()),
            mode: 0o644,
            placement: Placement::ROOT,
        },
        OsFile {
            path: "/system/bin/init".into(),
            source: bytes(b"\x7fELF init"),
            mode: 0o755,
            placement: Placement::ROOT,
        },
        OsFile {
            path: "/system/share/demo/data.bin".into(),
            source: Source::Bytes((0..300_000u32).map(|n| (n % 251) as u8).collect()),
            mode: 0o644,
            placement: Placement::ROOT,
        },
        OsFile {
            path: fhs::state::ACCOUNTS_DB.into(),
            source: bytes(b"seeded accounts\n"),
            mode: 0o600,
            placement: Placement::seed(accountdb::ACCOUNTS_UID, accountdb::ACCOUNTS_UID),
        },
    ];
    for (path, data) in extra {
        out.push(OsFile {
            path: (*path).into(),
            source: bytes(data),
            mode: 0o755,
            placement: Placement::ROOT,
        });
    }
    out
}

/// (Re)write the stick directory.
pub fn write_stick(stick: &Path, files: &[OsFile], kernel: &[u8]) {
    std::fs::create_dir_all(stick.join("efi/boot")).unwrap();
    let mut loader = b"MZ".to_vec();
    loader.resize(8192, 0x90);
    std::fs::write(stick.join("efi/boot/bootx64.efi"), loader).unwrap();
    std::fs::write(stick.join("kernel-x86_64"), kernel).unwrap();
    std::fs::write(stick.join("boot.json"), br#"{"frame_buffer":{}}"#).unwrap();
    let accounts = os_layout::parse_passwd(PASSWD);
    let settings = usb_ramdisk::Settings {
        uuid: crate::os_image::new_uuid(),
        root_free: 1 << 20,
    };
    usb_ramdisk::write(
        &stick.join("ramdisk"),
        &settings,
        &os_layout::dirs(&accounts),
        files,
    )
    .unwrap();
}

/// A kernel image of `len` bytes starting with the ELF magic, tagged `tag`.
pub fn kernel(tag: u8, len: usize) -> Vec<u8> {
    let mut bytes = b"\x7fELF".to_vec();
    bytes.resize(len, tag);
    bytes
}

fn write_disk(image: &Path) {
    let (parts, disk) = crate::testdisk::linux_box(40, 8, 48, 24);
    crate::testdisk::write(image, &parts, disk).unwrap();
}
