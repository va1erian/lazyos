//! The installer's decisions, pure so they are host-tested with no root and
//! no disk: which partitions may be written (decision 7), the generated
//! `lazyos.cfg` (decision 4), the GRUB entry (decision 3), and which GRUB
//! tool regenerates `grub.cfg` (step 7).

use gpt::guid::{self, Guid};

use crate::args::Command;

/// The ESP directory the loader and its files go to.
pub const ESP_DIR: &str = "EFI/lazyos";
/// The loader's name there (what the GRUB entry chainloads).
pub const LOADER: &str = "BOOTX64.EFI";
/// The ESP files, in the order an update writes them: the kernel last
/// ("Updates and recovery"). `boot.json` is the loader's own configuration
/// (the stick's framebuffer minimum); the loader reads all but itself from
/// its own directory (K4).
pub const ESP_FILES: [&str; 4] = ["ramdisk", "boot.json", LOADER, "kernel-x86_64"];
/// GPT names the partitions are found by when not given.
pub const SYSTEM_NAME: &str = "lazyos-system";
pub const HOME_NAME: &str = "lazyos-home";
/// The recommended floor for the desktop profile.
pub const MIN_SYSTEM_BYTES: u64 = 1 << 30;

/// What a partition holds today, read from its first blocks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Content {
    /// Nothing recognised (an empty partition).
    Unknown,
    Fat,
    /// An ext2/3/4 superblock with features beyond what LazyOS writes.
    OtherExt(&'static str),
    Ext2 {
        uuid: [u8; 16],
        label: String,
        /// It carries `/system/.image-manifest`: a LazyOS system volume.
        system: bool,
    },
}

/// One partition as the installer sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    /// How to name it to the person (`/dev/sda5`, `disk.img#5`).
    pub device: String,
    /// Identity for "distinct" checks: the device number, or the index in
    /// an image.
    pub key: u64,
    /// The whole disk it belongs to.
    pub disk: String,
    pub index: u32,
    pub start_lba: u64,
    pub sectors: u64,
    pub type_guid: Option<Guid>,
    pub name: String,
    pub content: Content,
    pub mounted_at: Option<String>,
    /// Swap, or held by device-mapper/MD/LVM.
    pub busy: bool,
}

impl Part {
    pub fn bytes(&self) -> u64 {
        self.sectors * 512
    }

    fn is_esp_like(&self) -> bool {
        self.type_guid == Some(guid::ESP) || self.content == Content::Fat
    }
}

/// The facts about the running system the checks need.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Context {
    /// Keys of the partitions Linux has at `/` and `/boot`.
    pub running: Vec<u64>,
    /// The disk the stick is on (never a target).
    pub stick_disk: Option<String>,
    /// The payload's OS volume size: the system partition's floor.
    pub payload_os_bytes: u64,
    pub esp_free: u64,
    /// Bytes the ESP files need beyond what is there.
    pub esp_need: u64,
}

/// Decision 6: a machine that did not boot through UEFI is refused.
pub fn check_firmware(uefi: bool) -> Result<(), String> {
    match uefi {
        true => Ok(()),
        false => Err(
            "this machine did not boot through UEFI; v1 supports UEFI GRUB only (decision 6)"
                .into(),
        ),
    }
}

/// Check the three roles for `command`; the warnings to print, or why
/// nothing may be written. Nothing is formatted before this passes.
pub fn validate(
    command: Command,
    system: &Part,
    home: &Part,
    esp: &Part,
    ctx: &Context,
    format_home: bool,
) -> Result<Vec<String>, String> {
    let mut warnings = Vec::new();
    if system.key == home.key || system.key == esp.key || home.key == esp.key {
        return Err("the system, home and ESP must be three different partitions".into());
    }
    if esp.type_guid != Some(guid::ESP) {
        return Err(format!("{} is not an EFI System Partition", esp.device));
    }
    for (role, part) in [("system", system), ("home", home)] {
        check_target(role, part, esp, ctx)?;
    }
    match command {
        Command::Install => {
            if system.bytes() < ctx.payload_os_bytes {
                return Err(format!(
                    "{} is {} MiB; the system needs at least {} MiB",
                    system.device,
                    system.bytes() >> 20,
                    ctx.payload_os_bytes.div_ceil(1 << 20)
                ));
            }
            if system.bytes() < MIN_SYSTEM_BYTES {
                warnings.push(format!(
                    "{} is under 1 GiB: enough for this payload, tight for updates and apps",
                    system.device
                ));
            }
            match &system.content {
                Content::Ext2 { system: true, .. } => {
                    return Err(format!(
                        "{} already holds LazyOS; run `lazyos-install update` to keep its data",
                        system.device
                    ))
                }
                Content::Unknown => {}
                other => warnings.push(format!(
                    "{} holds {}, which will be erased",
                    system.device,
                    describe(other)
                )),
            }
            match (&home.content, format_home) {
                (Content::Ext2 { system: false, .. }, false) if is_lazyhome(&home.content) => {
                    warnings.push(format!("{} holds a LazyOS home; keeping it", home.device))
                }
                (Content::Unknown, _) => {}
                (other, _) => warnings.push(format!(
                    "{} holds {}, which will be erased",
                    home.device,
                    describe(other)
                )),
            }
        }
        Command::Update => {
            if !matches!(system.content, Content::Ext2 { system: true, .. }) {
                return Err(format!(
                    "{} holds no LazyOS system to update; run install",
                    system.device
                ));
            }
            if !matches!(home.content, Content::Ext2 { .. }) {
                return Err(format!("{} holds no ext2 home volume", home.device));
            }
        }
        Command::Uninstall | Command::Status => {}
    }
    if ctx.esp_free < ctx.esp_need {
        return Err(format!(
            "the ESP has {} MiB free; the LazyOS files need {} MiB more (nothing is evicted)",
            ctx.esp_free >> 20,
            ctx.esp_need.div_ceil(1 << 20)
        ));
    }
    Ok(warnings)
}

fn check_target(role: &str, part: &Part, esp: &Part, ctx: &Context) -> Result<(), String> {
    let device = &part.device;
    if part.is_esp_like() {
        return Err(format!(
            "{device} ({role}) is an ESP or a FAT volume; refusing"
        ));
    }
    if part.disk != esp.disk {
        return Err(format!(
            "{device} ({role}) is not on the ESP's disk {}",
            esp.disk
        ));
    }
    if let Some(point) = &part.mounted_at {
        return Err(format!(
            "{device} ({role}) is mounted at {point}; unmount it first"
        ));
    }
    if part.busy {
        return Err(format!(
            "{device} ({role}) is in use (swap, LVM, RAID or device-mapper)"
        ));
    }
    if ctx.running.contains(&part.key) {
        return Err(format!(
            "{device} ({role}) is the running Linux's / or /boot"
        ));
    }
    if ctx.stick_disk.as_deref() == Some(part.disk.as_str()) {
        return Err(format!("{device} ({role}) is on the stick itself"));
    }
    Ok(())
}

fn is_lazyhome(content: &Content) -> bool {
    matches!(content, Content::Ext2 { label, .. } if label == crate::usb_stick::HOME_LABEL)
}

/// A filesystem in a few words, for the plan.
pub fn describe(content: &Content) -> String {
    match content {
        Content::Unknown => "nothing recognised".into(),
        Content::Fat => "a FAT filesystem".into(),
        Content::OtherExt(kind) => format!("an {kind} filesystem"),
        Content::Ext2 { system: true, .. } => "a LazyOS system".into(),
        Content::Ext2 { label, .. } if label.is_empty() => "an ext2 filesystem".into(),
        Content::Ext2 { label, .. } => format!("an ext2 filesystem labelled {label:?}"),
    }
}

/// What the first 2 KiB of a partition hold. `head` shorter than 2 KiB is
/// `Unknown`. An ext2 volume's `system` flag is filled in by the caller,
/// which can open it.
pub fn sniff(head: &[u8]) -> Content {
    if head.len() < 2048 {
        return Content::Unknown;
    }
    let sb = &head[1024..2048];
    if sb[56..58] == [0x53, 0xEF] {
        let incompat = u32::from_le_bytes([sb[96], sb[97], sb[98], sb[99]]);
        let compat = u32::from_le_bytes([sb[92], sb[93], sb[94], sb[95]]);
        // extents, 64bit, flex_bg, ... are ext4; a journal is ext3.
        if incompat & !0x2 != 0 {
            return Content::OtherExt("ext4");
        }
        if compat & 0x4 != 0 {
            return Content::OtherExt("ext3");
        }
        let mut uuid = [0u8; 16];
        uuid.copy_from_slice(&sb[104..120]);
        let label = sb[120..136]
            .iter()
            .take_while(|b| **b != 0)
            .map(|&b| b as char)
            .collect();
        return Content::Ext2 {
            uuid,
            label,
            system: false,
        };
    }
    if fat_serial(head[..512].try_into().unwrap_or(&[0; 512])).is_some() {
        return Content::Fat;
    }
    Content::Unknown
}

/// The serial GRUB's `search --fs-uuid` knows a FAT volume by (`ABCD-1234`),
/// from its boot sector; `None` when it is not one.
pub fn fat_serial(sector: &[u8; 512]) -> Option<String> {
    if !matches!(sector[0], 0xEB | 0xE9) || sector[510..] != [0x55, 0xAA] {
        return None;
    }
    let at = if &sector[82..87] == b"FAT32" {
        67
    } else if &sector[54..57] == b"FAT" {
        39
    } else {
        return None;
    };
    let id = u32::from_le_bytes([sector[at], sector[at + 1], sector[at + 2], sector[at + 3]]);
    Some(format!("{:04X}-{:04X}", id >> 16, id & 0xFFFF))
}

/// `lazyos.cfg` for the installed system: the stick's lines (limits,
/// display, trace, dbgd) with `root=` and `home=` naming the two partitions
/// by UUID.
pub fn lazyos_cfg(stick_cfg: &str, system: [u8; 16], home: [u8; 16]) -> String {
    let mut out = format!(
        "# Written by lazyos-install (docs/grub-install-plan.md).\nroot=UUID={}\nhome=UUID={}\n",
        crate::format_uuid(system),
        crate::format_uuid(home)
    );
    for line in stick_cfg.lines() {
        let key = line.trim_start();
        if key.starts_with("root=") || key.starts_with("home=") || key.starts_with("# Generated by")
        {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// The GRUB script template shipped on the stick as `install/40_lazyos.in`.
pub const TEMPLATE: &str = include_str!("../40_lazyos.in");
/// Markers around the entry when it goes into `custom.cfg`.
pub const BEGIN: &str = "### BEGIN lazyos-install ###";
pub const END: &str = "### END lazyos-install ###";
/// What `grub.cfg` must contain once the entry is in.
pub const CHAINLOAD: &str = "chainloader /EFI/lazyos/BOOTX64.EFI";

/// `/etc/grub.d/40_lazyos` for the ESP serial `uuid` (checked to be one, so
/// nothing else can reach the script).
pub fn grub_script(uuid: &str) -> Result<String, String> {
    let ok = uuid.len() == 9
        && uuid.char_indices().all(|(i, c)| {
            if i == 4 {
                c == '-'
            } else {
                c.is_ascii_hexdigit()
            }
        });
    if !ok {
        return Err(format!("{uuid:?} is not a FAT serial"));
    }
    Ok(TEMPLATE.replace("@ESP_UUID@", uuid))
}

/// The entry alone (the script minus its two shell lines), as `custom.cfg`
/// takes it.
pub fn menu_entry(uuid: &str) -> Result<String, String> {
    let script = grub_script(uuid)?;
    Ok(script
        .lines()
        .skip(2)
        .map(|line| format!("{line}\n"))
        .collect())
}

/// `text` (a `custom.cfg`) with the marked entry replaced by `entry`, or
/// appended.
pub fn with_block(text: &str, entry: &str) -> String {
    let (mut out, _) = without_block(text);
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&format!("{BEGIN}\n{entry}{END}\n"));
    out
}

/// `text` with the marked entry removed, and whether there was one.
pub fn without_block(text: &str) -> (String, bool) {
    let mut out = String::new();
    let mut inside = false;
    let mut found = false;
    for line in text.lines() {
        match line.trim() {
            BEGIN => {
                inside = true;
                found = true;
            }
            END if inside => inside = false,
            _ if !inside => {
                out.push_str(line);
                out.push('\n');
            }
            _ => {}
        }
    }
    (out, found)
}

/// The command that regenerates `grub.cfg` at `cfg`, given which programs
/// are on `PATH`: `update-grub` (Debian, Ubuntu), else `grub-mkconfig` or
/// `grub2-mkconfig` (Arch, Fedora) with `-o`.
pub fn regenerate_command(on_path: impl Fn(&str) -> bool, cfg: &str) -> Option<Vec<String>> {
    if on_path("update-grub") {
        return Some(vec!["update-grub".into()]);
    }
    ["grub-mkconfig", "grub2-mkconfig"]
        .into_iter()
        .find(|tool| on_path(tool))
        .map(|tool| vec![tool.into(), "-o".into(), cfg.into()])
}

/// `grub-reboot` or `grub2-reboot`.
pub fn reboot_command(on_path: impl Fn(&str) -> bool) -> Option<&'static str> {
    ["grub-reboot", "grub2-reboot"]
        .into_iter()
        .find(|tool| on_path(tool))
}
