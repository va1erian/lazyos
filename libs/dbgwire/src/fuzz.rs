//! Fuzz entry point: untrusted bytes in, no panic out.

use alloc::string::String;

use crate::{config, json, logline, methods, rpc};

/// Feed `data` to every decoder that sees network or log bytes. Invariants:
/// nothing panics; a parsed value writes back to JSON that parses to a value
/// that writes the same text; a request that parses can be looked up and
/// validated.
pub fn run(data: &[u8]) {
    let Ok(text) = core::str::from_utf8(data) else {
        // Bytes that are not UTF-8 never reach the parsers: the service
        // drops the line (`rpc::parse_request` takes a `&str`).
        return;
    };
    if let Ok(value) = json::parse(text) {
        let first = render(&value);
        let reparsed = json::parse(&first).expect("written JSON parses");
        assert_eq!(render(&reparsed), first, "JSON write/parse round trip");
    }
    if let Ok(request) = rpc::parse_request(text) {
        if let Some(method) = methods::lookup(&request.method) {
            let _ = methods::validate(method, &request.params);
        }
    }
    let line = logline::parse(text);
    let _ = line.to_json(Some(1));
    let _ = logline::split_complete(text);
    let _ = config::parse(text);
    let _ = crate::auth::parse_key(text);
    let _ = crate::auth::verify_client(b"0123456789abcdef", b"nonce", text);
}

fn render(value: &json::Value) -> String {
    let mut out = String::new();
    json::write(&mut out, value);
    out
}
