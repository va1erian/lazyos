//! `install`, `update`, `status` and `uninstall` end to end in test mode
//! (`--image`), against a disk image and a stick built like the real ones:
//! what lands on each partition, what survives an update, what is refused.

use std::io::Cursor;
use std::path::Path;

use super::fixture::{self, Rig};
use crate::args::{self, Options};
use crate::esp::{Esp, ImageEsp};
use crate::os_layout::MANIFEST_PATH;
use crate::plan::{self, Content, ESP_DIR};
use crate::populate::{self, Extent};
use crate::target::{self, Found};
use crate::{os_image, run, usb_ramdisk, verify};

fn options(rig: &Rig, command: &str) -> Options {
    let line = format!(
        "{command} --image {} --stick {} --yes",
        rig.image.display(),
        rig.stick.display()
    );
    args::parse(line.split_whitespace().map(String::from)).unwrap()
}

fn found(rig: &Rig) -> Found {
    target::in_image(&rig.image, &options(rig, "status")).unwrap()
}

fn esp(found: &Found) -> ImageEsp {
    let at = found.image_esp.as_ref().unwrap();
    ImageEsp {
        path: at.path.clone(),
        start: at.start,
        len: at.len,
    }
}

fn volume(extent: &Extent) -> ext2fs::Ext2 {
    ext2fs::Ext2::open(Box::new(extent.io(false).unwrap()), os_image::now).unwrap()
}

fn uuid_of(content: &Content) -> [u8; 16] {
    match content {
        Content::Ext2 { uuid, .. } => *uuid,
        other => panic!("not ext2: {other:?}"),
    }
}

/// The `lazyos.cfg` inside the config ramdisk on the ESP.
fn installed_cfg(esp: &mut ImageEsp) -> String {
    let ramdisk = esp
        .read(&format!("{ESP_DIR}/ramdisk"))
        .unwrap()
        .expect("no ramdisk on the ESP");
    let start = (usb_ramdisk::FAT_START_LBA * 512) as usize;
    let end = start + (usb_ramdisk::FAT_SECTORS * 512) as usize;
    let cfg = usb_ramdisk::fat_read(
        Cursor::new(ramdisk[start..end].to_vec()),
        fhs::boot::LAZYOS_CFG,
    )
    .unwrap();
    String::from_utf8(cfg).unwrap()
}

fn assert_clean(found: &Found) {
    for extent in [&found.system_extent, &found.home_extent] {
        verify::check_volume(extent, u64::MAX).unwrap();
    }
}

#[test]
fn install_update_uninstall() {
    let rig = fixture::rig(
        "e2e",
        &fixture::files(&[("/system/bin/old", b"old")]),
        &fixture::kernel(1, 64 << 10),
    );
    run::run(&options(&rig, "install")).unwrap();

    let found = found(&rig);
    assert!(
        matches!(found.system.content, Content::Ext2 { system: true, ref label, .. } if label == "lazyos")
    );
    assert!(
        matches!(found.home.content, Content::Ext2 { system: false, ref label, .. } if label == "lazyhome")
    );
    let (system_uuid, home_uuid) = (uuid_of(&found.system.content), uuid_of(&found.home.content));
    assert_clean(&found);

    // The system is the stick's OS volume, manifest and seed included.
    let system = volume(&found.system_extent);
    let stick = crate::payload::Payload::open(&rig.stick)
        .unwrap()
        .os_volume()
        .unwrap();
    assert_eq!(
        system.read_file(MANIFEST_PATH).unwrap(),
        stick.read_file(MANIFEST_PATH).unwrap()
    );
    assert_eq!(
        system
            .read_file("/system/share/demo/data.bin")
            .unwrap()
            .len(),
        300_000
    );
    let db = system.lookup(fhs::state::ACCOUNTS_DB).unwrap();
    assert_eq!((db.uid, db.mode & 0o777), (accountdb::ACCOUNTS_UID, 0o600));
    assert_eq!(system.lookup("/home/user").unwrap().mode & 0o777, 0o700);
    // The home has a private directory per account.
    let home = volume(&found.home_extent);
    let user = home.lookup("/user").unwrap();
    assert_eq!((user.uid, user.mode & 0o777), (1000, 0o700));

    // The ESP files, the config naming both partitions, the GRUB entry.
    let mut esp = esp(&found);
    for name in plan::ESP_FILES {
        assert!(
            esp.read(&format!("{ESP_DIR}/{name}")).unwrap().is_some(),
            "{name} missing"
        );
    }
    let cfg = installed_cfg(&mut esp);
    assert!(
        cfg.contains(&format!("root=UUID={}\n", crate::format_uuid(system_uuid))),
        "{cfg}"
    );
    assert!(
        cfg.contains(&format!("home=UUID={}\n", crate::format_uuid(home_uuid))),
        "{cfg}"
    );
    let custom = String::from_utf8(esp.read("EFI/BOOT/custom.cfg").unwrap().unwrap()).unwrap();
    assert!(
        custom.contains(plan::CHAINLOAD) && custom.contains("--set=root 1A2B-3C4D"),
        "{custom}"
    );

    // A second install refuses: the system is LazyOS already.
    let again = run::run(&options(&rig, "install")).unwrap_err();
    assert!(again.contains("already holds LazyOS"), "{again}");

    // The running system's own state, then a new stick: a program gone, one
    // added, a new kernel.
    {
        let system = os_image::open_cached(found.system_extent.io(true).unwrap()).unwrap();
        system.mkdir_p("/conf", 0o700, 0, 0).unwrap();
        system
            .write_file("/conf/mine", b"kept", 0o600, 0, 0, 0)
            .unwrap();
        system
            .write_file("/home/user/note", b"mine", 0o600, 1000, 1000, 0)
            .unwrap();
        system.flush().unwrap();
    }
    let new_kernel = fixture::kernel(2, 80 << 10);
    fixture::write_stick(
        &rig.stick,
        &fixture::files(&[("/system/bin/new", b"new")]),
        &new_kernel,
    );
    run::run(&options(&rig, "update")).unwrap();

    let found = self::found(&rig);
    assert_eq!(
        uuid_of(&found.system.content),
        system_uuid,
        "an update keeps the UUID"
    );
    let system = volume(&found.system_extent);
    assert_eq!(system.read_file("/conf/mine").unwrap(), b"kept");
    assert_eq!(system.read_file("/home/user/note").unwrap(), b"mine");
    assert_eq!(system.read_file("/system/bin/new").unwrap(), b"new");
    assert!(
        system.lookup("/system/bin/old").is_err(),
        "a dropped program survived the update"
    );
    let mut esp = self::esp(&found);
    assert_eq!(
        esp.read(&format!("{ESP_DIR}/kernel-x86_64"))
            .unwrap()
            .unwrap(),
        new_kernel
    );
    assert!(installed_cfg(&mut esp).contains(&crate::format_uuid(home_uuid)));
    assert_clean(&found);

    run::run(&options(&rig, "status")).unwrap();
    run::run(&options(&rig, "uninstall")).unwrap();
    let found = self::found(&rig);
    let mut esp = self::esp(&found);
    assert!(esp
        .read(&format!("{ESP_DIR}/kernel-x86_64"))
        .unwrap()
        .is_none());
    let custom = String::from_utf8(esp.read("EFI/BOOT/custom.cfg").unwrap().unwrap()).unwrap();
    assert!(!custom.contains(plan::BEGIN), "{custom}");
    // The partitions are the person's to reuse: left as they were.
    assert_eq!(uuid_of(&found.system.content), system_uuid);
}

#[test]
fn refusals_write_nothing() {
    let rig = fixture::rig("refuse", &fixture::files(&[]), &fixture::kernel(1, 4096));
    let before = std::fs::read(&rig.image).unwrap();
    let cases = [
        ("--system 1", "ESP"),
        ("--home 3", "different partitions"),
        ("--dry-run", ""),
    ];
    for (extra, want) in cases {
        let line = format!(
            "--image {} --stick {} --yes {extra}",
            rig.image.display(),
            rig.stick.display()
        );
        let result = run::run(&args::parse(line.split_whitespace().map(String::from)).unwrap());
        match want {
            "" => result.unwrap(),
            want => assert!(result.unwrap_err().contains(want), "{extra}"),
        }
    }
    assert!(
        std::fs::read(&rig.image).unwrap() == before,
        "a refused or dry run wrote to the disk"
    );
}

#[test]
fn keeps_an_existing_home_and_copies_one() {
    let rig = fixture::rig("home", &fixture::files(&[]), &fixture::kernel(1, 4096));
    let found = found(&rig);
    // A previous LazyOS home with a file in it.
    let uuid = [0x77; 16];
    populate::install_home(&found.home_extent, uuid, &[], None).unwrap();
    {
        let home = os_image::open_cached(found.home_extent.io(true).unwrap()).unwrap();
        home.mkdir_p("/user", 0o700, 1000, 1000).unwrap();
        home.write_file("/user/kept.txt", b"hello", 0o600, 1000, 1000, 0)
            .unwrap();
        home.flush().unwrap();
    }
    run::run(&options(&rig, "install")).unwrap();
    let found = self::found(&rig);
    assert_eq!(uuid_of(&found.home.content), uuid);
    assert_eq!(
        volume(&found.home_extent)
            .read_file("/user/kept.txt")
            .unwrap(),
        b"hello"
    );
    let cfg = installed_cfg(&mut esp(&found));
    assert!(cfg.contains(&crate::format_uuid(uuid)), "{cfg}");

    // --home-from: a fresh home with another volume's files in it.
    let other = rig.dir.join("stickhome.img");
    std::fs::write(&other, vec![0u8; 16 << 20]).unwrap();
    let extent = Extent {
        path: other.clone(),
        start_lba: 0,
        sectors: (16 << 20) / 512,
    };
    populate::install_home(&extent, [0x55; 16], &[], None).unwrap();
    {
        let v = os_image::open_cached(extent.io(true).unwrap()).unwrap();
        v.mkdir_p("/user/docs", 0o700, 1000, 1000).unwrap();
        v.write_file("/user/docs/a.txt", b"from the stick", 0o644, 1000, 1000, 0)
            .unwrap();
        v.flush().unwrap();
    }
    let into = found.home_extent.clone();
    let accounts = crate::os_layout::parse_passwd(fixture::PASSWD);
    populate::install_home(&into, [0x66; 16], &accounts, Some(Path::new(&other))).unwrap();
    let home = volume(&into);
    assert_eq!(
        home.read_file("/user/docs/a.txt").unwrap(),
        b"from the stick"
    );
    assert_eq!(home.lookup("/admin").unwrap().uid, 1001);
    assert!(
        home.lookup("/user/kept.txt").is_err(),
        "the old home was not reformatted"
    );
}
