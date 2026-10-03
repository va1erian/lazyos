//! `pkgstore::inspect::assess`, the assessment `pkgd`'s `Inspect` and
//! `Install` share and the LazyRAD IDE runs in its own process (it is a
//! labelled caller, which `pkgd` refuses).

#[path = "provisioning/lzp.rs"]
#[allow(dead_code)]
mod lzp;

use lazyos_crypto::hex;
use lazypkg::Package;
use pkgstore::inspect::assess;

#[test]
fn a_valid_package_is_described_with_its_digest_and_permissions() {
    let bytes = lzp::package("os.lazy.demo", "1.2.3", 4096, 7);
    let assessed = assess(&bytes).ok().expect("the package opens");
    let info = &assessed.info;
    assert_eq!(info.system_name, "os.lazy.demo");
    assert_eq!(info.version, "1.2.3");
    assert!(info.problems.is_empty(), "{:?}", info.problems);
    // The digest the install events carry: the whole archive's SHA-256.
    let digest = Package::open(&bytes).unwrap().digest();
    assert_eq!(info.digest, hex::encode(&digest));
    assert!(info.install_dir.starts_with("os.lazy.demo/1.2.3-"));
}

#[test]
fn an_archive_that_does_not_open_is_only_its_problems() {
    let info = assess(b"PK-not-an-archive").err().expect("refused");
    assert!(!info.problems.is_empty());
    assert!(info.system_name.is_empty() && info.digest.is_empty());
}

#[test]
fn problems_are_capped_and_each_line_is_bounded() {
    let mut bytes = lzp::package("os.lazy.demo", "1.2.3", 64, 1);
    bytes.truncate(bytes.len() / 2);
    let info = assess(&bytes)
        .err()
        .expect("a truncated archive is refused");
    assert!(info.problems.len() <= 13);
    assert!(info.problems.iter().all(|p| p.chars().count() <= 200));
}
