//! The stick's RAM root has room for what `pkgd` unpacks at first boot
//! (issue #703): `usb_ramdisk::installed_package_bytes` counts the core
//! packages in `/system/packages` only, and a ramdisk written with no other
//! free space can still take every package unpacked.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use ext2fs::memio::MemIo;
use ext2fs::{BlockIo, Ext2};

use crate::os_disk::{mbr_entry, SECTOR};
use crate::os_image::{OsFile, Placement, Source};
use crate::usb_ramdisk::{self, installed_package_bytes, Settings};

const MIB: u64 = 1 << 20;

fn file(path: &str, bytes: Vec<u8>) -> OsFile {
    OsFile {
        path: path.into(),
        source: Source::Bytes(bytes),
        mode: 0o644,
        placement: Placement::ROOT,
    }
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & (crc & 1).wrapping_neg());
        }
    }
    !crc
}

/// A stored (uncompressed) zip of `members`, as `tools/pkg/build.py` would
/// write it minus the compression.
fn zip(members: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let (mut out, mut central) = (Vec::new(), Vec::new());
    let u16le = |v: &mut Vec<u8>, x: u16| v.extend_from_slice(&x.to_le_bytes());
    let u32le = |v: &mut Vec<u8>, x: u32| v.extend_from_slice(&x.to_le_bytes());
    for (name, data) in members {
        let (offset, crc, len) = (out.len() as u32, crc32(data), data.len() as u32);
        u32le(&mut out, 0x0403_4B50);
        for x in [20, 0, 0, 0, 0] {
            u16le(&mut out, x);
        }
        for x in [crc, len, len] {
            u32le(&mut out, x);
        }
        u16le(&mut out, name.len() as u16);
        u16le(&mut out, 0);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
        u32le(&mut central, 0x0201_4B50);
        for x in [20, 20, 0, 0, 0, 0] {
            u16le(&mut central, x);
        }
        for x in [crc, len, len] {
            u32le(&mut central, x);
        }
        for x in [name.len() as u16, 0, 0, 0, 0] {
            u16le(&mut central, x);
        }
        u32le(&mut central, 0);
        u32le(&mut central, offset);
        central.extend_from_slice(name.as_bytes());
    }
    let (at, size) = (out.len() as u32, central.len() as u32);
    out.extend_from_slice(&central);
    u32le(&mut out, 0x0605_4B50);
    for x in [0, 0, members.len() as u16, members.len() as u16] {
        u16le(&mut out, x);
    }
    u32le(&mut out, size);
    u32le(&mut out, at);
    u16le(&mut out, 0);
    out
}

/// A valid package `os.lazy.<short>` with a `binary`-byte program and one
/// `doc`-byte document.
fn package(short: &str, binary: usize, doc: usize) -> Vec<u8> {
    let manifest = format!(
        "[app]\nname = \"T\"\nsystem_name = \"os.lazy.{short}\"\nauthor = \"T\"\nversion = \"1.0.0\"\n\n[entry]\nbinary = \"bin/{short}.elf\"\n"
    );
    let png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    zip(&[
        ("manifest.toml", manifest.into_bytes()),
        (&format!("bin/{short}.elf"), vec![0x7F; binary]),
        ("icons/app-16.png", png.clone()),
        ("icons/app-32.png", png.clone()),
        ("icons/app-128.png", png),
        ("docs/readme.md", vec![b'#'; doc]),
    ])
}

#[test]
fn counts_only_core_packages_unpacked() {
    let core = package("big", 3 * MIB as usize, 100_000);
    assert!(
        lazypkg::Package::open(&core).is_ok(),
        "the test package is valid"
    );
    let files = vec![
        file("/system/packages/os.lazy.big.lzp", core.clone()),
        // A sample package is installed by hand, not at boot.
        file(
            "/system/share/samples/doom.lzp",
            package("doom", 8 * MIB as usize, 0),
        ),
        file("/system/packages/index", b"os.lazy.big 1.0.0\n".to_vec()),
        file("/system/bin/hello", vec![1; 5 * MIB as usize]),
    ];
    let bytes = installed_package_bytes(&files).unwrap();
    // The program and the document twice (in /apps and in /docs/apps).
    assert!(bytes >= 3 * MIB + 200_000, "{bytes}");
    assert!(
        bytes < 6 * MIB,
        "a sample or a non-package was counted: {bytes}"
    );
    assert_eq!(bytes % MIB, 0);
    assert_eq!(installed_package_bytes(&files[1..]).unwrap(), 0);
}

#[test]
fn a_broken_core_package_fails_the_build() {
    let files = vec![file(
        "/system/packages/os.lazy.bad.lzp",
        b"not a zip".to_vec(),
    )];
    let error = installed_package_bytes(&files).unwrap_err();
    assert!(error.contains("os.lazy.bad.lzp"), "{error}");
}

#[test]
fn the_ram_root_takes_every_package_unpacked() {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "lazyos-usb-root-test-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path: PathBuf = dir.join("ramdisk.img");
    let files: Vec<OsFile> = (0..6)
        .map(|n| {
            let short = format!("app{n}");
            file(
                &format!("/system/packages/os.lazy.{short}.lzp"),
                package(&short, (n + 1) * MIB as usize, 20_000),
            )
        })
        .collect();
    let installed = installed_package_bytes(&files).unwrap();
    let settings = Settings {
        uuid: [7; 16],
        root_free: 0,
    };
    usb_ramdisk::write(&path, &settings, &[], &files).unwrap();
    let image = std::fs::read(&path).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let (_, lba, sectors) = mbr_entry(&image, 2).unwrap();
    let os = &image[(lba * SECTOR) as usize..((lba + sectors) * SECTOR) as usize];
    let io = MemIo::new(os.len());
    io.write_sectors(0, os).unwrap();
    let volume = Ext2::open(Box::new(io), || 0).unwrap();
    let stats = volume.statfs().unwrap();
    let free = stats.blocks_free * u64::from(stats.block_size);
    // 21 MiB of programs: with no free space asked for, the volume still
    // has room to unpack them all.
    assert!(installed >= 21 * MIB, "{installed}");
    assert!(free >= installed, "{free} bytes free, {installed} needed");
}
