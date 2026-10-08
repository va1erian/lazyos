//! Hostile bytes against every decoder and the client (`fuzz::run`), shared
//! by the seeded tests and the cargo-fuzz target (`fuzz/fuzz_targets/smbwire.rs`).
//! A server's bytes are untrusted network input: nothing may panic, loop or
//! allocate without bound.

use alloc::vec::Vec;

use crate::client::{Client, Config, Signing, Transport};
use crate::frame::FrameReader;
use crate::{header, msg, name, ntlm, spnego, Error};

/// Every decoder over `data`; their results are ignored, only panics count.
fn decoders(data: &[u8]) {
    let _ = header::Header::parse(data);
    let _ = msg::parse_negotiate(data);
    let _ = msg::parse_session_setup(data);
    let _ = msg::parse_tree_connect(data);
    let _ = msg::parse_create(data);
    let _ = msg::parse_read(data, msg::MAX_IO);
    let _ = msg::parse_write(data);
    let _ = msg::parse_output(data, msg::MAX_IO);
    let _ = msg::parse_directory(data);
    let _ = msg::parse_network_open(data);
    let _ = msg::parse_fs_full_size(data);
    let _ = ntlm::Challenge::parse(data);
    let _ = spnego::parse_hint(data);
    let _ = spnego::parse_reply(data);
    let _ = name::listed(data);
}

/// A server that answers every request with the next slice of a script.
struct Script<'a> {
    rest: &'a [u8],
}

impl Transport for Script<'_> {
    fn send(&mut self, _bytes: &[u8]) -> Result<(), Error> {
        Ok(())
    }

    fn recv(&mut self) -> Result<Vec<u8>, Error> {
        let take = self
            .rest
            .len()
            .min(1 + (self.rest.first().copied().unwrap_or(0) as usize) * 7);
        let (chunk, rest) = self.rest.split_at(take);
        self.rest = rest;
        Ok(chunk.to_vec())
    }
}

fn logon(data: &[u8]) -> Option<Client<Script<'_>>> {
    let cfg = Config {
        user: "user",
        password: "password",
        domain: None,
        workstation: "LAZYOS",
        signing: Signing::Auto,
        client_guid: [7; 16],
        client_challenge: [9; 8],
        time: 0,
    };
    Client::connect(Script { rest: data }, &cfg)
        .ok()
        .map(|(client, _)| client)
}

/// The names a scripted session lists at the share root, if it gets that far
/// (the seed test's proof that the seeds reach deep).
#[cfg(test)]
pub fn listing_of(data: &[u8]) -> Option<Vec<alloc::string::String>> {
    let mut client = logon(data)?;
    client.tree_connect("server", "share").ok()?;
    Some(client.list("").ok()?.into_iter().map(|e| e.name).collect())
}

/// The client logging on and working against a scripted byte stream.
fn session(data: &[u8]) {
    if let Some(mut client) = logon(data) {
        let _ = client.tree_connect("server", "share");
        let _ = client.list("");
        if let Ok(opened) = client.open("a", crate::client::Open::Read) {
            let _ = client.read(&opened.file_id, 0, 4096);
        }
        let _ = client.statfs();
    }
}

pub fn run(data: &[u8]) {
    let Some((&mode, rest)) = data.split_first() else {
        return;
    };
    match mode % 3 {
        0 => decoders(rest),
        1 => {
            let mut reader = FrameReader::new();
            for chunk in rest.chunks(1 + (mode as usize >> 2)) {
                if reader.feed(chunk).is_err() {
                    break;
                }
                while reader.next_frame().is_some() {}
            }
        }
        _ => session(rest),
    }
}
