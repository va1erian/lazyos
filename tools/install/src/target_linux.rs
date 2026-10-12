//! Discovery on the running Linux: the ESP from the mount table, its disk's
//! GPT, the two LazyOS partitions by path or GPT name, and every fact the
//! refusals of decision 7 need.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use super::{content_of, partition_node, read_gpt, Found, GrubSite};
use crate::args::Options;
use crate::esp::MountedEsp;
use crate::linux::{self, DevNum, Mount};
use crate::plan::{self, Part, HOME_NAME, SYSTEM_NAME};
use crate::populate::Extent;

/// Where a distro mounts its ESP, in the order they are tried.
const ESP_POINTS: [&str; 3] = ["/boot/efi", "/efi", "/boot"];

/// Discover the layout on this machine.
pub fn on_linux(options: &Options, stick: &Path) -> Result<Found, String> {
    plan::check_firmware(linux::booted_uefi())?;
    if !linux::is_root() {
        return Err(
            "run as root (sudo): the installer writes partitions, the ESP and GRUB's configuration"
                .into(),
        );
    }
    let mounts = linux::mounts()?;
    let esp_mount = find_esp(&mounts, options.esp.as_deref())?;
    let esp_node = node_of(esp_mount.dev, &esp_mount.source);
    let esp_sys = linux::sys_part(esp_mount.dev)?;
    let disk_node = PathBuf::from(format!("/dev/{}", esp_sys.disk));
    let mut disk = File::open(&disk_node).map_err(|e| format!("{}: {e}", disk_node.display()))?;
    let disk_bytes =
        std::io::Seek::seek(&mut disk, std::io::SeekFrom::End(0)).map_err(|e| e.to_string())?;
    let table = read_gpt(&mut disk, disk_bytes)?;
    let swaps = linux::swaps();
    let describe = |node: PathBuf| -> Result<(Part, Extent), String> {
        let dev = linux::block_dev(&node)?;
        let sys = linux::sys_part(dev)?;
        let entry = table
            .entries
            .iter()
            .find(|e| e.index == sys.index && sys.disk == esp_sys.disk);
        let extent = Extent {
            path: node.clone(),
            start_lba: 0,
            sectors: sys.sectors,
        };
        let part = Part {
            device: node.display().to_string(),
            key: linux::key(dev),
            disk: sys.disk.clone(),
            index: sys.index,
            start_lba: sys.start,
            sectors: sys.sectors,
            type_guid: entry.map(|e| e.type_guid),
            name: entry.map(|e| e.name_chars().collect()).unwrap_or_default(),
            content: content_of(&extent),
            mounted_at: mounts
                .iter()
                .find(|m| m.dev == dev)
                .map(|m| m.point.clone()),
            busy: sys.held || swaps.contains(&dev),
        };
        Ok((part, extent))
    };
    let by_name = |given: Option<&Path>, name: &str| -> Result<PathBuf, String> {
        if let Some(path) = given {
            return Ok(path.to_path_buf());
        }
        let entry = table.by_name(name).ok_or_else(|| {
            format!(
                "no single partition named {name:?} on /dev/{}; pass --system/--home",
                esp_sys.disk
            )
        })?;
        Ok(partition_node(&esp_sys.disk, entry.index))
    };
    let (system, system_extent) = describe(by_name(options.system.as_deref(), SYSTEM_NAME)?)?;
    let (home, home_extent) = describe(by_name(options.home.as_deref(), HOME_NAME)?)?;
    let (esp, _) = describe(esp_node.clone())?;
    let mut sector = [0u8; 512];
    File::open(&esp_node)
        .and_then(|mut f| f.read_exact(&mut sector))
        .map_err(|e| format!("{}: {e}", esp_node.display()))?;
    let esp_serial = plan::fat_serial(&sector)
        .ok_or_else(|| format!("{} is not a FAT volume", esp_node.display()))?;
    let running = mounts
        .iter()
        .filter(|m| m.point == "/" || m.point == "/boot")
        .map(|m| linux::key(m.dev))
        .collect();
    let stick_disk = linux::dev_of(stick)
        .ok()
        .and_then(|dev| linux::sys_part(dev).ok())
        .map(|s| s.disk);
    Ok(Found {
        system,
        home,
        esp,
        system_extent,
        home_extent,
        esp_access: Box::new(MountedEsp {
            root: PathBuf::from(&esp_mount.point),
        }),
        image_esp: None,
        esp_serial,
        running,
        stick_disk,
        grub: GrubSite::Linux,
        facts: facts(&mounts, &esp_mount),
    })
}

/// The ESP's mount: `given`, else the first of [`ESP_POINTS`] that is a
/// mounted FAT volume.
fn find_esp(mounts: &[Mount], given: Option<&Path>) -> Result<Mount, String> {
    let candidates: Vec<String> = match given {
        Some(path) => vec![path.to_string_lossy().trim_end_matches('/').to_string()],
        None => ESP_POINTS.iter().map(|p| p.to_string()).collect(),
    };
    candidates
        .iter()
        .find_map(|point| {
            mounts
                .iter()
                .rev()
                .find(|m| &m.point == point && m.fstype == "vfat")
                .cloned()
        })
        .ok_or_else(|| {
            format!(
                "no FAT ESP mounted at {}; pass --esp",
                candidates.join(" or ")
            )
        })
}

/// The device node for `dev`: udev's `/dev/block/M:m`, else the mount source.
fn node_of(dev: DevNum, source: &str) -> PathBuf {
    let udev = PathBuf::from(format!("/dev/block/{}:{}", dev.0, dev.1));
    if udev.exists() {
        udev
    } else {
        PathBuf::from(source)
    }
}

/// Step 1's report: distro, GRUB version, `/boot`'s filesystem, Secure Boot.
fn facts(mounts: &[Mount], esp: &Mount) -> Vec<String> {
    let boot = mounts
        .iter()
        .rev()
        .find(|m| m.point == "/boot")
        .or_else(|| mounts.iter().rev().find(|m| m.point == "/"))
        .map_or("unknown".to_string(), |m| {
            format!("{} ({})", m.fstype, m.source)
        });
    let grub = ["grub-install", "grub2-install"]
        .into_iter()
        .find(|tool| linux::on_path(tool))
        .and_then(|tool| linux::first_line(tool, &["--version"]))
        .unwrap_or_else(|| "not found".into());
    let secure = linux::first_line("mokutil", &["--sb-state"])
        .unwrap_or_else(|| "unknown (no mokutil)".into());
    let mut out = vec![
        format!("distro: {}", linux::distro()),
        format!("GRUB: {grub}"),
        format!("/boot: {boot}"),
        format!("ESP: {} at {}", esp.source, esp.point),
        format!("Secure Boot: {secure}"),
    ];
    if secure.contains("enabled") {
        out.push(
            "warning: Secure Boot is on; the unsigned LazyOS loader will be refused (turn it off)"
                .into(),
        );
    }
    out
}
