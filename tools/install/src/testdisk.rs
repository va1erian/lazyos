//! Disks for testing the installer without Linux's disk tools: a
//! Linux-shaped GPT disk image (ESP formatted FAT32, a Linux root stand-in,
//! `lazyos-system`, `lazyos-home`), files put on its ESP, and a stick
//! image's FAT partition unpacked to a directory. Used by the host tests and
//! by `tools/install/run.py` through `examples/testdisk.rs` (I3).

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use gpt::guid::{Guid, ESP, LINUX_DATA};
use gpt::write::Spec;

use crate::esp::{ImageEsp, Window};

/// One partition: start and size in MiB, GPT name, whether it is the ESP.
#[derive(Clone, Debug)]
pub struct PartSpec {
    pub start_mib: u64,
    pub mib: u64,
    pub name: String,
    pub esp: bool,
}

/// ESP, a Linux root stand-in, the two LazyOS partitions, 1 MiB aligned, and
/// the disk size that holds them plus the backup table.
pub fn linux_box(esp: u64, linux: u64, system: u64, home: u64) -> (Vec<PartSpec>, u64) {
    let mut at = 1;
    let mut parts = Vec::new();
    for (mib, name, is_esp) in [
        (esp, "EFI System Partition", true),
        (linux, "linux", false),
        (system, crate::plan::SYSTEM_NAME, false),
        (home, crate::plan::HOME_NAME, false),
    ] {
        parts.push(PartSpec {
            start_mib: at,
            mib,
            name: name.into(),
            esp: is_esp,
        });
        at += mib;
    }
    (parts, at + 1)
}

/// The FAT serial the test ESP gets (`1A2B-3C4D`).
pub const ESP_SERIAL: u32 = 0x1A2B_3C4D;

/// Write a disk of `disk_mib` with `parts` (GPT; the ESP formatted FAT32).
pub fn write(image: &Path, parts: &[PartSpec], disk_mib: u64) -> Result<(), String> {
    let sectors = disk_mib << 11;
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(image)
        .map_err(|e| format!("{}: {e}", image.display()))?;
    file.set_len(sectors * 512).map_err(|e| e.to_string())?;
    let specs: Vec<Spec> = parts
        .iter()
        .enumerate()
        .map(|(n, part)| Spec {
            type_guid: if part.esp { ESP } else { LINUX_DATA },
            unique_guid: Guid([n as u8 + 1; 16]),
            first_lba: part.start_mib << 11,
            last_lba: ((part.start_mib + part.mib) << 11) - 1,
            name: &part.name,
        })
        .collect();
    let layout =
        gpt::write::build(sectors, Guid([0x42; 16]), &specs).map_err(|e| format!("GPT: {e:?}"))?;
    write_at(&mut file, 0, &layout.head)?;
    write_at(&mut file, layout.tail_lba * 512, &layout.tail)?;
    let esp = parts
        .iter()
        .find(|part| part.esp)
        .ok_or("no ESP in the layout")?;
    let window = Window::new(file, esp.start_mib << 20, esp.mib << 20);
    let options = fatfs::FormatVolumeOptions::new()
        .fat_type(fatfs::FatType::Fat32)
        .bytes_per_cluster(512)
        .volume_id(ESP_SERIAL)
        .volume_label(*b"ESP        ");
    fatfs::format_volume(window, options).map_err(|e| format!("format the ESP: {e}"))
}

fn write_at(file: &mut File, at: u64, bytes: &[u8]) -> Result<(), String> {
    file.seek(SeekFrom::Start(at))
        .and_then(|_| file.write_all(bytes))
        .map_err(|e| e.to_string())
}

fn image_esp(image: &Path) -> Result<ImageEsp, String> {
    let mut file = File::open(image).map_err(|e| format!("{}: {e}", image.display()))?;
    let length = file.metadata().map_err(|e| e.to_string())?.len();
    let table = crate::target::read_gpt(&mut file, length)?;
    let entry = table
        .entries
        .iter()
        .find(|e| e.type_guid == ESP)
        .ok_or("no ESP")?;
    Ok(ImageEsp {
        path: image.to_path_buf(),
        start: entry.first_lba * 512,
        len: entry.sectors() * 512,
    })
}

/// Put `bytes` at `path` on the ESP of `image` (found by its GPT type).
pub fn esp_put(image: &Path, path: &str, bytes: &[u8]) -> Result<(), String> {
    image_esp(image)?.write_file(path.trim_start_matches('/'), bytes)
}

/// `path` on the ESP of `image`, when it is there.
pub fn esp_get(image: &Path, path: &str) -> Result<Option<Vec<u8>>, String> {
    crate::esp::Esp::read(&mut image_esp(image)?, path.trim_start_matches('/'))
}

fn system_extent(image: &Path) -> Result<crate::populate::Extent, String> {
    let options = crate::args::parse(["status".to_string()])?;
    Ok(crate::target::in_image(image, &options)?.system_extent)
}

/// `/releases/grubenv` on the system volume of `image` (U2a), read from the host.
pub fn grubenv_get(image: &Path) -> Result<Vec<u8>, String> {
    crate::grubenv::read(&system_extent(image)?)
}

/// Overwrite `/releases/grubenv` of `image` in place with the 1024-byte `bytes`.
pub fn grubenv_put(image: &Path, bytes: &[u8]) -> Result<(), String> {
    crate::grubenv::overwrite(&system_extent(image)?, bytes)
}

/// The installed volumes of `image`, from the host: the checker's verdict
/// on both, and what LazyOS wrote while it ran (`/apps`, `/logs`, homes).
/// One `key: value` line each, for `tools/install/judge.py`.
pub fn check(image: &Path) -> Result<String, String> {
    let options = crate::args::parse(["status".to_string()])?;
    let found = crate::target::in_image(image, &options)?;
    let mut lines = Vec::new();
    for (role, extent) in [
        ("system", &found.system_extent),
        ("home", &found.home_extent),
    ] {
        let verdict =
            crate::verify::check_volume(extent, u64::MAX).unwrap_or_else(|e| format!("FAIL {e}"));
        lines.push(format!("{role}.check: {}", verdict.replace('\n', " | ")));
    }
    let open = |extent: &crate::populate::Extent| {
        ext2fs::Ext2::open(Box::new(extent.io(false)?), crate::os_image::now)
            .map_err(|e| format!("{e:?}"))
    };
    let system = open(&found.system_extent)?;
    let count = |path: &str| {
        system
            .readdir(path)
            .map_or(0, |entries| entries.len().saturating_sub(2))
    };
    lines.push(format!("system.apps: {}", count(fhs::state::APPS_ROOT)));
    lines.push(format!("system.logs: {}", count(fhs::state::LOGS_ROOT)));
    let home = open(&found.home_extent)?;
    let user = home
        .readdir("/user")
        .map_or(0, |entries| entries.len().saturating_sub(2));
    lines.push(format!("home.user_entries: {user}"));
    Ok(lines.join("\n"))
}

/// Unpack the FAT partition (MBR entry 2) of a stick image into `dir`.
pub fn unpack_stick(image: &Path, dir: &Path) -> Result<usize, String> {
    let mut file = File::open(image).map_err(|e| format!("{}: {e}", image.display()))?;
    let mut mbr = [0u8; 512];
    file.read_exact(&mut mbr).map_err(|e| e.to_string())?;
    let (_, start, sectors) =
        crate::os_disk::mbr_entry(&mbr, 2).ok_or("the stick image has no MBR entry 2")?;
    let window = Window::new(file, start * 512, sectors * 512);
    let fs = fatfs::FileSystem::new(window, fatfs::FsOptions::new())
        .map_err(|e| format!("the stick's FAT: {e}"))?;
    let count = unpack_dir(&fs.root_dir(), dir)?;
    Ok(count)
}

fn unpack_dir(at: &fatfs::Dir<'_, Window>, out: &Path) -> Result<usize, String> {
    std::fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;
    let mut count = 0;
    for entry in at.iter() {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name();
        if name == "." || name == ".." {
            continue;
        }
        let target = out.join(&name);
        if entry.is_dir() {
            count += unpack_dir(&entry.to_dir(), &target)?;
        } else {
            let mut bytes = Vec::new();
            entry
                .to_file()
                .read_to_end(&mut bytes)
                .map_err(|e| format!("{name}: {e}"))?;
            std::fs::write(&target, bytes).map_err(|e| format!("{}: {e}", target.display()))?;
            count += 1;
        }
    }
    Ok(count)
}
