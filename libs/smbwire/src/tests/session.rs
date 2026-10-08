//! Whole sessions against the in-memory server, and the refusals.

use std::string::String;
use std::vec::Vec;

use super::server::{Behaviour, Server, DOMAIN, PASSWORD, USER};
use crate::client::{Client, Config, Logon, Open, Signing, Transport};
use crate::header::command as cmd;
use crate::msg;
use crate::status;
use crate::Error;

fn config(password: &str, signing: Signing) -> Config<'_> {
    Config {
        user: USER,
        password,
        domain: None,
        workstation: "LAZYOS",
        signing,
        client_guid: [0x11; 16],
        client_challenge: [0x22; 8],
        time: 0x01DA_0000_0000_0000,
    }
}

fn connect(how: Behaviour, signing: Signing) -> Result<(Client<Server>, Logon), Error> {
    Client::connect(Server::new(how), &config(PASSWORD, signing))
}

/// Read a whole file through the client.
fn read_all(c: &mut Client<Server>, path: &str) -> Result<Vec<u8>, Error> {
    let opened = c.open(path, Open::Read)?;
    let mut data = Vec::new();
    loop {
        let chunk = c.read(&opened.file_id, data.len() as u64, 65536)?;
        if chunk.is_empty() {
            break;
        }
        data.extend_from_slice(&chunk);
    }
    c.close(&opened.file_id)?;
    Ok(data)
}

fn write_all(c: &mut Client<Server>, path: &str, data: &[u8]) -> Result<(), Error> {
    let opened = c.open(path, Open::Replace)?;
    let mut at = 0;
    while at < data.len() {
        let end = (at + c.max_write() as usize).min(data.len());
        let n = c.write(&opened.file_id, at as u64, &data[at..end])? as usize;
        assert!(n > 0);
        at += n;
    }
    c.flush(&opened.file_id)?;
    c.close(&opened.file_id)
}

/// The round trip every configuration must pass.
fn work(c: &mut Client<Server>) {
    c.tree_connect("10.0.2.2", "share").unwrap();
    let names: Vec<String> = c.list("").unwrap().into_iter().map(|e| e.name).collect();
    assert_eq!(names.len(), 2, "{names:?}: no . or ..");
    assert!(names.contains(&String::from("hello.txt")) && names.contains(&String::from("docs")));
    assert_eq!(
        read_all(c, "hello.txt").unwrap(),
        b"hello from the server\n"
    );
    assert_eq!(read_all(c, "/docs/readme.md").unwrap(), b"# read me\n");
    let big: Vec<u8> = (0..150_000u32).map(|i| (i * 31 + (i >> 8)) as u8).collect();
    write_all(c, "up.bin", &big).unwrap();
    assert_eq!(c.transport().files["up.bin"], big, "the server's own copy");
    assert_eq!(read_all(c, "up.bin").unwrap(), big);
    assert_eq!(c.stat("up.bin").unwrap().end_of_file, big.len() as u64);
    assert!(c.stat("docs").unwrap().is_dir());
    c.mkdir("new").unwrap();
    c.rename("up.bin", "new/moved.bin", false).unwrap();
    assert!(c.transport().files.contains_key("new/moved.bin"));
    assert_eq!(
        c.delete("new").map_err(|e| e.status()),
        Err(Some(status::DIRECTORY_NOT_EMPTY))
    );
    c.delete("new/moved.bin").unwrap();
    c.delete("new").unwrap();
    assert!(!c.transport().dirs.contains("new"));
    let opened = c.open("hello.txt", Open::Replace).unwrap();
    c.write(&opened.file_id, 0, b"0123456789").unwrap();
    c.set_size(&opened.file_id, 4).unwrap();
    c.close(&opened.file_id).unwrap();
    assert_eq!(c.transport().files["hello.txt"], b"0123");
    let fs = c.statfs().unwrap();
    assert_eq!(
        (fs.total, fs.available, fs.unit),
        (1000 * 4096, 250 * 4096, 4096)
    );
    assert_eq!(
        c.stat("missing").map_err(|e| e.status()),
        Err(Some(status::OBJECT_NAME_NOT_FOUND))
    );
    assert_eq!(
        c.open("docs", Open::Read).map_err(|e| e.status()),
        Err(Some(status::FILE_IS_A_DIRECTORY))
    );
    assert_eq!(c.rename("hello.txt", "..", false), Err(Error::BadName));
    c.logoff();
    assert_eq!(c.transport().seen.last(), Some(&cmd::LOGOFF));
}

#[test]
fn a_spnego_session_logs_on_and_round_trips_files() {
    let (mut c, logon) = connect(Behaviour::default(), Signing::Auto).unwrap();
    assert_eq!(logon.dialect, msg::DIALECT_21);
    assert_eq!(logon.domain, DOMAIN, "taken from the challenge");
    assert!(logon.spnego && !logon.signing);
    work(&mut c);
    assert_eq!(c.transport().signed, 0);
}

#[test]
fn a_raw_ntlmssp_server_gets_raw_messages() {
    let how = Behaviour {
        spnego: false,
        timestamp: false,
        ..Behaviour::default()
    };
    let (mut c, logon) = connect(how, Signing::Auto).unwrap();
    assert!(!logon.spnego);
    work(&mut c);
}

#[test]
fn dialect_2_0_2_works_and_3_1_1_is_refused() {
    let how = Behaviour {
        dialect: msg::DIALECT_202,
        ..Behaviour::default()
    };
    let (mut c, logon) = connect(how, Signing::Auto).unwrap();
    assert_eq!(logon.dialect, msg::DIALECT_202);
    work(&mut c);
    let how = Behaviour {
        dialect: 0x0311,
        ..Behaviour::default()
    };
    assert_eq!(
        connect(how, Signing::Auto).err(),
        Some(Error::Dialect(0x0311))
    );
}

#[test]
fn a_server_that_requires_signing_gets_signed_requests() {
    let how = Behaviour {
        require_signing: true,
        ..Behaviour::default()
    };
    let (mut c, logon) = connect(how, Signing::Auto).unwrap();
    assert!(logon.signing);
    work(&mut c);
    assert!(c.transport().signed > 20, "every request after the logon");
}

#[test]
fn the_client_can_sign_when_the_server_does_not_ask() {
    for signing in [Signing::Always, Signing::Required] {
        let (mut c, logon) = connect(Behaviour::default(), signing).unwrap();
        assert!(logon.signing);
        work(&mut c);
        assert!(c.transport().signed > 20);
    }
}

#[test]
fn refusals_name_their_reason() {
    let required = Behaviour {
        require_signing: true,
        ..Behaviour::default()
    };
    assert_eq!(
        connect(required, Signing::Never).err(),
        Some(Error::Refused("the server requires signing"))
    );
    let guest = Behaviour {
        guest: true,
        ..Behaviour::default()
    };
    assert_eq!(
        connect(guest, Signing::Auto).err(),
        Some(Error::Refused("the server logged the user on as a guest"))
    );
    let encrypted = Behaviour {
        encrypt_session: true,
        ..Behaviour::default()
    };
    assert_eq!(
        connect(encrypted, Signing::Auto).err(),
        Some(Error::Refused("the server requires encryption (SMB3)"))
    );
    let share = Behaviour {
        encrypt_share: true,
        ..Behaviour::default()
    };
    let (mut c, _) = connect(share, Signing::Auto).unwrap();
    assert_eq!(
        c.tree_connect("h", "share"),
        Err(Error::Refused("the share requires encryption (SMB3)"))
    );
}

#[test]
fn a_wrong_password_or_domain_is_a_logon_failure() {
    let logon_failure = Some(Some(status::LOGON_FAILURE));
    let wrong = Client::connect(
        Server::new(Behaviour::default()),
        &config("nope", Signing::Auto),
    );
    assert_eq!(wrong.err().map(|e| e.status()), logon_failure);
    let mut cfg = config(PASSWORD, Signing::Auto);
    cfg.domain = Some("OTHER");
    let wrong = Client::connect(Server::new(Behaviour::default()), &cfg);
    assert_eq!(wrong.err().map(|e| e.status()), logon_failure);
    let mut cfg = config(PASSWORD, Signing::Auto);
    cfg.domain = Some(DOMAIN);
    assert!(Client::connect(Server::new(Behaviour::default()), &cfg).is_ok());
}

#[test]
fn an_unknown_share_is_bad_network_name() {
    let (mut c, _) = connect(Behaviour::default(), Signing::Auto).unwrap();
    assert_eq!(
        c.tree_connect("h", "nope").map_err(|e| e.status()),
        Err(Some(status::BAD_NETWORK_NAME))
    );
}

#[test]
fn a_tampered_or_missing_signature_is_refused() {
    for (tamper, unsign) in [
        (Some(cmd::READ), None),
        (None, Some(cmd::READ)),
        (Some(cmd::TREE_CONNECT), None),
    ] {
        let how = Behaviour {
            require_signing: true,
            tamper,
            unsign,
            ..Behaviour::default()
        };
        let (mut c, _) = connect(how, Signing::Auto).unwrap();
        let result = c
            .tree_connect("h", "share")
            .and_then(|_| read_all(&mut c, "hello.txt"));
        assert_eq!(
            result.err(),
            Some(Error::Signature),
            "{tamper:?} {unsign:?}"
        );
    }
    // A forged signature on a session that does not sign is refused too.
    let how = Behaviour {
        tamper: Some(cmd::READ),
        ..Behaviour::default()
    };
    let (mut c, _) = connect(how, Signing::Always).unwrap();
    c.tree_connect("h", "share").unwrap();
    assert_eq!(read_all(&mut c, "hello.txt").err(), Some(Error::Signature));
}

/// Records what the server sent, to replay damaged copies of it.
struct Recorder {
    server: Server,
    log: Vec<u8>,
}

impl Transport for Recorder {
    fn send(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.server.send(bytes)
    }

    fn recv(&mut self) -> Result<Vec<u8>, Error> {
        let bytes = self.server.recv()?;
        self.log.extend_from_slice(&bytes);
        Ok(bytes)
    }
}

struct Replay {
    bytes: Vec<u8>,
}

impl Transport for Replay {
    fn send(&mut self, _: &[u8]) -> Result<(), Error> {
        Ok(())
    }

    fn recv(&mut self) -> Result<Vec<u8>, Error> {
        let take = self.bytes.len().min(777);
        Ok(self.bytes.drain(..take).collect())
    }
}

#[test]
fn damaged_server_bytes_never_panic() {
    let how = Behaviour {
        require_signing: true,
        ..Behaviour::default()
    };
    let recorder = Recorder {
        server: Server::new(how),
        log: Vec::new(),
    };
    let cfg = config(PASSWORD, Signing::Auto);
    let (mut c, _) = Client::connect(recorder, &cfg).unwrap();
    c.tree_connect("h", "share").unwrap();
    c.list("").unwrap();
    let opened = c.open("hello.txt", Open::Read).unwrap();
    c.read(&opened.file_id, 0, 100).unwrap();
    let clean = c.transport().log.clone();
    fuzzkit::for_seeds("smbwire::replay", |_, rng| {
        let mut bytes = clean.clone();
        let flips = 1 + rng.below(8) as usize;
        rng.flip_bits(&mut bytes, flips);
        if rng.one_in(4) {
            bytes.truncate(rng.below(bytes.len() as u64) as usize);
        }
        if let Ok((mut c, _)) = Client::connect(Replay { bytes }, &cfg) {
            if c.tree_connect("h", "share").is_ok() && c.list("").is_ok() {
                if let Ok(opened) = c.open("hello.txt", Open::Read) {
                    let _ = c.read(&opened.file_id, 0, 100);
                }
            }
        }
    });
}
