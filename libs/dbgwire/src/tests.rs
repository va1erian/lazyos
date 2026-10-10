use alloc::string::String;
use alloc::vec::Vec;

use crate::auth::{self, Lockout};
use crate::config::{self, Refusal};
use crate::json::{self, Value};
use crate::logline;
use crate::methods::{self, Access};
use crate::rpc::{self, code, Id};

#[test]
fn json_scalars_and_containers() {
    assert_eq!(json::parse("null"), Ok(Value::Null));
    assert_eq!(json::parse(" true "), Ok(Value::Bool(true)));
    assert_eq!(json::parse("-12"), Ok(Value::Int(-12)));
    assert_eq!(json::parse("1.5"), Ok(Value::Float(1.5)));
    assert_eq!(
        json::parse("\"a\\n\\u00e9\\ud83d\\ude00\""),
        Ok(Value::Str("a\né😀".into()))
    );
    let value = json::parse(r#"{"a":[1,2,{"b":null}],"c":"d"}"#).unwrap();
    assert_eq!(value.get("c").and_then(Value::as_str), Some("d"));
}

#[test]
fn json_refuses_the_malformed() {
    for bad in [
        "",
        "{",
        "[1,]",
        "{\"a\":1,}",
        "01",
        "1.",
        "-",
        "\"\\x\"",
        "\"\\ud800\"",
        "nul",
        "{\"a\":1,\"a\":2}",
        "[1] 2",
        "\"a\nb\"",
        "{'a':1}",
        "1e",
        "tru",
    ] {
        assert!(json::parse(bad).is_err(), "{bad:?} parsed");
    }
}

#[test]
fn json_bounds() {
    let deep = "[".repeat(json::MAX_DEPTH + 1) + &"]".repeat(json::MAX_DEPTH + 1);
    assert_eq!(json::parse(&deep), Err(json::Error::TooDeep));
    let shallow = "[".repeat(json::MAX_DEPTH) + &"]".repeat(json::MAX_DEPTH);
    assert!(json::parse(&shallow).is_ok());
    let wide = alloc::format!("[{}1]", "1,".repeat(json::MAX_ELEMENTS));
    assert_eq!(json::parse(&wide), Err(json::Error::TooLong));
    let long = alloc::format!("\"{}\"", "x".repeat(json::MAX_STRING + 1));
    assert_eq!(json::parse(&long), Err(json::Error::TooLong));
}

#[test]
fn json_quote_round_trips_controls() {
    let text = "tab\t quote\" back\\ nl\n bell\u{7} del\u{7f} é";
    let quoted = json::quoted(text);
    assert_eq!(json::parse(&quoted), Ok(Value::Str(text.into())));
    assert!(!quoted.contains('\n'));
}

#[test]
fn object_builder() {
    let text = json::Object::new()
        .str("a", "x\"y")
        .uint("n", 7)
        .bool("b", true)
        .raw("r", "[1]")
        .finish();
    assert_eq!(text, "{\"a\":\"x\\\"y\",\"n\":7,\"b\":true,\"r\":[1]}");
    assert_eq!(json::Object::new().finish(), "{}");
}

#[test]
fn requests() {
    let r =
        rpc::parse_request(r#"{"jsonrpc":"2.0","id":5,"method":"log.tail","params":{"lines":3}}"#)
            .unwrap();
    assert_eq!(r.id, Some(Id::Int(5)));
    assert_eq!(r.method, "log.tail");
    assert_eq!(r.params.get("lines").and_then(Value::as_u64), Some(3));
    let n = rpc::parse_request(r#"{"jsonrpc":"2.0","method":"ping"}"#).unwrap();
    assert_eq!(n.id, None);
    assert_eq!(n.params, Value::Object(Vec::new()));
    let s = rpc::parse_request(r#"{"jsonrpc":"2.0","id":"a","method":"ping"}"#).unwrap();
    assert_eq!(s.id, Some(Id::Str("a".into())));
}

#[test]
fn bad_requests_say_why() {
    let code_of = |line: &str| rpc::parse_request(line).unwrap_err().code;
    assert_eq!(code_of("nonsense"), code::PARSE);
    assert_eq!(code_of("[1]"), code::INVALID_REQUEST);
    assert_eq!(
        code_of(r#"{"id":1,"method":"ping"}"#),
        code::INVALID_REQUEST
    );
    assert_eq!(
        code_of(r#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#),
        code::INVALID_REQUEST
    );
    assert_eq!(
        code_of(r#"{"jsonrpc":"2.0","id":1.5,"method":"ping"}"#),
        code::INVALID_REQUEST
    );
    assert_eq!(
        code_of(r#"{"jsonrpc":"2.0","id":1,"method":""}"#),
        code::INVALID_REQUEST
    );
    assert_eq!(
        code_of(r#"{"jsonrpc":"2.0","id":1,"method":"x","params":[1]}"#),
        code::INVALID_PARAMS
    );
    // The id of a refused request is echoed when it was readable.
    let f = rpc::parse_request(r#"{"jsonrpc":"2.0","id":9,"method":7}"#).unwrap_err();
    assert_eq!(f.id, Id::Int(9));
}

#[test]
fn answers_are_valid_json() {
    let ok = rpc::result_line(&Id::Str("q\"".into()), "{\"a\":1}");
    let value = json::parse(&ok).unwrap();
    assert_eq!(value.get("id").and_then(Value::as_str), Some("q\""));
    assert!(value.get("result").is_some());
    let err = rpc::error_line(&Id::Null, code::DENIED, "no \n way");
    let value = json::parse(&err).unwrap();
    assert_eq!(
        value.get("error").and_then(|e| e.get("code")),
        Some(&Value::Int(-32002))
    );
    let note = rpc::notification_line("log", "{}");
    assert!(json::parse(&note).unwrap().get("id").is_none());
}

#[test]
fn handshake() {
    let key = [7u8; 32];
    let nonce = [9u8; auth::NONCE_LEN];
    let mac = auth::hex(&auth::client_mac(&key, &nonce));
    assert!(auth::verify_client(&key, &nonce, &mac));
    assert!(auth::verify_client(&key, &nonce, &mac.to_uppercase()));
    assert!(!auth::verify_client(&key, &[8u8; auth::NONCE_LEN], &mac));
    assert!(!auth::verify_client(&[6u8; 32], &nonce, &mac));
    assert!(!auth::verify_client(&key, &nonce, ""));
    assert!(!auth::verify_client(&key, &nonce, "zz"));
    assert!(!auth::verify_client(&key, &nonce, &mac[..mac.len() - 2]));
    // The server's proof is not the client's: no reflection.
    assert_ne!(
        auth::client_mac(&key, &nonce),
        auth::server_mac(&key, &nonce)
    );
    let reflected = auth::hex(&auth::server_mac(&key, &nonce));
    assert!(!auth::verify_client(&key, &nonce, &reflected));
}

#[test]
fn key_lengths() {
    assert!(auth::parse_key(&"ab".repeat(15)).is_none());
    assert!(auth::parse_key(&"ab".repeat(16)).is_some());
    assert!(auth::parse_key(&"ab".repeat(64)).is_some());
    assert!(auth::parse_key(&"ab".repeat(65)).is_none());
    assert!(auth::parse_key(&"g".repeat(32)).is_none());
}

#[test]
fn lockout_backs_off_and_resets() {
    let mut l = Lockout::new();
    assert_eq!(l.check(0), Ok(()));
    l.failed(0);
    assert_eq!(l.check(500), Err(500));
    assert_eq!(l.check(1000), Ok(()));
    l.failed(1000);
    assert_eq!(l.check(1000), Err(2000));
    for _ in 0..40 {
        l.failed(0);
    }
    assert_eq!(l.check(0), Err(Lockout::MAX_DELAY_MS));
    l.succeeded();
    assert_eq!(l.check(0), Ok(()));
}

#[test]
fn config_parse() {
    let key = "00".repeat(16);
    let cfg = alloc::format!(
        "root=UUID=x\ndiag.dbg=1\ndiag.dbg.key={key}\ndiag.dbg.port=9000\ndiag.dbg.peer=10.0.2.2\n"
    );
    let parsed = config::parse(&cfg).unwrap();
    assert_eq!((parsed.port, parsed.peer), (9000, Some([10, 0, 2, 2])));
    assert_eq!(parsed.key.len(), 16);
    assert_eq!(config::parse("root=x\n"), Err(Refusal::Disabled));
    assert_eq!(
        config::parse("diag.dbg=0\ndiag.dbg.key=aa\n"),
        Err(Refusal::Disabled)
    );
    assert_eq!(config::parse("diag.dbg=1\n"), Err(Refusal::NoKey));
    assert_eq!(
        config::parse("diag.dbg=1\ndiag.dbg.key=ab\n"),
        Err(Refusal::BadKey)
    );
    let base = alloc::format!("diag.dbg=1\ndiag.dbg.key={key}\n");
    let with = |extra: &str| config::parse(&(base.clone() + extra));
    assert_eq!(with("diag.dbg.port=0\n"), Err(Refusal::BadPort));
    assert_eq!(with("diag.dbg.port=70000\n"), Err(Refusal::BadPort));
    assert_eq!(with("diag.dbg.peer=1.2.3\n"), Err(Refusal::BadPeer));
    assert_eq!(with("diag.dbg.peer=1.2.3.256\n"), Err(Refusal::BadPeer));
    assert_eq!(with("").unwrap().port, config::DEFAULT_PORT);
    // What the image build writes is what the service reads back.
    let written = config::cfg_lines(&key, Some(9701), Some("192.168.1.20"));
    assert_eq!(
        config::parse(&written).unwrap().peer,
        Some([192, 168, 1, 20])
    );
}

#[test]
fn method_table_is_consistent() {
    let mut names: Vec<&str> = methods::METHODS.iter().map(|m| m.name).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), methods::METHODS.len(), "duplicate method name");
    // Only `auth` is callable before authentication, and nothing that
    // changes the machine is outside the control tier.
    for m in methods::METHODS {
        let changes = (m.name.starts_with("service.") && m.name != "service.reloads")
            || m.name.starts_with("app.");
        assert_eq!(m.access == Access::Control, changes, "{}", m.name);
        assert_eq!(m.access == Access::Open, m.name == "auth", "{}", m.name);
        assert!(!m.summary.is_empty());
    }
    assert!(methods::lookup("log.tail").is_some());
    assert!(
        methods::lookup("fs.write").is_none(),
        "no general file write method"
    );
}

#[test]
fn params_are_checked() {
    let tail = methods::lookup("log.tail").unwrap();
    let ok = json::parse(r#"{"lines":10,"source":"usbd"}"#).unwrap();
    assert!(methods::validate(tail, &ok).is_ok());
    for bad in [
        r#"{"lines":0}"#,
        r#"{"lines":-1}"#,
        r#"{"lines":"3"}"#,
        r#"{"lines":99999}"#,
        r#"{"nope":1}"#,
        r#"{"source":3}"#,
    ] {
        let v = json::parse(bad).unwrap();
        assert!(methods::validate(tail, &v).is_err(), "{bad}");
    }
    assert!(methods::validate(tail, &Value::Int(1)).is_err());
}

#[test]
fn log_lines_become_records() {
    let l = logline::parse(
        "[12.345] USBD:DIAG port=1 timeout slot=Enabled/addr1 ep0dq=0x1f000 note=\"two words\" tail",
    );
    assert_eq!(l.stamp_ms, Some(12_345));
    assert_eq!(l.tag.as_deref(), Some("USBD:DIAG"));
    assert_eq!(l.field("port"), Some("1"));
    assert_eq!(l.field("slot"), Some("Enabled/addr1"));
    assert_eq!(l.field("ep0dq"), Some("0x1f000"));
    assert_eq!(l.field("note"), Some("two words"));
    assert_eq!(l.field("tail"), None);
    let json_text = l.to_json(Some(3));
    let v = json::parse(&json_text).unwrap();
    assert_eq!(v.get("pos"), Some(&Value::Int(3)));
    assert_eq!(
        v.get("fields")
            .and_then(|f| f.get("port"))
            .and_then(Value::as_str),
        Some("1")
    );

    let plain = logline::parse("kernel: mounted root");
    assert_eq!(plain.tag, None);
    assert!(plain.fields.is_empty());
    assert_eq!(plain.text, "kernel: mounted root");

    // `key=value` text that is not behind a marker is not split.
    assert!(logline::parse("a=1 b=2").fields.is_empty());
    let hw = logline::parse("HW:IRQCHIP:ioapic pins=24 dest=0");
    assert_eq!(hw.tag.as_deref(), Some("HW:IRQCHIP:ioapic"));
    assert_eq!(hw.field("pins"), Some("24"));
    // Unterminated quotes and empty keys cannot panic or loop.
    let odd = logline::parse("X:Y =1 k=\"open");
    assert_eq!(odd.field("k"), Some("open"));
}

#[test]
fn followers_hold_back_partial_lines() {
    let (lines, rest) = logline::split_complete("a\nb\nc");
    assert_eq!(lines, ["a", "b"]);
    assert_eq!(rest, "c");
    let (lines, rest) = logline::split_complete("a\n");
    assert_eq!(lines, ["a"]);
    assert_eq!(rest, "");
    let (lines, rest) = logline::split_complete("abc");
    assert!(lines.is_empty());
    assert_eq!(rest, "abc");
}

mod seeded {
    use super::*;
    use fuzzkit::for_seeds;

    #[test]
    fn replay_checked_in_seeds() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz");
        for dir in ["seeds/dbgwire", "regressions/dbgwire"] {
            let Ok(entries) = std::fs::read_dir(root.join(dir)) else {
                continue;
            };
            for entry in entries.flatten() {
                let bytes = std::fs::read(entry.path()).unwrap();
                crate::fuzz::run(&bytes);
            }
        }
    }

    #[test]
    fn random_bytes_never_panic() {
        for_seeds("dbgwire_random", |_, rng| {
            let len = rng.below(400) as usize;
            let bytes = rng.bytes(len);
            crate::fuzz::run(&bytes);
        });
    }

    #[test]
    fn mutated_requests_never_panic() {
        let base =
            r#"{"jsonrpc":"2.0","id":7,"method":"log.tail","params":{"lines":5,"source":"usbd"}}"#;
        for_seeds("dbgwire_mutate", |_, rng| {
            let mut bytes: Vec<u8> = base.as_bytes().to_vec();
            for _ in 0..(1 + rng.below(6)) {
                let at = rng.below(bytes.len() as u64) as usize;
                match rng.below(3) {
                    0 => bytes[at] = rng.byte(),
                    1 => {
                        bytes.remove(at);
                    }
                    _ => bytes.insert(at, b"{}[]\",:\\"[rng.below(8) as usize]),
                }
                if bytes.is_empty() {
                    bytes.push(b'{');
                }
            }
            crate::fuzz::run(&bytes);
            let _: String = String::from_utf8_lossy(&bytes).into_owned();
        });
    }
}

#[test]
fn fs_allowlist() {
    use crate::fsallow::{check, Refusal};
    assert_eq!(check("/transient/usbd.dump"), Ok(()));
    assert_eq!(check("/system/etc/passwd"), Ok(()));
    assert_eq!(check("/logs"), Ok(()));
    assert_eq!(check("/system/etc/shadow"), Err(Refusal::Secret));
    assert_eq!(check("/boot/lazyos.cfg"), Err(Refusal::Secret));
    assert_eq!(check("/conf/sys/x"), Err(Refusal::Secret));
    assert_eq!(check("/home/admin/x"), Err(Refusal::Outside));
    // A prefix is not a parent: `/tmpx` is not under `/tmp`.
    assert_eq!(check("/tmpx"), Err(Refusal::Outside));
    for bad in [
        "tmp/x",
        "/tmp/../conf/x",
        "/tmp//x",
        "/tmp/./x",
        "/tmp/a
b",
        "",
    ] {
        assert_eq!(check(bad), Err(Refusal::Malformed), "{bad:?}");
    }
}
