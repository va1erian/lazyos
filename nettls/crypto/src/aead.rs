//! Record protection: AES-GCM and ChaCha20-Poly1305 for TLS 1.3 and 1.2.
//!
//! One generic implementation over the RustCrypto `AeadInPlace` trait, with
//! the record layouts of RFC 8446 §5.2 (1.3: inner content type appended,
//! nonce = IV xor sequence number) and RFC 5288 / RFC 7905 (1.2 GCM: 4-byte
//! salt + 8-byte explicit nonce carried in the record; 1.2 ChaCha20: nonce
//! as in 1.3). Tags are always checked before any plaintext is released.

use std::marker::PhantomData;

use aead::{AeadInPlace, KeyInit};
use aes_gcm::{Aes128Gcm, Aes256Gcm};
use chacha20poly1305::ChaCha20Poly1305;
use rustls::crypto::cipher::{
    make_tls12_aad, make_tls13_aad, AeadKey, InboundOpaqueMessage, InboundPlainMessage, Iv,
    KeyBlockShape, MessageDecrypter, MessageEncrypter, Nonce, OutboundOpaqueMessage,
    OutboundPlainMessage, PrefixedPayload, Tls12AeadAlgorithm, Tls13AeadAlgorithm,
    UnsupportedOperationError, NONCE_LEN,
};
use rustls::{ConnectionTrafficSecrets, ContentType, Error, ProtocolVersion};

const TAG_LEN: usize = 16;
const GCM_EXPLICIT_NONCE_LEN: usize = 8;
/// RFC 8446 §5.2 / RFC 5246 §6.2.3: the largest plaintext in a record.
const MAX_FRAGMENT_LEN: usize = 16384;

/// The AEADs, named by the `ConnectionTrafficSecrets` variant they export as.
pub trait Cipher:
    AeadInPlace<NonceSize = aead::consts::U12, TagSize = aead::consts::U16>
    + KeyInit
    + Send
    + Sync
    + 'static
{
    const KEY_LEN: usize;
    fn secrets(key: AeadKey, iv: Iv) -> ConnectionTrafficSecrets;
}

impl Cipher for Aes128Gcm {
    const KEY_LEN: usize = 16;
    fn secrets(key: AeadKey, iv: Iv) -> ConnectionTrafficSecrets {
        ConnectionTrafficSecrets::Aes128Gcm { key, iv }
    }
}

impl Cipher for Aes256Gcm {
    const KEY_LEN: usize = 32;
    fn secrets(key: AeadKey, iv: Iv) -> ConnectionTrafficSecrets {
        ConnectionTrafficSecrets::Aes256Gcm { key, iv }
    }
}

impl Cipher for ChaCha20Poly1305 {
    const KEY_LEN: usize = 32;
    fn secrets(key: AeadKey, iv: Iv) -> ConnectionTrafficSecrets {
        ConnectionTrafficSecrets::Chacha20Poly1305 { key, iv }
    }
}

fn cipher<C: Cipher>(key: &AeadKey) -> C {
    // rustls hands over exactly `key_len()` bytes (KEY_LEN below).
    C::new_from_slice(key.as_ref()).expect("AEAD key of the declared length")
}

// ---- TLS 1.3 ----

/// A TLS 1.3 AEAD over cipher `C`.
pub struct Tls13<C>(PhantomData<fn() -> C>);

pub static TLS13_AES_128_GCM: Tls13<Aes128Gcm> = Tls13(PhantomData);
pub static TLS13_AES_256_GCM: Tls13<Aes256Gcm> = Tls13(PhantomData);
pub static TLS13_CHACHA20_POLY1305: Tls13<ChaCha20Poly1305> = Tls13(PhantomData);

impl<C: Cipher> Tls13AeadAlgorithm for Tls13<C> {
    fn encrypter(&self, key: AeadKey, iv: Iv) -> Box<dyn MessageEncrypter> {
        Box::new(Tls13Crypter {
            cipher: cipher::<C>(&key),
            iv,
        })
    }

    fn decrypter(&self, key: AeadKey, iv: Iv) -> Box<dyn MessageDecrypter> {
        Box::new(Tls13Crypter {
            cipher: cipher::<C>(&key),
            iv,
        })
    }

    fn key_len(&self) -> usize {
        C::KEY_LEN
    }

    fn extract_keys(
        &self,
        key: AeadKey,
        iv: Iv,
    ) -> Result<ConnectionTrafficSecrets, UnsupportedOperationError> {
        Ok(C::secrets(key, iv))
    }
}

struct Tls13Crypter<C> {
    cipher: C,
    iv: Iv,
}

impl<C: Cipher> MessageEncrypter for Tls13Crypter<C> {
    fn encrypt(
        &mut self,
        msg: OutboundPlainMessage<'_>,
        seq: u64,
    ) -> Result<OutboundOpaqueMessage, Error> {
        let total = self.encrypted_payload_len(msg.payload.len());
        let mut payload = PrefixedPayload::with_capacity(total);
        payload.extend_from_chunks(&msg.payload);
        payload.extend_from_slice(&msg.typ.to_array());
        let nonce = Nonce::new(&self.iv, seq).0;
        let tag = self
            .cipher
            .encrypt_in_place_detached(&nonce.into(), &make_tls13_aad(total), payload.as_mut())
            .map_err(|_| Error::EncryptError)?;
        payload.extend_from_slice(&tag);
        Ok(OutboundOpaqueMessage::new(
            ContentType::ApplicationData,
            ProtocolVersion::TLSv1_2,
            payload,
        ))
    }

    fn encrypted_payload_len(&self, payload_len: usize) -> usize {
        payload_len + 1 + TAG_LEN
    }
}

impl<C: Cipher> MessageDecrypter for Tls13Crypter<C> {
    fn decrypt<'a>(
        &mut self,
        mut msg: InboundOpaqueMessage<'a>,
        seq: u64,
    ) -> Result<InboundPlainMessage<'a>, Error> {
        let total = msg.payload.len();
        if total < TAG_LEN {
            return Err(Error::DecryptError);
        }
        let nonce = Nonce::new(&self.iv, seq).0;
        let aad = make_tls13_aad(total);
        let body_len = total - TAG_LEN;
        let (body, tag) = msg.payload.split_at_mut(body_len);
        self.cipher
            .decrypt_in_place_detached(
                &nonce.into(),
                &aad,
                body,
                aead::generic_array::GenericArray::from_slice(tag),
            )
            .map_err(|_| Error::DecryptError)?;
        msg.payload.truncate(body_len);
        msg.into_tls13_unpadded_message()
    }
}

// ---- TLS 1.2 ----

/// AES-GCM for TLS 1.2 (RFC 5288): implicit 4-byte salt, explicit 8-byte
/// nonce at the front of each record.
pub struct Tls12Gcm<C>(PhantomData<fn() -> C>);

pub static TLS12_AES_128_GCM: Tls12Gcm<Aes128Gcm> = Tls12Gcm(PhantomData);
pub static TLS12_AES_256_GCM: Tls12Gcm<Aes256Gcm> = Tls12Gcm(PhantomData);

/// The full 12-byte GCM IV: the salt from the key block and the explicit
/// part rustls generated for us.
fn gcm_iv(salt: &[u8], explicit: &[u8]) -> Iv {
    let mut iv = [0u8; NONCE_LEN];
    iv[..4].copy_from_slice(salt);
    iv[4..].copy_from_slice(explicit);
    Iv::new(iv)
}

impl<C: Cipher> Tls12AeadAlgorithm for Tls12Gcm<C> {
    fn encrypter(&self, key: AeadKey, iv: &[u8], extra: &[u8]) -> Box<dyn MessageEncrypter> {
        Box::new(Tls12GcmEncrypter {
            cipher: cipher::<C>(&key),
            iv: gcm_iv(iv, extra),
        })
    }

    fn decrypter(&self, key: AeadKey, iv: &[u8]) -> Box<dyn MessageDecrypter> {
        let mut salt = [0u8; 4];
        salt.copy_from_slice(iv);
        Box::new(Tls12GcmDecrypter {
            cipher: cipher::<C>(&key),
            salt,
        })
    }

    fn key_block_shape(&self) -> KeyBlockShape {
        KeyBlockShape {
            enc_key_len: C::KEY_LEN,
            fixed_iv_len: 4,
            explicit_nonce_len: 8,
        }
    }

    fn extract_keys(
        &self,
        key: AeadKey,
        iv: &[u8],
        explicit: &[u8],
    ) -> Result<ConnectionTrafficSecrets, UnsupportedOperationError> {
        Ok(C::secrets(key, gcm_iv(iv, explicit)))
    }
}

struct Tls12GcmEncrypter<C> {
    cipher: C,
    iv: Iv,
}

impl<C: Cipher> MessageEncrypter for Tls12GcmEncrypter<C> {
    fn encrypt(
        &mut self,
        msg: OutboundPlainMessage<'_>,
        seq: u64,
    ) -> Result<OutboundOpaqueMessage, Error> {
        let mut payload =
            PrefixedPayload::with_capacity(self.encrypted_payload_len(msg.payload.len()));
        // The nonce is IV xor seq; its last 8 bytes travel in the record.
        let nonce = Nonce::new(&self.iv, seq).0;
        let aad = make_tls12_aad(seq, msg.typ, msg.version, msg.payload.len());
        payload.extend_from_slice(&nonce[4..]);
        payload.extend_from_chunks(&msg.payload);
        let tag = self
            .cipher
            .encrypt_in_place_detached(
                &nonce.into(),
                &aad,
                &mut payload.as_mut()[GCM_EXPLICIT_NONCE_LEN..],
            )
            .map_err(|_| Error::EncryptError)?;
        payload.extend_from_slice(&tag);
        Ok(OutboundOpaqueMessage::new(msg.typ, msg.version, payload))
    }

    fn encrypted_payload_len(&self, payload_len: usize) -> usize {
        payload_len + GCM_EXPLICIT_NONCE_LEN + TAG_LEN
    }
}

struct Tls12GcmDecrypter<C> {
    cipher: C,
    salt: [u8; 4],
}

impl<C: Cipher> MessageDecrypter for Tls12GcmDecrypter<C> {
    fn decrypt<'a>(
        &mut self,
        mut msg: InboundOpaqueMessage<'a>,
        seq: u64,
    ) -> Result<InboundPlainMessage<'a>, Error> {
        let total = msg.payload.len();
        if total < GCM_EXPLICIT_NONCE_LEN + TAG_LEN {
            return Err(Error::DecryptError);
        }
        let mut nonce = [0u8; NONCE_LEN];
        nonce[..4].copy_from_slice(&self.salt);
        nonce[4..].copy_from_slice(&msg.payload[..GCM_EXPLICIT_NONCE_LEN]);
        let plain_len = total - GCM_EXPLICIT_NONCE_LEN - TAG_LEN;
        let aad = make_tls12_aad(seq, msg.typ, msg.version, plain_len);
        let (head, tag) = msg.payload.split_at_mut(total - TAG_LEN);
        self.cipher
            .decrypt_in_place_detached(
                &nonce.into(),
                &aad,
                &mut head[GCM_EXPLICIT_NONCE_LEN..],
                aead::generic_array::GenericArray::from_slice(tag),
            )
            .map_err(|_| Error::DecryptError)?;
        if plain_len > MAX_FRAGMENT_LEN {
            return Err(Error::PeerSentOversizedRecord);
        }
        // Drop the explicit nonce: move the plaintext to the front.
        msg.payload.copy_within(
            GCM_EXPLICIT_NONCE_LEN..GCM_EXPLICIT_NONCE_LEN + plain_len,
            0,
        );
        msg.payload.truncate(plain_len);
        Ok(msg.into_plain_message())
    }
}

/// ChaCha20-Poly1305 for TLS 1.2 (RFC 7905): the 1.3-style nonce, no
/// explicit part.
pub struct Tls12ChaCha;

pub static TLS12_CHACHA20_POLY1305: Tls12ChaCha = Tls12ChaCha;

impl Tls12AeadAlgorithm for Tls12ChaCha {
    fn encrypter(&self, key: AeadKey, iv: &[u8], _: &[u8]) -> Box<dyn MessageEncrypter> {
        Box::new(Tls12ChaChaCrypter {
            cipher: cipher::<ChaCha20Poly1305>(&key),
            iv: Iv::copy(iv),
        })
    }

    fn decrypter(&self, key: AeadKey, iv: &[u8]) -> Box<dyn MessageDecrypter> {
        Box::new(Tls12ChaChaCrypter {
            cipher: cipher::<ChaCha20Poly1305>(&key),
            iv: Iv::copy(iv),
        })
    }

    fn key_block_shape(&self) -> KeyBlockShape {
        KeyBlockShape {
            enc_key_len: 32,
            fixed_iv_len: 12,
            explicit_nonce_len: 0,
        }
    }

    fn extract_keys(
        &self,
        key: AeadKey,
        iv: &[u8],
        _: &[u8],
    ) -> Result<ConnectionTrafficSecrets, UnsupportedOperationError> {
        Ok(ConnectionTrafficSecrets::Chacha20Poly1305 {
            key,
            iv: Iv::copy(iv),
        })
    }
}

struct Tls12ChaChaCrypter {
    cipher: ChaCha20Poly1305,
    iv: Iv,
}

impl MessageEncrypter for Tls12ChaChaCrypter {
    fn encrypt(
        &mut self,
        msg: OutboundPlainMessage<'_>,
        seq: u64,
    ) -> Result<OutboundOpaqueMessage, Error> {
        let mut payload =
            PrefixedPayload::with_capacity(self.encrypted_payload_len(msg.payload.len()));
        let nonce = Nonce::new(&self.iv, seq).0;
        let aad = make_tls12_aad(seq, msg.typ, msg.version, msg.payload.len());
        payload.extend_from_chunks(&msg.payload);
        let tag = self
            .cipher
            .encrypt_in_place_detached(&nonce.into(), &aad, payload.as_mut())
            .map_err(|_| Error::EncryptError)?;
        payload.extend_from_slice(&tag);
        Ok(OutboundOpaqueMessage::new(msg.typ, msg.version, payload))
    }

    fn encrypted_payload_len(&self, payload_len: usize) -> usize {
        payload_len + TAG_LEN
    }
}

impl MessageDecrypter for Tls12ChaChaCrypter {
    fn decrypt<'a>(
        &mut self,
        mut msg: InboundOpaqueMessage<'a>,
        seq: u64,
    ) -> Result<InboundPlainMessage<'a>, Error> {
        let total = msg.payload.len();
        if total < TAG_LEN {
            return Err(Error::DecryptError);
        }
        let plain_len = total - TAG_LEN;
        let nonce = Nonce::new(&self.iv, seq).0;
        let aad = make_tls12_aad(seq, msg.typ, msg.version, plain_len);
        let (body, tag) = msg.payload.split_at_mut(plain_len);
        self.cipher
            .decrypt_in_place_detached(
                &nonce.into(),
                &aad,
                body,
                aead::generic_array::GenericArray::from_slice(tag),
            )
            .map_err(|_| Error::DecryptError)?;
        if plain_len > MAX_FRAGMENT_LEN {
            return Err(Error::PeerSentOversizedRecord);
        }
        msg.payload.truncate(plain_len);
        Ok(msg.into_plain_message())
    }
}
