//! The network payload (`build_support/netboot.rs`, docs/netboot-plan.md N1):
//! the build id, `manifest.json` and its digests, the Ed25519 signature the
//! registry verifies, and that the files are the inputs' bytes.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};

use crate::netboot::manifest::{
    hex, manifest_json, parse_key, revision, signature_file, valid_id, BuildInfo, FileEntry, Utc,
};
use crate::netboot::{self, Inputs};
use crate::usb_ramdisk::Written;

const REV: &str = "0123456789abcdef0123456789abcdef01234567";
/// 2026-10-12T09:30:00Z.
const WHEN: i64 = 1_791_797_400;

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Scratch {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "lazyos-netboot-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn vars(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect()
}

fn info(dirty: bool) -> BuildInfo {
    BuildInfo::new(WHEN, REV, dirty, "devbox", vars(&[("LAZYOS_USB", "1")]))
}

fn sha(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// A seed for the tests' own key: the OS random source, as the build uses.
fn seed_text() -> String {
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).expect("OS random source");
    format!("{}\n", hex(&seed))
}

/// `(kernel, ramdisk, loader, boot.json)` contents.
fn payload() -> (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>) {
    (
        (0..70_000u32).map(|n| (n * 7) as u8).collect(),
        (0..3_000_000u32).map(|n| (n % 251) as u8).collect(),
        b"MZ\x90 a PE image".to_vec(),
        b"{\n  \"frame_buffer\": {}\n}".to_vec(),
    )
}

fn write_in(
    scratch: &Scratch,
    dir: &str,
    key: Option<&ed25519_dalek::SigningKey>,
    info: &BuildInfo,
) -> (PathBuf, netboot::Output) {
    let (kernel, ramdisk, loader, boot_json) = payload();
    let sources = scratch.0.join("src");
    std::fs::create_dir_all(&sources).unwrap();
    std::fs::write(sources.join("k"), &kernel).unwrap();
    std::fs::write(sources.join("r"), &ramdisk).unwrap();
    let out = scratch.0.join(dir);
    let inputs = Inputs {
        kernel: &sources.join("k"),
        ramdisk: &sources.join("r"),
        loader: &loader,
        boot_json: &boot_json,
    };
    let output = netboot::write(&out, &inputs, info, key).unwrap();
    (out, output)
}

#[test]
fn times_are_civil_utc() {
    assert_eq!(Utc::from_unix(0).iso(), "1970-01-01T00:00:00Z");
    assert_eq!(Utc::from_unix(WHEN).iso(), "2026-10-12T09:30:00Z");
    assert_eq!(Utc::from_unix(WHEN).compact(), "20261012-093000");
    // A leap day, the day after, and a century that is not a leap year.
    assert_eq!(Utc::from_unix(1_709_251_199).iso(), "2024-02-29T23:59:59Z");
    assert_eq!(Utc::from_unix(951_868_800).iso(), "2000-03-01T00:00:00Z");
    assert_eq!(Utc::from_unix(4_102_444_800).iso(), "2100-01-01T00:00:00Z");
}

#[test]
fn the_build_id_has_the_documented_shape() {
    let clean = info(false);
    assert_eq!(clean.id, "20261012-093000-01234567");
    assert_eq!(clean.revision, REV);
    assert!(!clean.dirty);
    assert_eq!(clean.built_at, "2026-10-12T09:30:00Z");
    let dirty = info(true);
    assert_eq!(dirty.id, "20261012-093000-01234567-dirty");
    assert!(dirty.dirty);
    // Outside a checkout: the all-zero revision in the id, `unknown` in the manifest.
    let unknown = BuildInfo::new(WHEN, "unknown", false, "h", vars(&[]));
    assert_eq!(unknown.id, "20261012-093000-00000000");
    assert_eq!(unknown.revision, "unknown");
    for id in [&clean.id, &dirty.id, &unknown.id] {
        assert!(valid_id(id), "{id}");
    }
}

#[test]
fn the_kernels_image_id_rule() {
    assert!(valid_id("a"));
    assert!(valid_id(&"a".repeat(64)));
    assert!(valid_id("Ab9._-"));
    for bad in [
        "",
        &"a".repeat(65),
        "a b",
        "a/b",
        "a=b",
        "caf\u{e9}",
        "a\n",
        "a:b",
    ] {
        assert!(!valid_id(bad), "{bad:?}");
    }
}

#[test]
fn the_revision_is_forty_hex_or_unknown() {
    let (rev, problem) = revision(None, Some(REV));
    assert_eq!((rev.as_str(), problem), (REV, None));
    // The kernel's order: the variable wins over git, and an empty one does not count.
    let other = "f".repeat(40);
    assert_eq!(revision(Some(&other), Some(REV)).0, other);
    assert_eq!(revision(Some("  "), Some(REV)).0, REV);
    assert_eq!(revision(Some(&REV.to_uppercase()), None).0, REV);
    assert_eq!(revision(None, None), ("unknown".to_string(), None));
    for bad in ["abc123", &"g".repeat(40), &"a".repeat(41)] {
        let (rev, problem) = revision(Some(bad), None);
        assert_eq!(rev, "unknown");
        assert!(problem.is_some(), "{bad}");
    }
}

#[test]
fn switches_are_the_set_lazyos_variables_sorted() {
    let built = BuildInfo::new(
        WHEN,
        REV,
        false,
        "h",
        vars(&[
            ("PATH", "/bin"),
            ("LAZYOS_USB", "1"),
            ("LAZYOS_DESKTOP", "1"),
            ("LAZYOS_EMPTY", ""),
            ("LAZYOS_NETBOOT", "1"),
            ("NOT_LAZYOS_X", "1"),
            ("LAZYOS_USB", "1"),
        ]),
    );
    assert_eq!(
        built.switches,
        ["LAZYOS_DESKTOP", "LAZYOS_NETBOOT", "LAZYOS_USB"]
    );
}

#[test]
fn the_manifest_is_the_registrys_json() {
    let files = vec![
        FileEntry {
            name: "kernel-x86_64".into(),
            size: 12,
            sha256: sha(b"k"),
        },
        FileEntry {
            name: "ramdisk".into(),
            size: 34,
            sha256: sha(b"r"),
        },
    ];
    let mut build = info(true);
    build.host = "odd \"host\"\\\n".into();
    let text = manifest_json(&build, &files);
    let json: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
    assert_eq!(json["id"], build.id);
    assert_eq!(json["revision"], REV);
    assert_eq!(json["dirty"], true);
    assert_eq!(json["built_at"], "2026-10-12T09:30:00Z");
    assert_eq!(json["host"], "odd \"host\"\\\n");
    assert_eq!(json["switches"], serde_json::json!(["LAZYOS_USB"]));
    assert_eq!(json["files"]["kernel-x86_64"]["size"], 12);
    assert_eq!(json["files"]["ramdisk"]["sha256"], sha(b"r"));
    // Keys in the documented order, and nothing else at the top level.
    let order: Vec<usize> = [
        "id", "revision", "dirty", "built_at", "host", "switches", "files",
    ]
    .iter()
    .map(|key| text.find(&format!("\"{key}\":")).expect(key))
    .collect();
    assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{order:?}");
    assert_eq!(json.as_object().unwrap().len(), 7);
    // No switches is an empty list, not a missing key.
    let none = BuildInfo::new(WHEN, REV, false, "h", vars(&[]));
    let json: serde_json::Value = serde_json::from_str(&manifest_json(&none, &files)).unwrap();
    assert_eq!(json["switches"], serde_json::json!([]));
}

#[test]
fn the_payload_is_the_inputs_bytes_with_matching_digests() {
    let scratch = Scratch::new();
    let (kernel, ramdisk, loader, boot_json) = payload();
    let build = info(false);
    let (dir, output) = write_in(&scratch, "out", None, &build);
    for (name, bytes) in [
        ("kernel-x86_64", &kernel),
        ("ramdisk", &ramdisk),
        ("bootx64.efi", &loader),
        ("boot.json", &boot_json),
    ] {
        assert_eq!(&std::fs::read(dir.join(name)).unwrap(), bytes, "{name}");
    }
    assert_eq!(
        std::fs::read_to_string(dir.join("manifest.json")).unwrap(),
        output.manifest
    );
    let json: serde_json::Value = serde_json::from_str(&output.manifest).unwrap();
    let files = json["files"].as_object().unwrap();
    assert_eq!(files.len(), 4);
    for name in ["kernel-x86_64", "ramdisk", "bootx64.efi", "boot.json"] {
        assert!(files.contains_key(name), "{name}");
    }
    for (name, entry) in files {
        let bytes = std::fs::read(dir.join(name)).unwrap();
        assert_eq!(entry["size"], bytes.len() as u64, "{name} size");
        assert_eq!(entry["sha256"], sha(&bytes), "{name} digest");
        assert_eq!(entry["sha256"].as_str().unwrap().len(), 64);
    }
    // No temporary files are left behind.
    let names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert!(
        names.iter().all(|name| !name.ends_with(".tmp")),
        "{names:?}"
    );
}

#[test]
fn two_writes_of_one_payload_are_identical() {
    // `LAZYOS_USB_IMAGE=1` with `LAZYOS_NETBOOT=1` makes the payload once and
    // hands the same files to both: the netboot directory is a pure function
    // of its inputs and the build info.
    let scratch = Scratch::new();
    let build = info(false);
    let (first, _) = write_in(&scratch, "a", None, &build);
    let (second, _) = write_in(&scratch, "b", None, &build);
    for name in [
        "kernel-x86_64",
        "ramdisk",
        "bootx64.efi",
        "boot.json",
        "manifest.json",
    ] {
        assert_eq!(
            std::fs::read(first.join(name)).unwrap(),
            std::fs::read(second.join(name)).unwrap(),
            "{name}"
        );
    }
}

#[test]
fn rfc_8032_test_vector_1() {
    let seed = "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60";
    let key = parse_key(seed).unwrap();
    assert_eq!(
        hex(key.verifying_key().as_bytes()),
        "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a"
    );
    assert_eq!(
        signature_file(&key, b""),
        "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555\
         fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b\n"
    );
}

#[test]
fn the_signature_covers_the_exact_manifest_bytes() {
    let scratch = Scratch::new();
    let key = parse_key(&seed_text()).unwrap();
    let (dir, output) = write_in(&scratch, "signed", Some(&key), &info(false));
    assert!(output.signed);
    let sig = std::fs::read_to_string(dir.join("manifest.sig")).unwrap();
    assert_eq!(sig.len(), 129, "128 hex digits and a newline");
    assert!(sig.ends_with('\n'));
    assert!(sig[..128]
        .bytes()
        .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')));
    let mut raw = [0u8; 64];
    for (byte, pair) in raw.iter_mut().zip(sig[..128].as_bytes().chunks(2)) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap();
    }
    let signature = Signature::from_bytes(&raw);
    let public: VerifyingKey = key.verifying_key();
    let manifest = std::fs::read(dir.join("manifest.json")).unwrap();
    public
        .verify(&manifest, &signature)
        .expect("signature verifies");
    let mut tampered = manifest.clone();
    tampered[10] ^= 1;
    assert!(public.verify(&tampered, &signature).is_err());
    let other = parse_key(&seed_text()).unwrap().verifying_key();
    assert!(other.verify(&manifest, &signature).is_err());
}

#[test]
fn an_unsigned_build_leaves_no_stale_signature() {
    let scratch = Scratch::new();
    let key = parse_key(&seed_text()).unwrap();
    let (dir, _) = write_in(&scratch, "out", Some(&key), &info(false));
    assert!(dir.join("manifest.sig").exists());
    // The next build has no key: the old signature must not survive it.
    let (_, output) = write_in(&scratch, "out", None, &info(true));
    assert!(!output.signed);
    assert!(!dir.join("manifest.sig").exists());
    assert!(dir.join("manifest.json").exists());
}

#[test]
fn key_files_are_the_seed_in_hex() {
    let seed = "00".repeat(32);
    assert!(parse_key(&seed).is_ok());
    assert!(parse_key(&format!("{seed}\n")).is_ok());
    assert!(parse_key(&format!("{seed}\r\n")).is_ok());
    for bad in [
        String::new(),
        "00".repeat(31),
        "00".repeat(33),
        "zz".repeat(32),
        format!("{seed} {seed}"),
        "0".repeat(63) + "-",
    ] {
        assert!(parse_key(&bad).is_err(), "{bad:?}");
    }
}

#[test]
fn the_ramdisk_size_is_accounted_for() {
    let written = Written {
        bytes: 308 << 20,
        os_bytes: 306 << 20,
        files_bytes: 200 << 20,
        packages_bytes: 30 << 20,
        free_bytes: 64 << 20,
    };
    let line = netboot::breakdown(&written);
    assert!(!line.contains('\n'));
    assert!(line.contains("308 MiB"), "{line}");
    assert!(line.contains("files 200 MiB"), "{line}");
    assert!(
        line.contains("packages unpacked at first boot 30 MiB"),
        "{line}"
    );
    assert!(line.contains("free space 64 MiB"), "{line}");
    assert!(line.contains("regrown 12 MiB"), "{line}");
    assert!(line.contains("MBR and /boot 2 MiB"), "{line}");
}
