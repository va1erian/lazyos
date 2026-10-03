//! The TLS-related system files (docs/tls-plan.md §5.1, §5.2, §7): the CA
//! bundle and its PEM encoder, the host table and the test switches that
//! append to them, and the HTTPS client's three names.

use crate::ca_bundle::{
    base64_decode, base64_encode, bundle, looks_like_certificate, parse_pem_certificates,
    pem_encode,
};
use crate::hosts_embed::{hosts_file, parse_hosts, valid_name, BASE};
use crate::os_image::{Manifest, OsFiles, Source};

/// A minimal DER `SEQUENCE` that passes the shape check.
fn fake_der(len: usize) -> Vec<u8> {
    let mut der = vec![0x30];
    if len < 0x80 {
        der.push(len as u8);
    } else {
        der.extend_from_slice(&[0x82, (len >> 8) as u8, len as u8]);
    }
    der.extend((0..len).map(|i| (i * 7) as u8));
    der
}

#[test]
fn base64_matches_rfc4648_vectors() {
    let vectors = [
        ("", ""),
        ("f", "Zg=="),
        ("fo", "Zm8="),
        ("foo", "Zm9v"),
        ("foob", "Zm9vYg=="),
        ("fooba", "Zm9vYmE="),
        ("foobar", "Zm9vYmFy"),
    ];
    for (plain, encoded) in vectors {
        assert_eq!(base64_encode(plain.as_bytes()), encoded);
        assert_eq!(base64_decode(encoded).as_deref(), Some(plain.as_bytes()));
    }
}

#[test]
fn base64_round_trips_every_length_and_byte() {
    for len in 0..300usize {
        let bytes: Vec<u8> = (0..len).map(|i| (i * 31 + len) as u8).collect();
        assert_eq!(base64_decode(&base64_encode(&bytes)), Some(bytes));
    }
}

#[test]
fn base64_refuses_malformed_input() {
    for bad in ["Zg=", "Zg===", "Z===", "Zm9v!A==", "Zg==Zm9v", "Zh==", "Zm9=", "Zm 9v"] {
        assert_eq!(base64_decode(bad), None, "{bad:?}");
    }
}

#[test]
fn pem_blocks_are_64_columns_and_parse_back() {
    let der = fake_der(500);
    let pem = pem_encode(&der);
    assert!(pem.starts_with("-----BEGIN CERTIFICATE-----\n"));
    assert!(pem.ends_with("-----END CERTIFICATE-----\n"));
    assert!(pem.lines().all(|line| line.len() <= 64 || line.starts_with("-----")));
    assert_eq!(parse_pem_certificates(&pem), Ok(vec![der]));
}

#[test]
fn the_der_shape_check() {
    assert!(looks_like_certificate(&fake_der(10)));
    assert!(looks_like_certificate(&fake_der(1000)));
    let mut long = fake_der(1000);
    long.push(0);
    assert!(!looks_like_certificate(&long), "trailing bytes");
    assert!(!looks_like_certificate(&[0x31, 1, 0]), "not a SEQUENCE");
    assert!(!looks_like_certificate(&[0x30, 0]), "empty");
    assert!(!looks_like_certificate(&[0x30, 0x85, 0, 0, 0, 0, 1]), "length too long");
    assert!(!looks_like_certificate(&[]));
}

#[test]
fn the_test_ca_must_be_certificates_only() {
    let one = pem_encode(&fake_der(40));
    let two = format!("{one}\n{}", pem_encode(&fake_der(300)));
    assert_eq!(parse_pem_certificates(&two).map(|c| c.len()), Ok(2));
    assert_eq!(
        parse_pem_certificates(&two.replace('\n', "\r\n")).map(|c| c.len()),
        Ok(2),
        "CRLF files"
    );
    let key = "-----BEGIN PRIVATE KEY-----\nMC4CAQA=\n-----END PRIVATE KEY-----\n";
    let refused = [
        ("", "no certificate"),
        ("\n\n", "no certificate"),
        (key, "a private key"),
        (&format!("{one}{key}") as &str, "a key after a certificate"),
        (&format!("hello\n{one}"), "stray text"),
        (&one.replace("-----END CERTIFICATE-----\n", ""), "unclosed"),
        ("-----BEGIN CERTIFICATE-----\n!!!!\n-----END CERTIFICATE-----\n", "bad base64"),
        ("-----BEGIN CERTIFICATE-----\nZm9vYmFy\n-----END CERTIFICATE-----\n", "not DER"),
    ];
    for (text, why) in refused {
        assert!(parse_pem_certificates(text).is_err(), "{why} accepted");
    }
}

#[test]
fn the_mozilla_bundle_parses_back_to_the_pinned_roots() {
    let roots: Vec<&[u8]> = webpki_root_certs::TLS_SERVER_ROOT_CERTS
        .iter()
        .map(|der| der.as_ref())
        .collect();
    assert!(roots.len() > 100, "only {} roots", roots.len());
    let text = bundle(roots.iter().copied(), &[]);
    let parsed = parse_pem_certificates(&text).expect("the bundle parses");
    assert_eq!(parsed.len(), roots.len());
    assert!(parsed.iter().zip(&roots).all(|(a, b)| a == b));
    // Plausible size: roughly 200 KiB of PEM.
    assert!((100_000..1_000_000).contains(&text.len()), "{} bytes", text.len());
    let extra = vec![fake_der(64)];
    let with_test = bundle(roots.iter().copied(), &extra);
    let parsed = parse_pem_certificates(&with_test).unwrap();
    assert_eq!(parsed.last(), Some(&extra[0]), "the test CA comes last");
}

#[test]
fn host_names_are_lowercase_dns_names() {
    for good in ["localhost", "tls.test", "a-b.example.org", "x1", "10-0-2-2.nip"] {
        assert!(valid_name(good), "{good}");
    }
    let long = "a".repeat(254);
    for bad in ["", "Upper.test", "under_score", ".lead", "trail.", "-x", "a..b", "a b", "é", &long] {
        assert!(!valid_name(bad), "{bad:?}");
    }
}

#[test]
fn test_hosts_are_validated_and_canonicalised() {
    let text = "# the harness\n\n10.0.2.2   tls.test  imap.test\n::1 v6.test\r\n";
    assert_eq!(
        parse_hosts(text),
        Ok(String::from("10.0.2.2\ttls.test imap.test\n::1\tv6.test\n"))
    );
    for bad in [
        "10.0.2.2\n",
        "10.0.2 tls.test\n",
        "tls.test 10.0.2.2\n",
        "10.0.2.2 TLS.test\n",
        "10.0.2.2 tls.test # comment\n",
        "fe80::1%eth0 tls.test\n",
    ] {
        assert!(parse_hosts(bad).is_err(), "{bad:?} accepted");
    }
    assert_eq!(hosts_file(""), BASE);
    assert!(BASE.contains("127.0.0.1\tlocalhost lazyos\n") && BASE.contains("::1\tlocalhost\n"));
}

fn bytes_of(files: &OsFiles, path: &str) -> Option<Vec<u8>> {
    let file = files.files().into_iter().find(|file| file.path == path)?;
    Some(match file.source {
        Source::Bytes(bytes) => bytes,
        Source::Path(path) => std::fs::read(path).unwrap(),
    })
}

/// Every environment-driven case in one test: the switches are process-wide.
#[test]
fn the_embeds_follow_their_switches() {
    let dir = std::env::temp_dir().join(format!("lazyos-tls-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for var in ["LAZYOS_TLS", "LAZYOS_FETCH", "LAZYOS_TLS_TEST_CA", "LAZYOS_TLS_TEST_HOSTS"] {
        std::env::remove_var(var);
    }
    let roots = [fake_der(20)];
    let plain = || {
        let mut files = OsFiles::default();
        crate::hosts_embed::embed(&mut files);
        crate::ca_bundle::embed(&mut files, roots.iter().map(Vec::as_slice));
        crate::tls_embed::embed(&mut files, &dir);
        files
    };
    // A normal image: the base host table, the roots alone, no client.
    let files = plain();
    assert_eq!(bytes_of(&files, fhs::etc::HOSTS), Some(BASE.as_bytes().to_vec()));
    assert_eq!(
        bytes_of(&files, fhs::etc::CA_BUNDLE),
        Some(pem_encode(&roots[0]).into_bytes())
    );
    assert!(bytes_of(&files, fhs::bin::FETCH).is_none());
    // Both files are build-placed, so an update replaces them.
    let manifest = Manifest::of(&[], &files.files()).unwrap();
    let text = manifest.to_text();
    assert!(text.contains(fhs::etc::HOSTS) && text.contains(fhs::etc::CA_BUNDLE));

    // The test switches append, in canonical form.
    let ca = dir.join("ca.pem");
    let test_der = fake_der(90);
    std::fs::write(&ca, pem_encode(&test_der)).unwrap();
    let hosts = dir.join("hosts");
    std::fs::write(&hosts, "10.0.2.2 tls.test\n").unwrap();
    std::env::set_var("LAZYOS_TLS_TEST_CA", &ca);
    std::env::set_var("LAZYOS_TLS_TEST_HOSTS", &hosts);
    let files = plain();
    let bundle_text = String::from_utf8(bytes_of(&files, fhs::etc::CA_BUNDLE).unwrap()).unwrap();
    assert_eq!(parse_pem_certificates(&bundle_text), Ok(vec![roots[0].clone(), test_der]));
    let table = String::from_utf8(bytes_of(&files, fhs::etc::HOSTS).unwrap()).unwrap();
    assert_eq!(table, format!("{BASE}10.0.2.2\ttls.test\n"));
    // A bad test file fails the build.
    std::fs::write(&hosts, "10.0.2.2 Bad_Name\n").unwrap();
    assert!(std::panic::catch_unwind(plain).is_err(), "a bad hosts file built");
    std::env::remove_var("LAZYOS_TLS_TEST_HOSTS");
    std::fs::write(&ca, "-----BEGIN PRIVATE KEY-----\nMC4=\n-----END PRIVATE KEY-----\n").unwrap();
    assert!(std::panic::catch_unwind(plain).is_err(), "a key was embedded");
    std::env::remove_var("LAZYOS_TLS_TEST_CA");

    // LAZYOS_TLS=1 without a client warns and builds; with one, three names.
    std::env::set_var("LAZYOS_TLS", "1");
    assert!(bytes_of(&plain(), fhs::bin::FETCH).is_none());
    let client = dir.join("fetch.elf");
    std::fs::write(&client, b"\x7fELF fake client").unwrap();
    std::env::set_var("LAZYOS_FETCH", &client);
    let files = plain();
    for name in crate::tls_embed::NAMES {
        assert_eq!(bytes_of(&files, name).as_deref(), Some(&b"\x7fELF fake client"[..]));
        let file = files.files().into_iter().find(|f| f.path == name).unwrap();
        assert_eq!(file.mode, 0o755, "{name} is executable");
    }
    std::env::set_var("LAZYOS_TLS", "0");
    assert!(bytes_of(&plain(), fhs::bin::CURL).is_none(), "LAZYOS_TLS=0 embedded");
    for var in ["LAZYOS_TLS", "LAZYOS_FETCH"] {
        std::env::remove_var(var);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
