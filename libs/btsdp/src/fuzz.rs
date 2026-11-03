//! Fuzz entry point, shared by the seeded tests and the cargo-fuzz target
//! (`fuzz/fuzz_targets/btsdp.rs`).
//!
//! [`run`] uses the bytes three ways:
//!
//! * as attribute lists, straight into the record reader;
//! * as one response PDU to a fresh client (whose transaction id the input's
//!   own is patched to, so the PDU gets past the id check at least sometimes);
//! * as a script of `u8` length-prefixed response PDUs for one conversation,
//!   each with the client's current transaction id patched in.
//!
//! Nothing may panic, the client may never hold more than the bound, and a
//! descriptor that comes out is within its bound.

use std::vec::Vec;

use crate::record::parse_attribute_lists;
use crate::{Client, Progress, MAX_DESCRIPTOR};

pub fn run(data: &[u8]) {
    if let Ok(record) = parse_attribute_lists(data) {
        assert!(record.descriptor.len() <= MAX_DESCRIPTOR);
        assert!(!record.descriptor.is_empty());
    }
    single(data);
    script(data);
}

fn patched(pdu: &[u8], request: &[u8]) -> Vec<u8> {
    let mut pdu = pdu.to_vec();
    if pdu.len() >= 3 && request.len() >= 3 {
        pdu[1..3].copy_from_slice(&request[1..3]);
    }
    pdu
}

fn single(data: &[u8]) {
    let mut client = Client::new();
    let request = client.start();
    let _ = client.on_pdu(data);
    let _ = client.on_pdu(&patched(data, &request));
}

fn script(data: &[u8]) {
    let mut client = Client::new();
    let mut request = client.start();
    let mut rest = data;
    while let Some((&len, tail)) = rest.split_first() {
        let n = usize::from(len).min(tail.len());
        let (frame, next) = tail.split_at(n);
        rest = next;
        match client.on_pdu(&patched(frame, &request)) {
            Ok(Progress::Request(next)) => request = next,
            Ok(Progress::Done(record)) => {
                assert!(record.descriptor.len() <= MAX_DESCRIPTOR);
                request = client.start();
            }
            Err(_) => request = client.start(),
        }
    }
}
