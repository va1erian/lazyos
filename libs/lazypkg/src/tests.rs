//! End-to-end tests for [`crate::Package`], built on the in-test zip writer.
//!
//! Each structural rule has a failure case here (not just the happy path), so a
//! regression in validation shows up as a test failure rather than a package
//! that installs something it should not.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::testzip::{self, Member};
use crate::{OpenError, Package, ReadError, MAX_ENTRIES, MAX_ENTRY_UNCOMPRESSED};

#[test]
fn opens_a_stored_package() {
    let bytes = testzip::valid(false);
    let package = Package::open(&bytes).expect("valid");
    assert_eq!(package.manifest().app.system_name, "org.lazy.demo");
    assert_eq!(package.manifest().app.version, "1.0.0");
    assert_eq!(package.entries().count(), 5);
    assert_eq!(package.read("icons/app-16.png").unwrap(), testzip::png());
    assert_eq!(package.digest(), lazyos_crypto::sha256::sha256(&bytes));

    let install = package.install_dir();
    assert!(install.starts_with("org.lazy.demo/1.0.0-"), "{install}");
    let suffix = install.rsplit('-').next().unwrap();
    assert_eq!(suffix.len(), 8);
    assert!(suffix
        .bytes()
        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
}

#[test]
fn opens_a_deflated_package_and_reads_entries() {
    let bytes = testzip::valid(true);
    let package = Package::open(&bytes).expect("valid");
    assert_eq!(package.read("bin/app.elf").unwrap(), b"ELF fake binary");
    assert_eq!(
        package.read("manifest.toml").unwrap(),
        testzip::manifest("bin/app.elf")
    );
}

#[test]
fn a_trailing_comment_does_not_hide_the_eocd() {
    let mut bytes = testzip::valid(false);
    let comment = vec![b'x'; 10];
    let eocd = bytes.len() - 22;
    let len = comment.len() as u16;
    bytes[eocd + 20..eocd + 22].copy_from_slice(&len.to_le_bytes());
    bytes.extend_from_slice(&comment);
    assert!(Package::open(&bytes).is_ok());
}

#[test]
fn rejects_trailing_garbage_after_the_eocd() {
    let mut bytes = testzip::valid(false);
    bytes.extend_from_slice(b"not part of the archive");
    assert_eq!(
        Package::open(&bytes).unwrap_err(),
        OpenError::NoEndOfCentralDirectory
    );
}

#[test]
fn rejects_a_manifest_free_package() {
    let members = &testzip::valid_members(false)[1..];
    let bytes = testzip::build(members);
    assert_eq!(Package::open(&bytes).unwrap_err(), OpenError::NoManifest);
}

#[test]
fn rejects_a_missing_required_icon() {
    let mut members = testzip::valid_members(false);
    members.remove(3); // icons/app-32.png
    let bytes = testzip::build(&members);
    assert!(matches!(
        Package::open(&bytes),
        Err(OpenError::Layout { name, .. }) if name == "icons/app-32.png"
    ));
}

#[test]
fn rejects_an_unknown_top_level_directory() {
    let mut members = testzip::valid_members(false);
    members.push(Member::stored("extra/foo.txt", b"x".to_vec()));
    let bytes = testzip::build(&members);
    assert!(matches!(
        Package::open(&bytes),
        Err(OpenError::Layout { name, .. }) if name == "extra/foo.txt"
    ));
}

#[test]
fn rejects_a_wrong_extension() {
    let mut members = testzip::valid_members(false);
    members.push(Member::stored("bin/notes.txt", b"x".to_vec()));
    let bytes = testzip::build(&members);
    assert!(matches!(
        Package::open(&bytes),
        Err(OpenError::Layout { name, .. }) if name == "bin/notes.txt"
    ));
}

#[test]
fn accepts_optional_directories_and_a_deflated_resource() {
    let mut members = testzip::valid_members(false);
    members.push(Member::stored("docs/", Vec::new()));
    members.push(Member::stored("docs/README.md", b"# docs".to_vec()));
    members.push(Member::deflated("resources/note.txt", b"hello".to_vec()));
    let bytes = testzip::build(&members);
    let package = Package::open(&bytes).expect("valid");
    assert_eq!(package.read("resources/note.txt").unwrap(), b"hello");
    assert_eq!(package.read("docs/").unwrap_err(), ReadError::IsDirectory);
}

#[test]
fn rejects_duplicate_and_case_colliding_names() {
    let mut members = testzip::valid_members(false);
    members.push(Member::stored("bin/app.elf", b"again".to_vec()));
    let bytes = testzip::build(&members);
    assert!(matches!(
        Package::open(&bytes),
        Err(OpenError::DuplicateName { .. })
    ));

    let mut members = testzip::valid_members(false);
    members.push(Member::stored("bin/APP.ELF", b"again".to_vec()));
    let bytes = testzip::build(&members);
    assert!(matches!(
        Package::open(&bytes),
        Err(OpenError::CaseCollision { .. })
    ));
}

#[test]
fn rejects_escaping_and_absolute_names() {
    for name in ["../evil", "/etc/passwd", "a/../../b", "C:/x"] {
        let mut members = testzip::valid_members(false);
        members.push(Member::stored(name, b"x".to_vec()));
        let bytes = testzip::build(&members);
        assert!(
            matches!(Package::open(&bytes), Err(OpenError::BadPath { .. })),
            "{name:?} should be rejected"
        );
    }
}

#[test]
fn rejects_a_directory_with_data() {
    let mut members = testzip::valid_members(false);
    members.push(Member::stored("docs/", b"not empty".to_vec()));
    let bytes = testzip::build(&members);
    assert!(matches!(
        Package::open(&bytes),
        Err(OpenError::BadPath { .. })
    ));
}

#[test]
fn rejects_zip64_extra_fields() {
    let mut members = testzip::valid_members(false);
    members[0].extra = vec![0x01, 0x00, 0x00, 0x00];
    let bytes = testzip::build(&members);
    assert_eq!(
        Package::open(&bytes).unwrap_err(),
        OpenError::Zip64Unsupported
    );
}

#[test]
fn rejects_data_descriptors_and_encryption() {
    for flags in [0x0008u16, 0x0001] {
        let mut members = testzip::valid_members(false);
        members[0].flags = flags;
        let bytes = testzip::build(&members);
        assert!(matches!(
            Package::open(&bytes),
            Err(OpenError::UnsupportedFlags { .. })
        ));
    }
}

#[test]
fn rejects_unsupported_compression() {
    let mut members = testzip::valid_members(false);
    members[0].method = Some(12);
    let bytes = testzip::build(&members);
    assert!(matches!(
        Package::open(&bytes),
        Err(OpenError::UnsupportedCompression { method: 12, .. })
    ));
}

#[test]
fn rejects_a_local_header_that_disagrees() {
    let mut members = testzip::valid_members(false);
    members[1].local_method = Some(8);
    let bytes = testzip::build(&members);
    assert!(matches!(
        Package::open(&bytes),
        Err(OpenError::BadLocalHeader { .. })
    ));
}

#[test]
fn rejects_local_data_that_overlaps_the_directory() {
    let mut members = testzip::valid_members(true);
    members[1].declared_compressed = Some(1_000_000);
    let bytes = testzip::build(&members);
    assert!(matches!(
        Package::open(&bytes),
        Err(OpenError::BadLocalHeader { .. })
    ));
}

#[test]
fn rejects_a_central_directory_outside_the_archive() {
    let mut bytes = testzip::valid(false);
    let eocd = bytes.len() - 22;
    bytes[eocd + 16..eocd + 20].copy_from_slice(&0xFFFF_FFF0u32.to_le_bytes());
    assert_eq!(
        Package::open(&bytes).unwrap_err(),
        OpenError::BadCentralDirectory
    );
}

#[test]
fn rejects_an_overlong_entry_name() {
    let long: &'static str = Box::leak("a".repeat(crate::MAX_NAME_LEN + 1).into_boxed_str());
    let mut members = testzip::valid_members(false);
    members.push(Member::stored(long, b"x".to_vec()));
    let bytes = testzip::build(&members);
    assert_eq!(
        Package::open(&bytes).unwrap_err(),
        OpenError::BadName {
            reason: "is longer than 255 bytes"
        }
    );
}

#[test]
fn rejects_a_non_utf8_entry_name() {
    let mut bytes = testzip::valid(false);
    let eocd = bytes.len() - 22;
    let cd_offset = u32::from_le_bytes(bytes[eocd + 16..eocd + 20].try_into().unwrap()) as usize;
    bytes[cd_offset + 46] = 0xFF; // the first byte of the first central name
    assert!(matches!(
        Package::open(&bytes),
        Err(OpenError::BadName { .. })
    ));
}

#[test]
fn rejects_too_many_entries() {
    let members: Vec<Member<'_>> = (0..=MAX_ENTRIES)
        .map(|index| Member::stored("resources/f", format!("{index}").into_bytes()))
        .collect();
    let bytes = testzip::build(&members);
    assert!(matches!(
        Package::open(&bytes),
        Err(OpenError::TooManyEntries { count }) if count == (MAX_ENTRIES + 1) as u64
    ));
}

#[test]
fn rejects_an_oversized_entry() {
    let mut members = testzip::valid_members(false);
    members[0].declared_size = Some(MAX_ENTRY_UNCOMPRESSED + 1);
    let bytes = testzip::build(&members);
    assert!(matches!(
        Package::open(&bytes),
        Err(OpenError::EntryTooLarge { .. })
    ));
}

#[test]
fn rejects_an_oversized_total() {
    let mut members = testzip::valid_members(false);
    for _ in 0..5 {
        members.push(
            Member::stored("resources/f", b"x".to_vec()).declared_size(MAX_ENTRY_UNCOMPRESSED),
        );
    }
    let bytes = testzip::build(&members);
    assert!(matches!(
        Package::open(&bytes),
        Err(OpenError::TotalTooLarge { .. })
    ));
}

#[test]
fn read_errors_are_specific() {
    let bytes = testzip::valid(false);
    let package = Package::open(&bytes).unwrap();
    assert_eq!(package.read("nope").unwrap_err(), ReadError::NoSuchEntry);

    let mut members = testzip::valid_members(false);
    members[1].crc = Some(0xdead_beef);
    let bytes = testzip::build(&members);
    let package = Package::open(&bytes).expect("structure is fine");
    assert!(matches!(
        package.read("bin/app.elf"),
        Err(ReadError::CrcMismatch { .. })
    ));
}

#[test]
fn rejects_a_non_png_icon() {
    let mut members = testzip::valid_members(false);
    members[2].data = b"not a png".to_vec();
    let bytes = testzip::build(&members);
    assert!(matches!(
        Package::open(&bytes),
        Err(OpenError::BadIcon { name }) if name == "icons/app-16.png"
    ));
}

#[test]
fn collects_every_manifest_problem() {
    let manifest = b"[app]\nname = \"\"\nsystem_name = \"Bad Name\"\nauthor = \"\"\nversion = \"1\"\n\n[entry]\nbinary = \"bin/missing.elf\"\n".to_vec();
    let mut members = testzip::valid_members(false);
    members[0].data = manifest;
    let bytes = testzip::build(&members);
    match Package::open(&bytes) {
        Err(OpenError::Manifest(error)) => {
            assert!(error.problems().len() >= 4, "{error}");
            assert!(format!("{error}").starts_with("manifest: "));
        }
        other => panic!("expected manifest errors, got {other:?}"),
    }
}

#[test]
fn a_non_utf8_manifest_is_an_error_not_a_panic() {
    let mut members = testzip::valid_members(false);
    members[0].data = vec![0xff, 0xfe, 0x00];
    let bytes = testzip::build(&members);
    assert!(matches!(Package::open(&bytes), Err(OpenError::Manifest(_))));
}

#[test]
fn manifest_error_messages_are_one_line() {
    let error = OpenError::Manifest(crate::ManifestError::new(vec![
        crate::Problem::new(String::from("first")),
        crate::Problem::new(String::from("second")),
    ]));
    let text = format!("{error}");
    assert!(!text.contains('\n'));
    assert_eq!(text, "manifest: first; second");
}
