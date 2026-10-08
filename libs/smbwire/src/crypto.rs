//! The primitives NTLMv2 and SMB 2.x signing need (docs/smb-plan.md §5).
//!
//! ```text
//! NT_hash        = MD4(UTF-16LE(password))
//! NTOWFv2        = HMAC-MD5(NT_hash, UTF-16LE(uppercase(user) + domain))
//! NTProofStr     = HMAC-MD5(NTOWFv2, server_challenge || client_blob)
//! SessionBaseKey = HMAC-MD5(NTOWFv2, NTProofStr)
//! signature      = HMAC-SHA256(SessionKey, message with a zero signature)[..16]
//! ```

use alloc::vec::Vec;

use hmac::{Hmac, Mac};
use md4::{Digest, Md4};
use md5::Md5;
use sha2::Sha256;

/// UTF-16LE of `text`, the encoding of every NTLM and SMB2 string.
pub fn utf16le(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

/// `MD4(UTF-16LE(password))`.
pub fn nt_hash(password: &str) -> [u8; 16] {
    Md4::digest(utf16le(password)).into()
}

/// `HMAC-MD5(key, parts...)`.
pub fn hmac_md5(key: &[u8], parts: &[&[u8]]) -> [u8; 16] {
    // An HMAC key may have any length: `new_from_slice` cannot fail.
    let mut mac = <Hmac<Md5> as Mac>::new_from_slice(key).expect("any HMAC key length");
    for part in parts {
        mac.update(part);
    }
    mac.finalize().into_bytes().into()
}

/// `NTOWFv2`: the NTLMv2 response key of `user` in `domain`. The user name is
/// upper-cased (as `MS-NLMP` specifies, in Unicode); the domain is not.
pub fn ntowfv2(password: &str, user: &str, domain: &str) -> [u8; 16] {
    let mut identity = alloc::string::String::new();
    for c in user.chars() {
        identity.extend(c.to_uppercase());
    }
    identity.push_str(domain);
    hmac_md5(&nt_hash(password), &[&utf16le(&identity)])
}

/// The first 16 bytes of `HMAC-SHA256(key, message)`: the SMB 2.0.2/2.1
/// signature. `message` must already have its signature field zeroed.
pub fn smb2_signature(key: &[u8; 16], message: &[u8]) -> [u8; 16] {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("any HMAC key length");
    mac.update(message);
    let full = mac.finalize().into_bytes();
    let mut out = [0u8; 16];
    out.copy_from_slice(&full[..16]);
    out
}

/// Compare two MACs without an early exit.
pub fn equal(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}
