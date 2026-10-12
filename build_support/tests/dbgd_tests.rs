//! `dbgd`'s switch and `lazyos.cfg` lines (`build_support/dbgd_cfg.rs`): what
//! the image build writes is what the service parses.

use crate::os_image::dbgd_cfg::{
    hardware_lines, lines, validate_key, validate_peer, validate_range,
};

const KEY: &str = "00112233445566778899aabbccddeeff";

#[test]
fn the_lines_are_what_dbgd_reads_back() {
    let cfg = lines(KEY, Some(9000), Some("192.168.1.20"), false);
    let parsed = dbgwire::config::parse(&cfg).expect("dbgd accepts the build's lines");
    assert_eq!(parsed.port, 9000);
    assert_eq!(parsed.peer, Some([192, 168, 1, 20]));
    assert_eq!(parsed.key.len(), 16);
    assert!(!parsed.control, "control is off unless asked for");
    let minimal = dbgwire::config::parse(&lines(KEY, None, None, false)).unwrap();
    assert_eq!(minimal.port, dbgwire::config::DEFAULT_PORT);
    assert_eq!(minimal.peer, None);
    let control = lines(KEY, None, None, true);
    assert!(dbgwire::config::parse(&control).unwrap().control);
    // `init` reads the same switch on its own before it accepts a reload.
    assert!(dbgwire::config::control_enabled(&control));
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

#[test]
fn the_hardware_lines_are_what_the_kernel_and_dbgd_read_back() {
    // Off unless asked for, and the control tier is a separate switch.
    assert_eq!(hardware_lines(false, true, &["8086:5916:0:0-4".into()]), "");
    let base = lines(KEY, None, None, false);
    let off = dbgwire::config::parse(&base).unwrap();
    assert!(!off.probe.mmio);

    let allow = vec![
        "1af4:1050:0:0x10-0x30".to_string(),
        "8086:5916:2:0-1000".to_string(),
    ];
    let cfg = base + &hardware_lines(true, true, &allow);
    let parsed = dbgwire::config::parse(&cfg).unwrap();
    assert!(parsed.probe.mmio && parsed.probe.claimed);
    assert!(!parsed.control);
    assert_eq!(parsed.probe.extra().len(), 2);
    assert_eq!(parsed.probe.rejected, 0);
    // The kernel parses the same text with the same library.
    assert_eq!(dbgwire::pciprobe::Policy::from_cfg(&cfg), parsed.probe);
}

#[test]
fn ranges_the_policy_would_refuse_fail_the_build() {
    for good in [
        "8086:5916:0:0-4",
        "1AF4:1050:5:0x1000-0x2000",
        "8086:5916:0:0-4000000",
    ] {
        assert!(validate_range(good).is_ok(), "{good}");
        assert!(dbgwire::pciprobe::parse_range(good).is_some(), "{good}");
    }
    for bad in [
        "",
        "8086:5916:0",
        "8086:5916:6:0-4",
        "8086:5916:0:4-0",
        "8086:5916:0:1-4",
        "8086:5916:0:0-5",
        "8086:5916:0:0-4000004",
        "10000:5916:0:0-4",
        "zz:5916:0:0-4",
        "8086:5916:0:0-4:1",
    ] {
        assert!(validate_range(bad).is_err(), "{bad}");
        assert!(dbgwire::pciprobe::parse_range(bad).is_none(), "{bad}");
    }
}
