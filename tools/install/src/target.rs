//! Finding the three partitions (steps 1 and 2): on the running Linux by
//! device path or GPT name, or in a whole-disk image file (test mode). Either
//! way the result is the same [`Found`], so everything after discovery is
//! one code path.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use gpt::guid;

use crate::args::Options;
use crate::esp::{Esp, ImageEsp};
use crate::plan::{self, Content, Part, HOME_NAME, SYSTEM_NAME};
use crate::populate::{self, Extent};

/// Where GRUB's configuration is written.
pub enum GrubSite {
    /// The running Linux, rooted at `/`.
    Linux,
    /// Test mode: `custom.cfg` in this directory of the image's ESP.
    Image { dir: String },
}

/// Everything discovery learned.
pub struct Found {
    pub system: Part,
    pub home: Part,
    pub esp: Part,
    pub system_extent: Extent,
    pub home_extent: Extent,
    pub esp_access: Box<dyn Esp>,
    /// Test mode's ESP, for `custom.cfg`.
    pub image_esp: Option<ImageEsp>,
    /// The ESP's FAT serial (`search --fs-uuid`).
    pub esp_serial: String,
    /// Keys of the running Linux's `/` and `/boot`, and the stick's disk.
    pub running: Vec<u64>,
    pub stick_disk: Option<String>,
    pub grub: GrubSite,
    /// What step 1 reports (distro, GRUB, Secure Boot, ...).
    pub facts: Vec<String>,
}

struct FileReader<'a>(&'a mut File, u64);

impl gpt::Reader for FileReader<'_> {
    fn read(&mut self, lba: u64, buf: &mut [u8]) -> bool {
        self.0.seek(SeekFrom::Start((self.1 + lba) * 512)).is_ok() && self.0.read_exact(buf).is_ok()
    }
}

/// The GPT of the disk in `file` of `bytes`.
pub fn read_gpt(file: &mut File, bytes: u64) -> Result<gpt::Table, String> {
    let mut mbr = [0u8; 512];
    file.seek(SeekFrom::Start(0))
        .and_then(|_| file.read_exact(&mut mbr))
        .map_err(|e| e.to_string())?;
    if !gpt::is_protective(&mbr) {
        return Err("the disk has no GPT (a UEFI Linux install has one)".into());
    }
    gpt::read(&mut FileReader(file, 0), bytes / 512).map_err(|f| {
        format!(
            "the disk's GPT is unreadable (primary {:?}, backup {:?})",
            f.primary, f.backup
        )
    })
}

/// The first 2 KiB of `extent`, sniffed, with the ext2 details filled in.
pub fn content_of(extent: &Extent) -> Content {
    let mut head = vec![0u8; 2048];
    let read = File::open(&extent.path).and_then(|mut file| {
        file.seek(SeekFrom::Start(extent.start_lba * 512))?;
        file.read_exact(&mut head)
    });
    if read.is_err() {
        return Content::Unknown;
    }
    match plan::sniff(&head) {
        Content::Ext2 { .. } => match populate::identify(extent) {
            Some((uuid, label, system)) => Content::Ext2 {
                uuid,
                label,
                system,
            },
            None => Content::Unknown,
        },
        other => other,
    }
}

/// Pick a role's GPT entry: `given` (an index in test mode) or the entry
/// named `name`.
fn pick<'a>(
    table: &'a gpt::Table,
    given: Option<&Path>,
    name: &str,
) -> Result<&'a gpt::Entry, String> {
    match given {
        Some(text) => {
            let text = text.to_string_lossy();
            let index: u32 = text
                .parse()
                .map_err(|_| format!("{text:?}: in test mode a partition is its GPT index"))?;
            table
                .entries
                .iter()
                .find(|e| e.index == index)
                .ok_or_else(|| format!("no partition {index}"))
        }
        None => table.by_name(name).ok_or_else(|| {
            format!("no single partition named {name:?}; name it or pass its device")
        }),
    }
}

fn image_part(image: &Path, entry: &gpt::Entry) -> (Part, Extent) {
    let extent = Extent {
        path: image.to_path_buf(),
        start_lba: entry.first_lba,
        sectors: entry.sectors(),
    };
    let part = Part {
        device: format!("{}#{}", image.display(), entry.index),
        key: u64::from(entry.index),
        disk: image.display().to_string(),
        index: entry.index,
        start_lba: entry.first_lba,
        sectors: entry.sectors(),
        type_guid: Some(entry.type_guid),
        name: entry.name_chars().collect(),
        content: content_of(&extent),
        mounted_at: None,
        busy: false,
    };
    (part, extent)
}

/// Test mode: the partitions of a whole-disk image file.
pub fn in_image(image: &Path, options: &Options) -> Result<Found, String> {
    let mut file = File::open(image).map_err(|e| format!("{}: {e}", image.display()))?;
    let bytes = file.metadata().map_err(|e| e.to_string())?.len();
    let table = read_gpt(&mut file, bytes)?;
    let esps: Vec<&gpt::Entry> = table
        .entries
        .iter()
        .filter(|e| e.type_guid == guid::ESP)
        .collect();
    let [esp_entry] = esps[..] else {
        return Err(format!(
            "{} has {} EFI System Partitions; expected one",
            image.display(),
            esps.len()
        ));
    };
    let (system, system_extent) =
        image_part(image, pick(&table, options.system.as_deref(), SYSTEM_NAME)?);
    let (home, home_extent) = image_part(image, pick(&table, options.home.as_deref(), HOME_NAME)?);
    let (esp, esp_extent) = image_part(image, esp_entry);
    let image_esp = ImageEsp {
        path: image.to_path_buf(),
        start: esp_extent.start_lba * 512,
        len: esp_extent.bytes(),
    };
    let esp_serial = image_esp.serial()?;
    let access = ImageEsp {
        path: image.to_path_buf(),
        start: image_esp.start,
        len: image_esp.len,
    };
    Ok(Found {
        system,
        home,
        esp,
        system_extent,
        home_extent,
        esp_access: Box::new(access),
        image_esp: Some(image_esp),
        esp_serial,
        running: Vec::new(),
        stick_disk: None,
        grub: GrubSite::Image {
            dir: options.grub_dir.clone(),
        },
        facts: vec![format!(
            "test mode: {} ({} MiB, GPT from {:?})",
            image.display(),
            bytes >> 20,
            table.source
        )],
    })
}

/// The device node of partition `index` of kernel disk `disk`.
pub fn partition_node(disk: &str, index: u32) -> PathBuf {
    let sep = if disk.ends_with(|c: char| c.is_ascii_digit()) {
        "p"
    } else {
        ""
    };
    PathBuf::from(format!("/dev/{disk}{sep}{index}"))
}

#[cfg(unix)]
pub use self::linux_target::on_linux;

#[cfg(unix)]
#[path = "target_linux.rs"]
mod linux_target;

#[cfg(not(unix))]
pub fn on_linux(_: &Options, _: &Path) -> Result<Found, String> {
    Err("installing to block devices needs Linux; use --image FILE for test mode".into())
}
