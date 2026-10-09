//! `dbgd`'s switch and `lazyos.cfg` lines (`build_support/dbgd_cfg.rs`): what
//! the image build writes is what the service parses.

use crate::os_image::dbgd_cfg::{lines, validate_key, validate_peer};

const KEY: &str = "00112233445566778899aabbccddeeff";

#[test]
fn the_lines_are_what_dbgd_reads_back() {
    let cfg = lines(KEY, Some(9000), Some("192.168.1.20"));
    let parsed = dbgwire::config::parse(&cfg).expect("dbgd accepts the build's lines");
    assert_eq!(parsed.port, 9000);
    assert_eq!(parsed.peer, Some([192, 168, 1, 20]));
    assert_eq!(parsed.key.len(), 16);
    let minimal = dbgwire::config::parse(&lines(KEY, None, None)).unwrap();
    assert_eq!(minimal.port, dbgwire::config::DEFAULT_PORT);
    assert_eq!(minimal.peer, None);
}

#[test]
fn keys_and_peers_the_service_would_refuse_fail_the_build() {
    assert!(validate_key(KEY).is_ok());
    assert!(validate_key(&"ab".repeat(64)).is_ok());
    for bad in [
        "",
        "abc",
        &"ab".repeat(15),
        &"ab".repeat(65),
        &"zz".repeat(16),
    ] {
        assert!(validate_key(bad).is_err(), "{bad:?}");
        assert!(dbgwire::config::parse(&format!("diag.dbg=1\ndiag.dbg.key={bad}\n")).is_err());
    }
    assert!(validate_peer("10.0.2.2").is_ok());
    for bad in ["10.0.2", "10.0.2.256", "a.b.c.d", "10.0.2.2.1", ""] {
        assert!(validate_peer(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn an_image_without_the_switch_carries_no_dbgd_lines() {
    // `from_env` is empty unless LAZYOS_DBGD=1: no key file is made either.
    let dir = std::env::temp_dir().join("lazyos-dbgd-cfg-test");
    let file = dir.join("dbgd.key");
    let _ = std::fs::remove_file(&file);
    if std::env::var_os("LAZYOS_DBGD").is_none() {
        assert_eq!(crate::os_image::dbgd_cfg::from_env(&file), "");
        assert!(!file.exists());
    }
}
