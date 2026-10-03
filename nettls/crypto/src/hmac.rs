//! HMAC-SHA-256 and HMAC-SHA-384 as rustls HMACs (rustls derives HKDF and
//! the TLS 1.2 PRF from these).

use hmac::{Mac, SimpleHmac};
use rustls::crypto::hmac as rhmac;
use sha2::Digest;

pub struct Hmac<D>(std::marker::PhantomData<fn() -> D>);

pub static HMAC_SHA256: Hmac<sha2::Sha256> = Hmac(std::marker::PhantomData);
pub static HMAC_SHA384: Hmac<sha2::Sha384> = Hmac(std::marker::PhantomData);

impl<D> rhmac::Hmac for Hmac<D>
where
    D: Digest + digest_bounds::BlockSized + Clone + Send + Sync + 'static,
{
    fn with_key(&self, key: &[u8]) -> Box<dyn rhmac::Key> {
        // HMAC accepts keys of any length (long ones are hashed first).
        Box::new(Key(
            SimpleHmac::<D>::new_from_slice(key).expect("HMAC takes any key length")
        ))
    }

    fn hash_output_len(&self) -> usize {
        <D as Digest>::output_size()
    }
}

struct Key<D: Digest + digest_bounds::BlockSized>(SimpleHmac<D>);

impl<D> rhmac::Key for Key<D>
where
    D: Digest + digest_bounds::BlockSized + Clone + Send + Sync + 'static,
{
    fn sign_concat(&self, first: &[u8], middle: &[&[u8]], last: &[u8]) -> rhmac::Tag {
        let mut mac = self.0.clone();
        mac.update(first);
        for part in middle {
            mac.update(part);
        }
        mac.update(last);
        rhmac::Tag::new(&mac.finalize().into_bytes())
    }

    fn tag_len(&self) -> usize {
        <D as Digest>::output_size()
    }
}

/// `SimpleHmac` needs the digest's block size; this names that bound once.
mod digest_bounds {
    pub trait BlockSized: sha2::digest::core_api::BlockSizeUser {}
    impl<T: sha2::digest::core_api::BlockSizeUser> BlockSized for T {}
}

#[cfg(test)]
mod tests {
    use rustls::crypto::hmac::Hmac as _;

    #[test]
    fn rfc4231_case_2() {
        let key = super::HMAC_SHA256.with_key(b"Jefe");
        let tag = key.sign(&[b"what do ya want ", b"for nothing?"]);
        assert_eq!(tag.as_ref()[..4], [0x5b, 0xdc, 0xc1, 0x46]);
        let key = super::HMAC_SHA384.with_key(b"Jefe");
        let tag = key.sign(&[b"what do ya want for nothing?"]);
        assert_eq!(tag.as_ref()[..4], [0xaf, 0x45, 0xd2, 0xe3]);
    }
}
