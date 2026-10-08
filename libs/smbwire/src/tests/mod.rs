//! Host tests: published vectors for the crypto, the codecs against hostile
//! bytes, and whole sessions against the in-memory server in `server.rs`.

mod session;

use std::vec;
use std::vec::Vec;

use crate::crypto::{hmac_md5, nt_hash, ntowfv2, smb2_signature};
use crate::frame::{self, FrameReader, MAX_FRAME};
use crate::header::{Header, FLAG_ASYNC, FLAG_RESPONSE};
use crate::ntlm::{self, Challenge};
use crate::{name, spnego, Error};

fn hex(bytes: &[u8]) -> std::string::String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

/// `MS-NLMP` 4.2.4: User / Domain / Password, server challenge 0123456789abcdef,
/// client challenge aa..., time 0, target info Domain + Server.
#[test]
fn ntlmv2_matches_the_ms_nlmp_vectors() {
    assert_eq!(
        hex(&nt_hash("Password")),
        "a4f49c406510bdcab6824ee7c30fd852"
    );
    let key = ntowfv2("Password", "User", "Domain");
    assert_eq!(hex(&key), "0c868a403bfd7a93a3001ef22ef02e3f");
    let server: [u8; 8] = unhex("0123456789abcdef").try_into().unwrap();
    let client = [0xaa; 8];
    let info = unhex("02000c0044006f006d00610069006e0001000c0053006500720076006500720000000000");
    let (response, session) = ntlm::ntlmv2_response(&key, &server, &client, 0, &info);
    assert_eq!(hex(&response[..16]), "68cd0ab851e51c96aabc927bebef6a1c");
    assert_eq!(hex(&session), "8de40ccadbc14a82f15cb0ad0de95ca3");
    let lm = ntlm::lmv2_response(&key, &server, &client);
    assert_eq!(hex(&lm), "86c35097ac9cec102554764a57cccc19aaaaaaaaaaaaaaaa");
}

#[test]
fn the_user_name_is_upper_cased_and_the_domain_is_not() {
    assert_eq!(
        ntowfv2("pw", "chaton", "Dom"),
        ntowfv2("pw", "CHATON", "Dom")
    );
    assert_ne!(
        ntowfv2("pw", "chaton", "Dom"),
        ntowfv2("pw", "chaton", "DOM")
    );
}

/// Computed independently by `tools/smb/ntlm.py`.
#[test]
fn the_smb2_signature_matches_the_python_reference() {
    let key: [u8; 16] = core::array::from_fn(|i| i as u8);
    let mut message = vec![0xFE, b'S', b'M', b'B', 64, 0];
    message.extend((6..64).map(|i| (i * 7) as u8));
    message.extend_from_slice(b"payload");
    message[48..64].fill(0);
    assert_eq!(
        hex(&smb2_signature(&key, &message)),
        "67ce337e003df076edf7c61549c402d4"
    );
    assert_eq!(
        hex(&hmac_md5(
            b"key",
            &[b"The quick brown fox jumps over the lazy dog"]
        )),
        "80070713463e7749b90c2dc24911e275"
    );
}

#[test]
fn frames_reassemble_from_any_chunking_and_bound_their_length() {
    let message: Vec<u8> = (0..300u32).map(|i| i as u8).collect();
    let mut wire = frame::encode(&message).unwrap();
    wire.extend(frame::encode(&message[..64]).unwrap());
    for size in [1, 3, 64, 1000] {
        let mut reader = FrameReader::new();
        let mut got = Vec::new();
        for chunk in wire.chunks(size) {
            reader.feed(chunk).unwrap();
            while let Some(frame) = reader.next_frame() {
                got.push(frame);
            }
        }
        assert_eq!(got, vec![message.clone(), message[..64].to_vec()]);
        assert_eq!(reader.pending(), 0);
    }
    let too_long = (MAX_FRAME as u32 + 1).to_be_bytes();
    assert!(FrameReader::new()
        .feed(&[0, too_long[1], too_long[2], too_long[3]])
        .is_err());
    assert!(
        FrameReader::new().feed(&[0, 0, 0, 10]).is_err(),
        "shorter than a header"
    );
    assert!(
        FrameReader::new().feed(&[0x85, 0, 0, 0]).is_err(),
        "NetBIOS keep-alive"
    );
}

#[test]
fn headers_round_trip_and_reject_damage() {
    let mut h = Header::request(5, 42, 7, 0x1122_3344_5566_7788);
    h.credits = 32;
    h.signature = [9; 16];
    let mut bytes = Vec::new();
    h.encode(&mut bytes);
    assert_eq!(bytes.len(), 64);
    assert_eq!(Header::parse(&bytes).unwrap(), h);
    let mut async_header = h.clone();
    async_header.flags = FLAG_RESPONSE | FLAG_ASYNC;
    async_header.async_id = 99;
    async_header.tree_id = 0;
    bytes.clear();
    async_header.encode(&mut bytes);
    assert_eq!(Header::parse(&bytes).unwrap(), async_header);
    for damage in [0usize, 4] {
        let mut bad = bytes.clone();
        bad[damage] ^= 0xFF;
        assert!(Header::parse(&bad).is_err());
    }
    assert!(Header::parse(&bytes[..63]).is_err());
}

#[test]
fn paths_become_backslashes_and_hostile_ones_are_refused() {
    let utf16 = |s: &str| crate::crypto::utf16le(s);
    assert_eq!(
        name::to_smb("/docs/./readme.md").unwrap(),
        utf16("docs\\readme.md")
    );
    assert_eq!(name::to_smb("").unwrap(), Vec::<u8>::new());
    assert_eq!(name::to_smb("été/ü").unwrap(), utf16("été\\ü"));
    for bad in [
        "..",
        "a/../b",
        "a\\b",
        "file:stream",
        "*",
        "a?",
        "x\u{1}",
        "a|b",
    ] {
        assert_eq!(name::to_smb(bad), Err(Error::BadName), "{bad:?}");
    }
    assert!(name::unc("10.0.2.2", "share").is_ok());
    assert!(name::unc("chatonnas", "IPC$").is_ok());
    for (server, share) in [("a\\b", "s"), ("host", "a\\b"), ("", "s"), ("host", "")] {
        assert!(name::unc(server, share).is_err(), "{server:?} {share:?}");
    }
    assert_eq!(name::listed(&utf16("ok.txt")).as_deref(), Some("ok.txt"));
    let bidi = [
        "evil\u{202E}txt.exe",
        "zero\u{200B}width",
        "\u{FEFF}bom",
        "iso\u{2066}late",
    ];
    for bad in [".", "..", "a/b", "a\\b", "", "nul\0"]
        .into_iter()
        .chain(bidi)
    {
        assert_eq!(name::listed(&utf16(bad)), None, "{bad:?}");
    }
    assert_eq!(name::listed(&[0x00, 0xD8]), None, "an unpaired surrogate");
}

fn challenge_message(info: &[u8], flags: u32) -> Vec<u8> {
    let mut m = Vec::from(&b"NTLMSSP\0"[..]);
    m.extend_from_slice(&2u32.to_le_bytes());
    m.extend_from_slice(&[0, 0, 0, 0, 48, 0, 0, 0]);
    m.extend_from_slice(&flags.to_le_bytes());
    m.extend_from_slice(&[7; 8]);
    m.extend_from_slice(&[0; 8]);
    m.extend_from_slice(&(info.len() as u16).to_le_bytes());
    m.extend_from_slice(&(info.len() as u16).to_le_bytes());
    m.extend_from_slice(&48u32.to_le_bytes());
    m.extend_from_slice(info);
    m
}

#[test]
fn a_challenge_yields_its_domain_and_time_and_refuses_damage() {
    let info = unhex("02000c0044006f006d00610069006e000700080001020304050607080000000099");
    let c = Challenge::parse(&challenge_message(&info, ntlm::CLIENT_FLAGS)).unwrap();
    assert_eq!(c.nb_domain.as_deref(), Some("Domain"));
    assert_eq!(c.timestamp, Some(0x0807_0605_0403_0201));
    assert_eq!(c.server_challenge, [7; 8]);
    assert_eq!(
        c.target_info.len(),
        info.len() - 1,
        "echoed up to the terminator"
    );
    let no_info = ntlm::CLIENT_FLAGS & !ntlm::NEGOTIATE_TARGET_INFO;
    assert!(Challenge::parse(&challenge_message(&info, no_info)).is_err());
    assert!(
        Challenge::parse(&challenge_message(
            &info[..info.len() - 5],
            ntlm::CLIENT_FLAGS
        ))
        .is_err(),
        "no terminator"
    );
    let mut overlong = info.clone();
    overlong[2] = 0xFF;
    assert!(Challenge::parse(&challenge_message(&overlong, ntlm::CLIENT_FLAGS)).is_err());
    let full = challenge_message(&info, ntlm::CLIENT_FLAGS);
    for cut in 0..full.len() {
        let _ = Challenge::parse(&full[..cut]);
    }
}

#[test]
fn the_authenticate_message_lays_out_its_fields() {
    let info = unhex("02000c0044006f006d00610069006e0000000000");
    let c = Challenge::parse(&challenge_message(&info, ntlm::CLIENT_FLAGS)).unwrap();
    let who = ntlm::Credentials {
        user: "u",
        domain: "Domain",
        password: "p",
        workstation: "WS",
    };
    let auth = ntlm::authenticate(&c, &who, &[1; 8], 5);
    let m = &auth.message;
    let field = |at: usize| {
        let len = u16::from_le_bytes([m[at], m[at + 1]]) as usize;
        let off = u32::from_le_bytes(m[at + 4..at + 8].try_into().unwrap()) as usize;
        &m[off..off + len]
    };
    assert_eq!(field(36), crate::crypto::utf16le("u").as_slice());
    assert_eq!(field(28), crate::crypto::utf16le("Domain").as_slice());
    assert_eq!(field(12).len(), 24, "LMv2 without a server time");
    let nt = field(20);
    let key = ntowfv2("p", "u", "Domain");
    assert_eq!(hmac_md5(&key, &[&[7; 8], &nt[16..]]), nt[..16]);
    let flags = u32::from_le_bytes(m[60..64].try_into().unwrap());
    assert_eq!(flags & ntlm::NEGOTIATE_KEY_EXCH, 0, "no key exchange");
}

#[test]
fn spnego_round_trips_and_refuses_other_mechanisms() {
    let init = spnego::wrap_init(b"NTLMSSP\0x");
    assert_eq!(spnego::parse_hint(&init), Ok(spnego::Hint::Spnego));
    assert_eq!(spnego::parse_hint(&[]), Ok(spnego::Hint::None));
    let resp = spnego::wrap_resp(b"NTLMSSP\0abc");
    let reply = spnego::parse_reply(&resp).unwrap();
    assert_eq!(
        (reply.wrapped, reply.token),
        (true, Some(&b"NTLMSSP\0abc"[..]))
    );
    let raw = spnego::parse_reply(b"NTLMSSP\0raw").unwrap();
    assert!(!raw.wrapped);
    // A Kerberos-only hint (1.2.840.113554.1.2.2).
    let krb = [0x2a, 0x86, 0x48, 0x86, 0xf7, 0x12, 0x01, 0x02, 0x02];
    let mut hint = init.clone();
    let at = hint
        .windows(10)
        .position(|w| w == spnego::NTLMSSP_OID)
        .unwrap();
    hint[at + 2] = krb[2];
    assert!(spnego::parse_hint(&hint).is_err());
    for cut in 0..init.len() {
        let _ = spnego::parse_hint(&init[..cut]);
        let _ = spnego::parse_reply(&resp[..cut.min(resp.len())]);
    }
    // A long-form length that points past the data.
    assert!(spnego::parse_reply(&[0xa1, 0x84, 0xff, 0xff, 0xff, 0xff]).is_err());
    let big = spnego::wrap_resp(&[0x4e; 300]);
    assert_eq!(spnego::parse_reply(&big).unwrap().token.unwrap().len(), 300);
}

/// The checked-in cargo-fuzz seeds (`fuzz/gen_corpus.py`) replay here, and
/// the session seed reaches a listing: the seeds start libFuzzer deep in the
/// client, not at the frame header.
#[test]
fn the_fuzz_seeds_replay_and_reach_a_listing() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/seeds/smbwire");
    let mut seen = 0;
    for entry in std::fs::read_dir(&dir).expect("fuzz/seeds/smbwire exists") {
        crate::fuzz::run(&std::fs::read(entry.unwrap().path()).unwrap());
        seen += 1;
    }
    assert!(seen >= 5);
    let script = std::fs::read(dir.join("session")).unwrap();
    assert_eq!(
        crate::fuzz::listing_of(&script[1..]),
        Some(vec![std::string::String::from("a")])
    );
}

mod seeded {
    use fuzzkit::for_seeds;

    #[test]
    fn hostile_bytes_never_panic() {
        for_seeds("smbwire::fuzz", |_, rng| {
            let len = rng.range(0, 600) as usize;
            crate::fuzz::run(&rng.bytes(len));
        });
    }
}
