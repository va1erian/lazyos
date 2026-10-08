//! An in-memory SMB 2.1 server for the client's tests: it verifies the NTLMv2
//! proof and the client's signatures, keeps files in a map, and can be told to
//! misbehave (guest logon, encryption, required signing, a tampered
//! signature, a raw or SPNEGO logon).

use std::collections::{BTreeMap, BTreeSet};
use std::string::String;
use std::vec;
use std::vec::Vec;

use crate::client::Transport;
use crate::crypto::{hmac_md5, ntowfv2, smb2_signature, utf16le};
use crate::frame::{self, FrameReader};
use crate::header::{command as cmd, Header, FLAG_RESPONSE, FLAG_SIGNED, SIGNATURE_AT, SIZE};
use crate::msg;
use crate::ntlm::from_utf16le;
use crate::status::*;
use crate::{le16, le32, le64, Error};

pub const USER: &str = "chaton";
pub const PASSWORD: &str = "s3cret pass";
pub const DOMAIN: &str = "LAZYNAS";
const SESSION: u64 = 0x0000_4000_0000_0011;
const TREE: u32 = 7;
const CHALLENGE: [u8; 8] = [1, 2, 3, 4, 5, 6, 7, 8];

/// How the server behaves.
#[derive(Clone)]
pub struct Behaviour {
    pub dialect: u16,
    pub spnego: bool,
    pub require_signing: bool,
    pub guest: bool,
    pub encrypt_session: bool,
    pub encrypt_share: bool,
    /// Corrupt the signature of the response to this command.
    pub tamper: Option<u16>,
    /// Strip the signature (and flag) from the response to this command.
    pub unsign: Option<u16>,
    pub timestamp: bool,
    pub max_io: u32,
    /// Answer every QUERY_DIRECTORY with only `.` and `..`, never ending.
    pub endless_listing: bool,
}

impl Default for Behaviour {
    fn default() -> Behaviour {
        Behaviour {
            dialect: msg::DIALECT_21,
            spnego: true,
            require_signing: false,
            guest: false,
            encrypt_session: false,
            encrypt_share: false,
            tamper: None,
            unsign: None,
            timestamp: true,
            max_io: 65536,
            endless_listing: false,
        }
    }
}

pub struct Server {
    pub how: Behaviour,
    pub files: BTreeMap<String, Vec<u8>>,
    pub dirs: BTreeSet<String>,
    /// Commands seen, in order.
    pub seen: Vec<u16>,
    /// Requests that arrived signed (with a good signature).
    pub signed: usize,
    key: Option<[u8; 16]>,
    sign: bool,
    handles: BTreeMap<u64, (String, bool)>,
    next_handle: u64,
    out: Vec<u8>,
    reader: FrameReader,
}

impl Server {
    pub fn new(how: Behaviour) -> Server {
        let mut files = BTreeMap::new();
        files.insert(
            String::from("hello.txt"),
            b"hello from the server\n".to_vec(),
        );
        files.insert(String::from("docs/readme.md"), b"# read me\n".to_vec());
        let mut dirs = BTreeSet::new();
        dirs.insert(String::new());
        dirs.insert(String::from("docs"));
        Server {
            how,
            files,
            dirs,
            seen: Vec::new(),
            signed: 0,
            key: None,
            sign: false,
            handles: BTreeMap::new(),
            next_handle: 1,
            out: Vec::new(),
            reader: FrameReader::new(),
        }
    }

    fn reply(&mut self, request: &Header, status: u32, body: &[u8]) {
        let mut h = Header {
            status,
            command: request.command,
            credits: 32,
            flags: FLAG_RESPONSE,
            message_id: request.message_id,
            tree_id: request.tree_id,
            session_id: if request.command == cmd::NEGOTIATE {
                0
            } else {
                SESSION
            },
            ..Header::default()
        };
        let sign = self.key.is_some()
            && self.sign
            && self.how.unsign != Some(request.command)
            && (request.command != cmd::SESSION_SETUP || status == SUCCESS);
        if sign {
            h.flags |= FLAG_SIGNED;
        }
        let mut message = Vec::new();
        h.encode(&mut message);
        if status != SUCCESS && body.is_empty() {
            message.extend_from_slice(&[9, 0, 0, 0, 0, 0, 0, 0, 0]);
        } else {
            message.extend_from_slice(body);
        }
        if let (true, Some(key)) = (sign, self.key) {
            let mut signature = smb2_signature(&key, &message);
            if self.how.tamper == Some(request.command) {
                signature[0] ^= 1;
            }
            message[SIGNATURE_AT..SIZE].copy_from_slice(&signature);
        }
        self.out
            .extend_from_slice(&frame::encode(&message).unwrap());
    }

    fn handle(&mut self, message: &[u8]) {
        let h = Header::parse(message).expect("client header");
        self.seen.push(h.command);
        if h.is_signed() {
            let key = self.key.expect("signed before a key");
            let mut copy = message.to_vec();
            copy[SIGNATURE_AT..SIZE].fill(0);
            assert_eq!(smb2_signature(&key, &copy), h.signature, "client signature");
            self.signed += 1;
            self.sign = true;
        } else if self.sign && h.command != cmd::SESSION_SETUP {
            return self.reply(&h, ACCESS_DENIED, &[]);
        }
        let body = &message[SIZE..];
        match h.command {
            cmd::NEGOTIATE => self.negotiate(&h),
            cmd::SESSION_SETUP => self.session_setup(&h, message, body),
            cmd::TREE_CONNECT => self.tree_connect(&h, message, body),
            cmd::CREATE => self.create(&h, message, body),
            cmd::CLOSE => {
                let id = le64(body, 8).unwrap();
                if let Some((path, true)) = self.handles.remove(&id) {
                    self.files.remove(&path);
                    self.dirs.remove(&path);
                }
                let mut b = vec![0u8; 60];
                b[0] = 60;
                self.reply(&h, SUCCESS, &b);
            }
            cmd::READ => self.read(&h, body),
            cmd::WRITE => self.write(&h, message, body),
            cmd::FLUSH => self.reply(&h, SUCCESS, &[4, 0, 0, 0]),
            cmd::QUERY_DIRECTORY => self.query_directory(&h, body),
            cmd::QUERY_INFO => self.query_info(&h, body),
            cmd::SET_INFO => self.set_info(&h, message, body),
            cmd::TREE_DISCONNECT | cmd::LOGOFF => self.reply(&h, SUCCESS, &[4, 0, 0, 0]),
            _ => self.reply(&h, NOT_SUPPORTED, &[]),
        }
    }

    fn negotiate(&mut self, h: &Header) {
        let mut b = vec![0u8; 64];
        b[0] = 65;
        let mode = msg::SIGNING_ENABLED
            | if self.how.require_signing {
                msg::SIGNING_REQUIRED
            } else {
                0
            };
        b[2..4].copy_from_slice(&mode.to_le_bytes());
        b[4..6].copy_from_slice(&self.how.dialect.to_le_bytes());
        for at in [28, 32, 36] {
            b[at..at + 4].copy_from_slice(&self.how.max_io.to_le_bytes());
        }
        let hint = if self.how.spnego {
            crate::spnego::wrap_init(&[])
        } else {
            Vec::new()
        };
        b[56..58].copy_from_slice(&128u16.to_le_bytes());
        b[58..60].copy_from_slice(&(hint.len() as u16).to_le_bytes());
        b.extend_from_slice(&hint);
        self.sign = false;
        self.reply(h, SUCCESS, &b);
    }

    fn challenge(&self) -> Vec<u8> {
        let mut info = Vec::new();
        let mut pair = |id: u16, value: &[u8]| {
            info.extend_from_slice(&id.to_le_bytes());
            info.extend_from_slice(&(value.len() as u16).to_le_bytes());
            info.extend_from_slice(value);
        };
        pair(2, &utf16le(DOMAIN));
        pair(1, &utf16le("SERVER"));
        if self.how.timestamp {
            pair(7, &0x01D9_0000_0000_0000u64.to_le_bytes());
        }
        pair(0, &[]);
        let mut m = Vec::from(&b"NTLMSSP\0"[..]);
        m.extend_from_slice(&2u32.to_le_bytes());
        m.extend_from_slice(&[0, 0, 0, 0, 56, 0, 0, 0]); // empty target name
        m.extend_from_slice(&crate::ntlm::CLIENT_FLAGS.to_le_bytes());
        m.extend_from_slice(&CHALLENGE);
        m.extend_from_slice(&[0; 8]);
        let len = info.len() as u16;
        m.extend_from_slice(&len.to_le_bytes());
        m.extend_from_slice(&len.to_le_bytes());
        m.extend_from_slice(&56u32.to_le_bytes());
        m.extend_from_slice(&[6, 1, 0, 0, 0, 0, 0, 15]);
        m.extend_from_slice(&info);
        m
    }

    fn session_setup(&mut self, h: &Header, message: &[u8], body: &[u8]) {
        let offset = le16(body, 12).unwrap() as usize;
        let len = le16(body, 14).unwrap() as usize;
        let token = &message[offset..offset + len];
        // The NTLM message is the last thing in either wrapping.
        let at = token
            .windows(8)
            .position(|w| w == b"NTLMSSP\0")
            .expect("NTLMSSP");
        let ntlm = &token[at..];
        assert_eq!(
            self.how.spnego,
            at != 0,
            "the client wraps as the server did"
        );
        let kind = le32(ntlm, 8).unwrap();
        let mut out = vec![9u8, 0, 0, 0, 72, 0, 0, 0];
        if kind == 1 {
            let challenge = self.challenge();
            let token = if self.how.spnego {
                spnego_resp(1, &challenge)
            } else {
                challenge
            };
            out[6..8].copy_from_slice(&(token.len() as u16).to_le_bytes());
            out.extend_from_slice(&token);
            return self.reply(h, MORE_PROCESSING_REQUIRED, &out);
        }
        let field = |at: usize| {
            let len = le16(ntlm, at).unwrap() as usize;
            let off = le32(ntlm, at + 4).unwrap() as usize;
            &ntlm[off..off + len]
        };
        let nt = field(20);
        let domain = from_utf16le(field(28)).unwrap();
        let user = from_utf16le(field(36)).unwrap();
        let key = ntowfv2(PASSWORD, &user, &domain);
        let proof = hmac_md5(&key, &[&CHALLENGE, &nt[16..]]);
        if user != USER || domain != DOMAIN || proof[..] != nt[..16] {
            return self.reply(h, LOGON_FAILURE, &[]);
        }
        self.key = Some(hmac_md5(&key, &[&proof]));
        self.sign = self.how.require_signing;
        let mut flags = 0u16;
        if self.how.guest {
            flags |= msg::SESSION_FLAG_IS_GUEST;
        }
        if self.how.encrypt_session {
            flags |= msg::SESSION_FLAG_ENCRYPT_DATA;
        }
        out[2..4].copy_from_slice(&flags.to_le_bytes());
        if self.how.spnego {
            let token = spnego_resp(0, &[]);
            out[6..8].copy_from_slice(&(token.len() as u16).to_le_bytes());
            out.extend_from_slice(&token);
        }
        self.reply(h, SUCCESS, &out);
    }

    fn tree_connect(&mut self, h: &Header, message: &[u8], body: &[u8]) {
        let offset = le16(body, 4).unwrap() as usize;
        let len = le16(body, 6).unwrap() as usize;
        let path = from_utf16le(&message[offset..offset + len]).unwrap();
        if !path.ends_with("\\share") {
            return self.reply(h, BAD_NETWORK_NAME, &[]);
        }
        let mut out = vec![16u8, 0, msg::SHARE_TYPE_DISK, 0];
        let flags = if self.how.encrypt_share {
            msg::SHAREFLAG_ENCRYPT_DATA
        } else {
            0
        };
        out.extend_from_slice(&flags.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0x001F_01FFu32.to_le_bytes());
        let mut header = h.clone();
        header.tree_id = TREE;
        self.reply(&header, SUCCESS, &out);
    }
}

/// A `NegTokenResp` with `state` and an optional token.
fn spnego_resp(state: u8, token: &[u8]) -> Vec<u8> {
    fn tlv(tag: u8, c: &[u8]) -> Vec<u8> {
        let mut out = vec![tag];
        if c.len() < 128 {
            out.push(c.len() as u8);
        } else {
            out.extend_from_slice(&[0x82, (c.len() >> 8) as u8, c.len() as u8]);
        }
        out.extend_from_slice(c);
        out
    }
    let mut seq = tlv(0xa0, &tlv(0x0a, &[state]));
    seq.extend(tlv(0xa1, &tlv(0x06, crate::spnego::NTLMSSP_OID)));
    if !token.is_empty() {
        seq.extend(tlv(0xa2, &tlv(0x04, token)));
    }
    tlv(0xa1, &tlv(0x30, &seq))
}

impl Transport for Server {
    fn send(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.reader.feed(bytes).expect("client frame");
        while let Some(message) = self.reader.next_frame() {
            self.handle(&message);
        }
        Ok(())
    }

    fn recv(&mut self) -> Result<Vec<u8>, Error> {
        // Hand the bytes back in uneven pieces, as TCP may.
        let take = self.out.len().min(1000);
        Ok(self.out.drain(..take).collect())
    }
}

#[path = "server_files.rs"]
mod files;
