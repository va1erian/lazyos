//! `LAZYOS_WIFI=1` (docs/wifi-rtw88-plan.md R0): refused without the stack
//! or without `devd`, and the firmware placed only when it is exactly the
//! pinned bytes, with its licence.

use std::path::Path;

use crate::wifi_embed::{check_bytes, firmware_files, parse_manifest, requirements, selected};

const BLOB: &[u8] = b"\x22\x88firmware";
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

fn manifest() -> String {
    format!(
        "# pins\n{}{}{}",
        line("acme/fw.bin", "wifi", BLOB, "acme/LICENCE.acme"),
        line("acme/LICENCE.acme", "other", LICENCE, "acme/LICENCE.acme"),
        line("other/x.bin", "sound", BLOB, "acme/LICENCE.acme"),
    )
}

#[test]
fn the_switch_needs_the_stack_and_devd() {
    assert_eq!(requirements(false, false, true), Ok(()));
    assert_eq!(requirements(true, true, false), Ok(()));
    let netd = requirements(true, false, false).unwrap_err();
    assert!(
        netd.contains("LAZYOS_NETD=1") && netd.contains("--wifi"),
        "{netd}"
    );
    let devd = requirements(true, true, true).unwrap_err();
    assert!(devd.contains("LAZYOS_DEVD=0"), "{devd}");
}

#[test]
fn the_checked_in_manifest_pins_the_rtl8822c_firmware_with_its_licence() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let text = std::fs::read_to_string(root.join(crate::wifi_embed::MANIFEST)).unwrap();
    let pins = parse_manifest(&text).unwrap();
    let wifi: Vec<&str> = selected(&pins, "wifi")
        .iter()
        .map(|pin| pin.name.as_str())
        .collect();
    assert!(wifi.contains(&"rtw88/rtw8822c_fw.bin"), "{wifi:?}");
    assert!(
        wifi.contains(&"rtw88/LICENCE.rtlwifi_firmware.txt"),
        "{wifi:?}"
    );
}

#[test]
fn a_switch_brings_the_licences_it_names() {
    let pins = parse_manifest(&manifest()).unwrap();
    let names: Vec<&str> = selected(&pins, "wifi")
        .iter()
        .map(|pin| pin.name.as_str())
        .collect();
    assert_eq!(names, ["acme/fw.bin", "acme/LICENCE.acme"]);
    assert!(selected(&pins, "none").is_empty());
}

#[test]
fn bad_manifests_are_refused() {
    let good = line("acme/fw.bin", "wifi", BLOB, "acme/fw.bin");
    for (bad, why) in [
        (good.replace("acme/fw.bin |", "../fw.bin |"), "`../fw.bin`"),
        (good.replace("acme/fw.bin |", "/acme/fw.bin |"), "absolute"),
        (good.replace("https://", "http://"), "https"),
        (good.replace(&hex(BLOB), "abc"), "digest"),
        (good.replace(&format!(" {} |", BLOB.len()), " 0 |"), "size"),
        (good.replace(" | wifi", ""), "6 fields"),
        (line("acme/fw.bin", "wifi", BLOB, "acme/L"), "not listed"),
        (format!("{good}{good}"), "twice"),
    ] {
        let error = parse_manifest(&bad).unwrap_err();
        assert!(error.contains(why), "{bad:?}: {error}");
    }
}

#[test]
fn only_the_pinned_bytes_are_placed() {
    let pins = parse_manifest(&manifest()).unwrap();
    assert_eq!(check_bytes(&pins[0], BLOB), Ok(()));
    let mut altered = BLOB.to_vec();
    altered[3] ^= 1;
    assert!(check_bytes(&pins[0], &altered)
        .unwrap_err()
        .contains("SHA-256"));
    assert!(check_bytes(&pins[0], &BLOB[1..])
        .unwrap_err()
        .contains("bytes"));

    let cache = std::env::temp_dir().join(format!("lazyos-wifi-fw-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&cache);
    std::fs::create_dir_all(cache.join("acme")).unwrap();
    let missing = firmware_files(&pins, "wifi", &cache).unwrap_err();
    assert!(missing.contains("tools/firmware/fetch.py"), "{missing}");
    std::fs::write(cache.join("acme/fw.bin"), BLOB).unwrap();
    std::fs::write(cache.join("acme/LICENCE.acme"), LICENCE).unwrap();
    let files = firmware_files(&pins, "wifi", &cache).unwrap();
    let dests: Vec<&str> = files.iter().map(|(dest, _)| dest.as_str()).collect();
    assert_eq!(
        dests,
        [
            format!("{}/acme/fw.bin", fhs::FIRMWARE),
            format!("{}/acme/LICENCE.acme", fhs::FIRMWARE)
        ]
    );
    std::fs::write(cache.join("acme/fw.bin"), &altered).unwrap();
    assert!(
        firmware_files(&pins, "wifi", &cache).is_err(),
        "a changed cache fails the build"
    );
    let _ = std::fs::remove_dir_all(&cache);
}
