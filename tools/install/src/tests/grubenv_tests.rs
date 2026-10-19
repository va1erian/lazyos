//! The U2a spike's pieces: the env block, the snippet, `--test-grubenv`.

use super::fixture;
use crate::args;
use crate::esp::Esp;
use crate::grubenv;
use crate::plan;
use crate::{os_image, run};

#[test]
fn the_block_is_one_padded_grub_env_block() {
    let block = grubenv::block("4").unwrap();
    assert_eq!(block.len(), 1024);
    assert!(block.starts_with(b"# GRUB Environment Block\nlazyos_boots=4\n"));
    assert!(block[40..].iter().all(|&b| b == b'#'));
    assert!(grubenv::block("a\nb").is_err());
    assert!(grubenv::block("#").is_err());
    assert!(grubenv::block(&"9".repeat(1100)).is_err());
}

#[test]
fn the_snippet_names_the_volume_and_leaves_no_placeholder() {
    let text = grubenv::snippet([0x11; 16]);
    assert!(text.contains("--fs-uuid --set=lzsys 11111111-1111-1111-1111-111111111111"));
    assert!(!text.contains('@'));
    assert!(text.contains("save_env -f ($lzsys)/releases/grubenv lazyos_boots"));
}

#[test]
fn the_option_is_test_mode_only() {
    let parse = |line: &str| args::parse(line.split_whitespace().map(String::from));
    assert!(parse("install --image d.img --test-grubenv 4").is_ok());
    assert!(parse("install --test-grubenv 4").is_err());
    assert!(parse("update --image d.img --test-grubenv 4").is_err());
}

#[test]
fn install_creates_the_block_in_place_and_the_snippet() {
    let rig = fixture::rig(
        "grubenv",
        &fixture::files(&[("/system/bin/old", b"old")]),
        &fixture::kernel(1, 64 << 10),
    );
    let line = format!(
        "install --image {} --stick {} --yes --test-grubenv 4",
        rig.image.display(),
        rig.stick.display()
    );
    let options = args::parse(line.split_whitespace().map(String::from)).unwrap();
    run::run(&options).unwrap();

    let mut found = crate::target::in_image(&rig.image, &options).unwrap();
    let bytes = grubenv::read(&found.system_extent).unwrap();
    assert_eq!(bytes, grubenv::block("4").unwrap());
    let volume = os_image::open_cached(found.system_extent.io(false).unwrap()).unwrap();
    let uuid = os_image::format_uuid(volume.uuid());
    let custom = String::from_utf8(
        found
            .image_esp
            .as_mut()
            .unwrap()
            .read("EFI/BOOT/custom.cfg")
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert!(custom.contains(&format!("--set=lzsys {uuid}")));
    assert!(custom.find("save_env").unwrap() < custom.find("menuentry").unwrap());
    assert!(custom.contains(plan::CHAINLOAD));

    // A test overwrites the counter in place: same block, same file.
    let before = volume.mapped_block(fhs::boot::GRUBENV_PATH, 0).unwrap();
    grubenv::overwrite(&found.system_extent, &grubenv::block("x").unwrap()).unwrap();
    assert_eq!(
        grubenv::read(&found.system_extent).unwrap(),
        grubenv::block("x").unwrap()
    );
    let volume = os_image::open_cached(found.system_extent.io(false).unwrap()).unwrap();
    assert_eq!(
        volume.mapped_block(fhs::boot::GRUBENV_PATH, 0).unwrap(),
        before
    );
    assert!(grubenv::overwrite(&found.system_extent, b"short").is_err());
    crate::verify::check_volume(&found.system_extent, u64::MAX).unwrap();
}

#[test]
fn a_normal_install_writes_neither() {
    let rig = fixture::rig(
        "nogrubenv",
        &fixture::files(&[]),
        &fixture::kernel(1, 64 << 10),
    );
    let line = format!(
        "install --image {} --stick {} --yes",
        rig.image.display(),
        rig.stick.display()
    );
    let options = args::parse(line.split_whitespace().map(String::from)).unwrap();
    run::run(&options).unwrap();
    let mut found = crate::target::in_image(&rig.image, &options).unwrap();
    assert!(grubenv::read(&found.system_extent).is_err());
    let custom = found
        .image_esp
        .as_mut()
        .unwrap()
        .read("EFI/BOOT/custom.cfg")
        .unwrap()
        .unwrap();
    assert!(!String::from_utf8(custom).unwrap().contains("lzsys"));
}
