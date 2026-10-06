//! Fuzzing of the Messenger wire decoding behind `accountsd` and `keyd`
//! (issue #626).
//!
//! `accountsd` (a binary, so not host-testable) turns a request into a call of
//! `messenger_generated::os_lazy_accounts_v1::decode_*_args` on the parcel
//! body, and `keyd` does the same with `os_lazy_keyd_v1`. Both bodies come
//! from any process that may reach the service (lookups are open to every
//! caller), so the decoders are the untrusted-input surface of the account
//! stack. [`run`] is the shared entry point for the cargo-fuzz target
//! (`fuzz/fuzz_targets/accountwire.rs`) and the seeded tests: it feeds the
//! bytes to the parcel envelope decoder and to every request and reply body
//! decoder of both interfaces. None may panic, and a value that decodes must
//! re-encode and decode back to itself.
//!
//! The account database (`libs/passwd`) has its own entry point
//! (`passwd::fuzz::run`); the dispatch in `accountsd` itself only picks a
//! table row from the decoded fields.

use libmessenger::Parcel;
use messenger_generated::{os_lazy_accounts_v1 as accounts, os_lazy_keyd_v1 as keyd};

/// Decode `body` with `decode`; when it is a value, check it survives a round
/// trip through `encode`.
fn round_trip<T, E, D, N>(body: &[u8], decode: D, encode: N)
where
    T: PartialEq + core::fmt::Debug,
    D: Fn(&[u8]) -> Result<T, E>,
    N: Fn(&T) -> Result<Vec<u8>, E>,
    E: core::fmt::Debug,
{
    let Ok(value) = decode(body) else {
        return;
    };
    let bytes = encode(&value).expect("a decoded value re-encodes");
    let again = decode(&bytes).expect("re-encoded bytes decode");
    assert_eq!(again, value);
}

macro_rules! check {
    ($body:expr, $module:ident, $($decode:ident / $encode:ident),+ $(,)?) => {
        $(round_trip($body, $module::$decode, $module::$encode);)+
    };
}

/// Every body decoder of `os.lazy.accounts.v1`.
fn accounts_bodies(body: &[u8]) {
    check!(
        body,
        accounts,
        decode_user / encode_user,
        decode_new_user / encode_new_user,
        decode_lookup_args / encode_lookup_args,
        decode_lookup_reply / encode_lookup_reply,
        decode_authenticate_args / encode_authenticate_args,
        decode_authenticate_reply / encode_authenticate_reply,
        decode_create_args / encode_create_args,
        decode_create_reply / encode_create_reply,
    );
}

/// Every body decoder of `os.lazy.keyd.v1`.
fn keyd_bodies(body: &[u8]) {
    check!(
        body,
        keyd,
        decode_key_info / encode_key_info,
        decode_verify_args / encode_verify_args,
        decode_verify_reply / encode_verify_reply,
        decode_sign_args / encode_sign_args,
        decode_sign_reply / encode_sign_reply,
        decode_wrap_args / encode_wrap_args,
        decode_wrap_reply / encode_wrap_reply,
        decode_unwrap_args / encode_unwrap_args,
        decode_unwrap_reply / encode_unwrap_reply,
        decode_random_args / encode_random_args,
        decode_random_reply / encode_random_reply,
        decode_generate_args / encode_generate_args,
        decode_generate_reply / encode_generate_reply,
        decode_list_reply / encode_list_reply,
        decode_provision_args / encode_provision_args,
    );
}

/// Treat `input` as a whole parcel (header, body, handles) and as a bare body.
/// Never panics.
pub fn run(input: &[u8]) {
    if let Ok(parcel) = Parcel::decode(input) {
        // What the envelope accepts, it also reproduces.
        let mut bytes = Vec::new();
        parcel
            .encode(&mut bytes)
            .expect("a decoded parcel re-encodes");
        assert_eq!(Parcel::decode(&bytes).as_ref(), Ok(&parcel));
        accounts_bodies(&parcel.body);
        keyd_bodies(&parcel.body);
    }
    accounts_bodies(input);
    keyd_bodies(input);
}

#[cfg(test)]
mod seeded {
    use super::*;
    use fuzzkit::for_seeds;
    use messenger_generated::os_lazy_accounts_v1 as wire;

    /// Replay every checked-in seed (`fuzz/seeds/accountwire`) and saved crash
    /// (`fuzz/regressions/accountwire`).
    #[test]
    fn corpus_and_regressions_replay() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz");
        let mut seen = 0;
        for dir in ["seeds", "regressions"] {
            let Ok(entries) = std::fs::read_dir(root.join(dir).join("accountwire")) else {
                continue;
            };
            for entry in entries.flatten() {
                run(&std::fs::read(entry.path()).unwrap());
                seen += 1;
            }
        }
        if std::env::var_os("CI").is_some() {
            assert!(seen > 0, "no seeds found for accountwire");
        }
    }

    #[test]
    fn raw_noise_is_safe() {
        for_seeds("accountwire::raw_noise_is_safe", |_, rng| {
            let len = rng.range(0, 96) as usize;
            run(&rng.bytes(len));
        });
    }

    /// Valid bodies with bits flipped: the shapes the decoders really see.
    #[test]
    fn mutated_valid_bodies_are_safe() {
        let bodies = [
            wire::encode_lookup_args(&wire::LookupArgs {
                name: Some("admin".into()),
                uid: Some(0),
            })
            .unwrap(),
            wire::encode_authenticate_args(&wire::AuthenticateArgs {
                name: "user".into(),
                secret: "pw".into(),
            })
            .unwrap(),
            wire::encode_create_args(&wire::CreateArgs {
                user: wire::NewUser {
                    name: "guest".into(),
                    uid: 1001,
                    gid: 100,
                    secret: "s".into(),
                    home: "/home/guest".into(),
                    shell: "/bin/sh".into(),
                },
            })
            .unwrap(),
            keyd::encode_provision_args(&keyd::ProvisionArgs {
                user: "user".into(),
                secret: "pw".into(),
            })
            .unwrap(),
        ];
        for_seeds("accountwire::mutated_valid_bodies_are_safe", |_, rng| {
            let mut body = rng.pick(&bodies).clone();
            let flips = rng.range(0, 6) as usize;
            rng.flip_bits(&mut body, flips);
            if rng.one_in(4) {
                body.truncate(rng.below(body.len() as u64 + 1) as usize);
            }
            run(&body);
        });
    }
}
