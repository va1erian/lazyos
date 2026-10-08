//! NTLMv2 (`MS-NLMP`): the NEGOTIATE (type 1) the client sends, the server's
//! CHALLENGE (type 2) parsed and bounded, and the AUTHENTICATE (type 3) with
//! the NTLMv2 response.
//!
//! Key exchange stays off (`NTLMSSP_NEGOTIATE_KEY_EXCH` clear), so the SMB
//! session key is the `SessionBaseKey` itself and no RC4 is needed. NTLMv1 and
//! LM are never sent. The AUTHENTICATE carries no MIC: a MIC obliges SPNEGO's
//! `mechListMIC` as well, which F2 leaves out (docs/smb-plan.md §4.3).

use alloc::string::String;
use alloc::vec::Vec;

use crate::crypto::{hmac_md5, ntowfv2, utf16le};
use crate::{le16, le32, le64, slice, Error};

pub const SIGNATURE: &[u8; 8] = b"NTLMSSP\0";

pub const NEGOTIATE_UNICODE: u32 = 0x0000_0001;
pub const REQUEST_TARGET: u32 = 0x0000_0004;
pub const NEGOTIATE_SIGN: u32 = 0x0000_0010;
pub const NEGOTIATE_NTLM: u32 = 0x0000_0200;
pub const NEGOTIATE_ALWAYS_SIGN: u32 = 0x0000_8000;
pub const NEGOTIATE_EXTENDED_SESSIONSECURITY: u32 = 0x0008_0000;
pub const NEGOTIATE_TARGET_INFO: u32 = 0x0080_0000;
pub const NEGOTIATE_VERSION: u32 = 0x0200_0000;
pub const NEGOTIATE_128: u32 = 0x2000_0000;
pub const NEGOTIATE_KEY_EXCH: u32 = 0x4000_0000;
pub const NEGOTIATE_56: u32 = 0x8000_0000;

/// Everything this client supports; key exchange deliberately absent.
pub const CLIENT_FLAGS: u32 = NEGOTIATE_UNICODE
    | REQUEST_TARGET
    | NEGOTIATE_SIGN
    | NEGOTIATE_NTLM
    | NEGOTIATE_ALWAYS_SIGN
    | NEGOTIATE_EXTENDED_SESSIONSECURITY
    | NEGOTIATE_TARGET_INFO
    | NEGOTIATE_VERSION
    | NEGOTIATE_128
    | NEGOTIATE_56;

/// The version field the client sends: 6.1, NTLM revision 15.
const VERSION: [u8; 8] = [6, 1, 0, 0, 0, 0, 0, 15];

/// `AvId`s read from the target info.
pub const AV_EOL: u16 = 0;
pub const AV_NB_COMPUTER: u16 = 1;
pub const AV_NB_DOMAIN: u16 = 2;
pub const AV_DNS_DOMAIN: u16 = 4;
pub const AV_TIMESTAMP: u16 = 7;
/// Most pairs a target info may carry.
const MAX_AV_PAIRS: usize = 32;
/// Longest string field accepted from the server, bytes.
const MAX_FIELD: usize = 1024;

/// The NEGOTIATE message.
pub fn negotiate(flags: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(40);
    out.extend_from_slice(SIGNATURE);
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&flags.to_le_bytes());
    // Empty domain and workstation fields: offsets past the fixed part.
    for _ in 0..2 {
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&40u32.to_le_bytes());
    }
    out.extend_from_slice(&VERSION);
    out
}

/// A parsed CHALLENGE message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Challenge {
    pub flags: u32,
    pub server_challenge: [u8; 8],
    /// The target info, verbatim (echoed inside the NTLMv2 blob).
    pub target_info: Vec<u8>,
    /// `MsvAvNbDomainName`, decoded.
    pub nb_domain: Option<String>,
    /// `MsvAvNbComputerName`, decoded.
    pub nb_computer: Option<String>,
    /// `MsvAvTimestamp`, the server's FILETIME.
    pub timestamp: Option<u64>,
}

/// Decode UTF-16LE, refusing odd lengths and unpaired surrogates.
pub fn from_utf16le(bytes: &[u8]) -> Option<String> {
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let units = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|p| u16::from_le_bytes(*p));
    char::decode_utf16(units)
        .collect::<Result<String, _>>()
        .ok()
}

/// The `(len, offset)` of a security buffer field at `at`.
fn field(message: &[u8], at: usize) -> Option<&[u8]> {
    let len = le16(message, at)? as usize;
    let offset = le32(message, at + 4)? as usize;
    if len > MAX_FIELD * 4 {
        return None;
    }
    slice(message, offset, len)
}

impl Challenge {
    pub fn parse(message: &[u8]) -> Result<Challenge, Error> {
        let bad = |what| Error::Malformed(what);
        if message.get(..8) != Some(SIGNATURE.as_slice()) || le32(message, 8) != Some(2) {
            return Err(bad("NTLM challenge header"));
        }
        let flags = le32(message, 20).ok_or(bad("NTLM challenge flags"))?;
        let mut server_challenge = [0u8; 8];
        server_challenge.copy_from_slice(slice(message, 24, 8).ok_or(bad("NTLM challenge"))?);
        if flags & NEGOTIATE_TARGET_INFO == 0 {
            return Err(bad("NTLM challenge without target info (NTLMv2 needs it)"));
        }
        let target_info = field(message, 40).ok_or(bad("NTLM target info"))?.to_vec();
        let mut challenge = Challenge {
            flags,
            server_challenge,
            target_info,
            nb_domain: None,
            nb_computer: None,
            timestamp: None,
        };
        challenge.read_pairs()?;
        Ok(challenge)
    }

    fn read_pairs(&mut self) -> Result<(), Error> {
        let info = &self.target_info;
        let (mut at, mut ended) = (0usize, false);
        for _ in 0..MAX_AV_PAIRS {
            let id = le16(info, at).ok_or(Error::Malformed("NTLM AV pair"))?;
            let len = le16(info, at + 2).ok_or(Error::Malformed("NTLM AV pair"))? as usize;
            let value = slice(info, at + 4, len).ok_or(Error::Malformed("NTLM AV pair length"))?;
            at += 4 + len;
            match id {
                AV_EOL => {
                    ended = true;
                    break;
                }
                AV_NB_DOMAIN => self.nb_domain = from_utf16le(value),
                AV_NB_COMPUTER => self.nb_computer = from_utf16le(value),
                AV_TIMESTAMP if len == 8 => self.timestamp = le64(value, 0),
                _ => {}
            }
        }
        if !ended {
            return Err(Error::Malformed("NTLM target info without an end"));
        }
        // Echo exactly what was read, up to the terminator.
        self.target_info.truncate(at);
        Ok(())
    }
}

/// What the AUTHENTICATE needs from the caller.
pub struct Credentials<'a> {
    pub user: &'a str,
    pub domain: &'a str,
    pub password: &'a str,
    pub workstation: &'a str,
}

/// The AUTHENTICATE message and the session key it establishes.
pub struct Authenticate {
    pub message: Vec<u8>,
    pub session_key: [u8; 16],
}

/// The NTLMv2 `NtChallengeResponse` (NTProofStr then the blob) and the
/// `SessionBaseKey`.
pub fn ntlmv2_response(
    response_key: &[u8; 16],
    server_challenge: &[u8; 8],
    client_challenge: &[u8; 8],
    time: u64,
    target_info: &[u8],
) -> (Vec<u8>, [u8; 16]) {
    let mut blob = Vec::with_capacity(32 + target_info.len());
    blob.extend_from_slice(&[1, 1, 0, 0, 0, 0, 0, 0]);
    blob.extend_from_slice(&time.to_le_bytes());
    blob.extend_from_slice(client_challenge);
    blob.extend_from_slice(&[0; 4]);
    blob.extend_from_slice(target_info);
    blob.extend_from_slice(&[0; 4]);
    let proof = hmac_md5(response_key, &[server_challenge, &blob]);
    let key = hmac_md5(response_key, &[&proof]);
    let mut response = Vec::from(proof);
    response.extend_from_slice(&blob);
    (response, key)
}

/// The LMv2 response: `HMAC-MD5(key, server || client) || client`.
pub fn lmv2_response(
    response_key: &[u8; 16],
    server_challenge: &[u8; 8],
    client_challenge: &[u8; 8],
) -> Vec<u8> {
    let mut out = Vec::from(hmac_md5(
        response_key,
        &[server_challenge, client_challenge],
    ));
    out.extend_from_slice(client_challenge);
    out
}

/// Build the AUTHENTICATE for `challenge`. `time` is the client's FILETIME,
/// used only when the server sent no `MsvAvTimestamp`; then the LMv2 response
/// is sent too, otherwise 24 zero bytes stand in for it (`MS-NLMP` 3.1.5.1.2).
pub fn authenticate(
    challenge: &Challenge,
    who: &Credentials,
    client_challenge: &[u8; 8],
    time: u64,
) -> Authenticate {
    let key = ntowfv2(who.password, who.user, who.domain);
    let stamp = challenge.timestamp.unwrap_or(time);
    let (nt, session_key) = ntlmv2_response(
        &key,
        &challenge.server_challenge,
        client_challenge,
        stamp,
        &challenge.target_info,
    );
    let lm = match challenge.timestamp {
        Some(_) => Vec::from([0u8; 24]),
        None => lmv2_response(&key, &challenge.server_challenge, client_challenge),
    };
    let flags = (challenge.flags & CLIENT_FLAGS) | NEGOTIATE_UNICODE;
    let fields: [Vec<u8>; 6] = [
        lm,
        nt,
        utf16le(who.domain),
        utf16le(who.user),
        utf16le(who.workstation),
        Vec::new(),
    ];
    const FIXED: usize = 64 + 8 + 16;
    let mut head = Vec::with_capacity(FIXED);
    head.extend_from_slice(SIGNATURE);
    head.extend_from_slice(&3u32.to_le_bytes());
    let mut payload = Vec::new();
    // The payload order differs from the field order: strings, then the
    // responses, as Windows sends it. Any order is valid.
    let order = [2usize, 3, 4, 0, 1, 5];
    let mut offsets = [0usize; 6];
    for index in order {
        offsets[index] = FIXED + payload.len();
        payload.extend_from_slice(&fields[index]);
    }
    for (index, value) in fields.iter().enumerate() {
        let len = value.len() as u16;
        head.extend_from_slice(&len.to_le_bytes());
        head.extend_from_slice(&len.to_le_bytes());
        head.extend_from_slice(&(offsets[index] as u32).to_le_bytes());
    }
    head.extend_from_slice(&flags.to_le_bytes());
    head.extend_from_slice(&VERSION);
    head.extend_from_slice(&[0; 16]);
    head.extend_from_slice(&payload);
    Authenticate {
        message: head,
        session_key,
    }
}
