//! The control tier (docs/dbgd-plan.md, v2): its switch, its methods, the
//! upload tracker and the base64 and digest parsers.

use alloc::string::String;
use alloc::vec::Vec;

use crate::config;
use crate::control::{self, Refusal, Upload, Write};
use crate::json;
use crate::methods::{self, Access};

const KEY: &str = "00112233445566778899aabbccddeeff";

#[test]
fn control_is_off_unless_both_lines_say_so() {
    let base = alloc::format!("diag.dbg=1\ndiag.dbg.key={KEY}\n");
    assert!(!config::parse(&base).unwrap().control);
    let on = base.clone() + "diag.dbg.control=1\n";
    assert!(config::parse(&on).unwrap().control);
    assert!(config::control_enabled(&on));
    assert!(!config::control_enabled(
        &(base.clone() + "diag.dbg.control=yes\n")
    ));
    assert!(!config::control_enabled(
        &(on.clone() + "diag.dbg.control=0\n")
    ));
    // Control without the service is nothing.
    assert!(!config::control_enabled("diag.dbg.control=1\n"));
    assert!(!config::control_enabled("diag.dbg=0\ndiag.dbg.control=1\n"));
}

#[test]
fn the_control_methods_take_checked_parameters() {
    let reload = methods::lookup("service.reload").unwrap();
    assert_eq!(reload.access, Access::Control);
    let ok = json::parse(r#"{"name":"usbd","sha256":"ab","trial_ms":5000}"#).unwrap();
    assert!(methods::validate(reload, &ok).is_ok());
    for bad in [
        r#"{"trial_ms":10}"#,
        r#"{"trial_ms":999999}"#,
        r#"{"name":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#,
        r#"{"binary":"/etc/shadow"}"#,
    ] {
        assert!(
            methods::validate(reload, &json::parse(bad).unwrap()).is_err(),
            "{bad}"
        );
    }
    let upload = methods::lookup("service.upload").unwrap();
    let big = "A".repeat(methods::UPLOAD_CHUNK_B64 + 4);
    let line = alloc::format!(r#"{{"data":"{big}"}}"#);
    // Refused by the parser's string cap or by the parameter bound.
    assert!(json::parse(&line).map_or(true, |v| methods::validate(upload, &v).is_err()));
    // A full chunk fits a request line.
    let full = "A".repeat(methods::UPLOAD_CHUNK_B64);
    let request = alloc::format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"service.upload","params":{{"name":"usbd","offset":0,"total":9000,"data":"{full}"}}}}"#
    );
    assert_eq!(methods::UPLOAD_CHUNK_B64, json::MAX_STRING);
    assert!(request.len() < crate::rpc::MAX_LINE);
    let parsed = crate::rpc::parse_request(&request).expect("a full chunk parses");
    assert!(methods::validate(upload, &parsed.params).is_ok());
    assert_eq!(
        methods::lookup("control.begin").unwrap().access,
        Access::Read
    );
}

#[test]
fn base64_round_trips_and_refuses_junk() {
    let cases: [(&str, &[u8]); 5] = [
        ("", b""),
        ("Zg==", b"f"),
        ("Zm8=", b"fo"),
        ("Zm9v", b"foo"),
        ("AP8A/w+/", &[0, 255, 0, 255, 15, 191]),
    ];
    for (text, bytes) in cases {
        assert_eq!(control::base64_decode(text).unwrap(), bytes, "{text}");
    }
    for bad in [
        "Zg", "Zg=", "Z===", "Zg==Zg==", "Zm9v\n", "Zm-v", "Zm9v====",
    ] {
        assert_eq!(control::base64_decode(bad), Err(Refusal::BadData), "{bad}");
    }
}

#[test]
fn only_plain_service_names_and_never_the_fabric_or_dbgd() {
    for good in ["usbd", "netdrv", "x", "a-b_c9"] {
        assert_eq!(control::reloadable(good), Ok(()), "{good}");
    }
    for bad in ["", "Usbd", "../init", "a/b", "a.b", &"a".repeat(33)] {
        assert_eq!(control::reloadable(bad), Err(Refusal::BadName), "{bad:?}");
    }
    assert_eq!(control::reloadable("messengerd"), Err(Refusal::Never));
    assert_eq!(control::reloadable("dbgd"), Err(Refusal::Never));
    let stage = fhs::state::DBGD_STAGE;
    assert!(control::is_staged_path(
        "usbd",
        &alloc::format!("{stage}/usbd.elf")
    ));
    assert!(!control::is_staged_path(
        "usbd",
        &alloc::format!("{stage}/netdrv.elf")
    ));
    assert!(!control::is_staged_path(
        "../x",
        &alloc::format!("{stage}/../x.elf")
    ));
    assert!(control::reload_path("usbd").starts_with(fhs::state::INIT_RELOAD));
}

#[test]
fn packages_have_their_own_name_path_and_limit() {
    assert_eq!(control::reloadable(control::PACKAGE), Err(Refusal::Never));
    assert!(control::staged_package_path().starts_with(fhs::state::DBGD_STAGE));
    assert!(control::staged_package_path().ends_with(".lzp"));
    assert_eq!(
        control::staged_path(control::PACKAGE),
        control::staged_package_path()
    );
    let mut up = None;
    let big = control::MAX_BINARY + 1;
    assert_eq!(
        Upload::accept(&mut up, control::PACKAGE, 0, big, 1),
        Ok(Write::Create)
    );
    assert_eq!(
        Upload::accept(&mut up, control::PACKAGE, 0, control::MAX_PACKAGE + 1, 1),
        Err(Refusal::TooBig)
    );
    assert_eq!(
        Upload::accept(&mut up, "usbd", 0, big, 1),
        Err(Refusal::TooBig)
    );
}

#[test]
fn app_ids_are_system_names_or_builtin_ids() {
    for good in ["org.lazy.doom", "os.lazy.writer", "term", "a-b_c.9"] {
        assert!(control::valid_app_id(good), "{good}");
    }
    for bad in ["", ".x", "a..b", "A.b", "a/b", "a b", &"a".repeat(65)] {
        assert!(!control::valid_app_id(bad), "{bad:?}");
    }
}

#[test]
fn digests_are_exactly_32_bytes_of_hex() {
    assert_eq!(control::parse_digest(&"ab".repeat(32)), Ok([0xab; 32]));
    for bad in ["", "ab", &"ab".repeat(33), &"zz".repeat(32)] {
        assert_eq!(control::parse_digest(bad), Err(Refusal::BadDigest));
    }
}

#[test]
fn an_upload_goes_in_order_and_stops_at_its_total() {
    let mut up = None;
    assert_eq!(Upload::finished(&up, "usbd"), Err(Refusal::Incomplete));
    assert_eq!(Upload::accept(&mut up, "usbd", 0, 10, 4), Ok(Write::Create));
    assert_eq!(
        Upload::accept(&mut up, "usbd", 3, 10, 4),
        Err(Refusal::OutOfOrder)
    );
    assert_eq!(
        Upload::accept(&mut up, "netdrv", 4, 10, 4),
        Err(Refusal::OutOfOrder)
    );
    assert_eq!(
        Upload::accept(&mut up, "usbd", 4, 11, 4),
        Err(Refusal::OutOfOrder)
    );
    assert_eq!(Upload::accept(&mut up, "usbd", 4, 10, 4), Ok(Write::Append));
    assert_eq!(Upload::finished(&up, "usbd"), Err(Refusal::Incomplete));
    assert_eq!(Upload::accept(&mut up, "usbd", 8, 10, 2), Ok(Write::Append));
    assert_eq!(Upload::finished(&up, "usbd").unwrap().total, 10);
    assert_eq!(Upload::finished(&up, "netdrv"), Err(Refusal::Incomplete));
    // Past the total: the upload is dropped.
    assert_eq!(
        Upload::accept(&mut up, "usbd", 10, 10, 1),
        Err(Refusal::TooBig)
    );
    assert_eq!(up, None);
    // Offset 0 always starts over; the limit holds.
    assert_eq!(
        Upload::accept(&mut up, "usbd", 0, control::MAX_BINARY + 1, 1),
        Err(Refusal::TooBig)
    );
    assert_eq!(
        Upload::accept(&mut up, "dbgd", 0, 10, 1),
        Err(Refusal::Never)
    );
}

#[test]
fn the_refusal_texts_are_distinct() {
    let all = [
        Refusal::BadName,
        Refusal::Never,
        Refusal::BadDigest,
        Refusal::BadData,
        Refusal::OutOfOrder,
        Refusal::TooBig,
        Refusal::Incomplete,
    ];
    let mut texts: Vec<String> = all.iter().map(|r| String::from(r.text())).collect();
    texts.sort();
    texts.dedup();
    assert_eq!(texts.len(), all.len());
}
