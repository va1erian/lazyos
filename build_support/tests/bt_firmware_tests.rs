//! `LAZYOS_BT=1` firmware (docs/bluetooth-plan.md B7): the files `btd` loads
//! are placed under `fhs::FIRMWARE` exactly like the Wi-Fi firmware, from the
//! same pins and cache, and only for the `bt` switch.

use std::path::{Path, PathBuf};

use crate::os_image::OsFiles;
use crate::wifi_embed::{parse_manifest, place_firmware, selected, BT_SWITCH, MANIFEST};

const BLOB: &[u8] = b"\x01\x02bluetooth firmware";
const LICENCE: &[u8] = b"Redistribution in binary form is permitted.\n";

fn hex(bytes: &[u8]) -> String {
    lazyos_crypto::sha256::sha256(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn line(name: &str, switch: &str, bytes: &[u8], licence: &str) -> String {
    format!(
        "{name} | {switch} | {} | {} | {licence} | https://example.test/{name}\n",
        hex(bytes),
        bytes.len()
    )
}

/// A scratch repository root: a manifest, and a firmware cache holding the
/// files in `cached`.
fn tree(label: &str, cached: &[(&str, &[u8])]) -> PathBuf {
    let root = std::env::temp_dir().join(format!("lazyos-bt-fw-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let manifest = format!(
        "{}{}{}",
        line("intel/ibt-1.sfi", "bt", BLOB, "intel/LICENCE.ibt"),
        line("intel/LICENCE.ibt", "bt", LICENCE, "intel/LICENCE.ibt"),
        line("rtw88/fw.bin", "wifi", BLOB, "intel/LICENCE.ibt"),
    );
    std::fs::create_dir_all(root.join("tools/firmware")).unwrap();
    std::fs::write(root.join(MANIFEST), manifest).unwrap();
    for (name, bytes) in cached {
        let path = root.join("target/firmware").join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    root
}

fn placed(root: &Path, switch: &str) -> Vec<String> {
    let mut sink = OsFiles::default();
    place_firmware(&mut sink, root, switch, "LAZYOS_BT");
    sink.files().into_iter().map(|file| file.path).collect()
}

#[test]
fn the_bt_firmware_lands_under_the_firmware_tree_with_its_licence() {
    let root = tree(
        "ok",
        &[("intel/ibt-1.sfi", BLOB), ("intel/LICENCE.ibt", LICENCE)],
    );
    let paths = placed(&root, BT_SWITCH);
    let want = |name: &str| format!("{}/{name}", fhs::FIRMWARE);
    assert_eq!(paths.len(), 2, "{paths:?}");
    assert!(paths.contains(&want("intel/ibt-1.sfi")), "{paths:?}");
    assert!(paths.contains(&want("intel/LICENCE.ibt")), "{paths:?}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn another_switchs_firmware_stays_out() {
    let root = tree(
        "wifi",
        &[("intel/ibt-1.sfi", BLOB), ("intel/LICENCE.ibt", LICENCE)],
    );
    let text = std::fs::read_to_string(root.join(MANIFEST)).unwrap();
    let pins = parse_manifest(&text).unwrap();
    let bt: Vec<&str> = selected(&pins, BT_SWITCH)
        .iter()
        .map(|pin| pin.name.as_str())
        .collect();
    assert!(!bt.contains(&"rtw88/fw.bin"), "{bt:?}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_missing_or_altered_file_fails_the_build_with_the_fetch_command() {
    let missing = tree("missing", &[("intel/LICENCE.ibt", LICENCE)]);
    let altered = tree(
        "altered",
        &[
            ("intel/ibt-1.sfi", b"not it"),
            ("intel/LICENCE.ibt", LICENCE),
        ],
    );
    for (root, why) in [(&missing, "is missing"), (&altered, "bytes")] {
        let failure = std::panic::catch_unwind(|| placed(root, BT_SWITCH))
            .expect_err("the build must not go on without the pinned bytes");
        let text = failure
            .downcast_ref::<String>()
            .cloned()
            .unwrap_or_default();
        assert!(text.contains("LAZYOS_BT=1"), "{text}");
        assert!(text.contains("tools/firmware/fetch.py"), "{text}");
        assert!(text.contains(why), "{text}");
    }
    let _ = std::fs::remove_dir_all(missing);
    let _ = std::fs::remove_dir_all(altered);
}

#[test]
fn the_checked_in_manifest_pins_what_the_loaders_ask_for() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let text = std::fs::read_to_string(root.join(MANIFEST)).unwrap();
    let pins = parse_manifest(&text).unwrap();
    let bt: Vec<&str> = selected(&pins, BT_SWITCH)
        .iter()
        .map(|pin| pin.name.as_str())
        .collect();
    // The boxes in docs/usb-hotplug-plan.md: an AX201 (legacy bootloader, the
    // file named by the version record), AX210/AX211 (named by the CNVi/CNVr
    // words), the AMD NUC's RTL8822CE and an RTL8761B dongle.
    for name in [
        "intel/ibt-19-0-4.sfi",
        "intel/ibt-19-0-4.ddc",
        "intel/ibt-0041-0041.sfi",
        "intel/ibt-0040-0041.sfi",
        "intel/ibt-0040-0041.ddc",
        "rtl_bt/rtl8822cu_fw.bin",
        "rtl_bt/rtl8761bu_fw.bin",
        "rtl_bt/rtl8761bu_config.bin",
        "intel/LICENCE.ibt_firmware",
        "rtl_bt/LICENCE.rtlwifi_firmware.txt",
    ] {
        assert!(bt.contains(&name), "{name} is not pinned for bt: {bt:?}");
    }
    assert!(
        !bt.iter().any(|name| name.starts_with("rtw88/")),
        "the Wi-Fi firmware is not a Bluetooth one"
    );
}
