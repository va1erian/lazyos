//! A DNS answer builder for the scripted gateway: parses one query and builds
//! the response a resolver would send.

use std::string::String;
use std::vec::Vec;

/// The name in the question section, as dotted text, and where the question ends.
fn question(query: &[u8]) -> Option<(String, usize)> {
    let mut at = 12;
    let mut name = String::new();
    loop {
        let len = *query.get(at)? as usize;
        at += 1;
        if len == 0 {
            break;
        }
        if !name.is_empty() {
            name.push('.');
        }
        name.push_str(core::str::from_utf8(query.get(at..at + len)?).ok()?);
        at += len;
    }
    // QTYPE and QCLASS.
    (query.len() >= at + 4).then_some((name, at + 4))
}

/// The response to `query`: the records for the name if `records` has any, else
/// NXDOMAIN. `None` when the query does not parse.
pub fn answer(query: &[u8], records: &[(&str, [u8; 4])]) -> Option<Vec<u8>> {
    let (name, end) = question(query)?;
    let hits: Vec<[u8; 4]> = records
        .iter()
        .filter(|(n, _)| n.eq_ignore_ascii_case(&name))
        .map(|(_, a)| *a)
        .collect();
    let rcode = if hits.is_empty() { 3 } else { 0 };
    let mut out = Vec::new();
    out.extend_from_slice(&query[0..2]);
    out.extend_from_slice(&(0x8180u16 | rcode).to_be_bytes());
    out.extend_from_slice(&[0, 1]);
    out.extend_from_slice(&(hits.len() as u16).to_be_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&query[12..end]);
    for addr in hits {
        out.extend_from_slice(&[0xC0, 0x0C, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4]);
        out.extend_from_slice(&addr);
    }
    Some(out)
}
