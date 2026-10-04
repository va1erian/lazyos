//! `tlsfix` — rustls with the `nettls-crypto` provider on the Linux ABI
//! (docs/tls-plan.md §8, stage T0).
//!
//! 1. Reports the CPU features the RustCrypto SIMD dispatch depends on and
//!    fails on LazyOS if an AVX path could be chosen (LazyOS does not save
//!    YMM state; see `cpu.rs`).
//! 2. Runs complete TLS 1.3 and 1.2 handshakes between a rustls client and
//!    server joined by in-memory buffers, with a request and a 256 KiB
//!    response through each AEAD (AES-128/256-GCM, ChaCha20-Poly1305):
//!    X25519/P-256 key exchange, ECDSA signing and webpki verification of a
//!    test chain (`testdata/`, valid 2000..2099).
//! 3. Checks that webpki refuses a wrong host name and an unknown CA.
//!
//! The ABI bench has no network peer, so nothing here touches a socket; the
//! networked path is `fetch` under `tools/net/run.py --tls`. Prints
//! `TLSFIX:<check>:PASS|FAIL` lines, then `ABI:tlsfix:PASS` or `FAIL`.

#[path = "../../src/common.rs"]
mod common;
mod cpu;
mod pipe;

const BODY: usize = 256 * 1024;

fn check(name: &str, outcome: Result<String, String>, failures: &mut Vec<String>) {
    match outcome {
        Ok(detail) => println!("TLSFIX:{name}:PASS {detail}"),
        Err(why) => {
            println!("TLSFIX:{name}:FAIL:{why}");
            failures.push(format!("{name}: {why}"));
        }
    }
}

fn cpu_check() -> Result<String, String> {
    let features = cpu::Features::probe();
    println!("{}", features.line());
    let system = cpu::sysname();
    if system == "LazyOS" && features.avx_path() {
        return Err("an AVX backend could be chosen, but LazyOS does not save YMM state".into());
    }
    Ok(format!("system={system} avx_path={}", features.avx_path()))
}

fn main() {
    let mut failures = Vec::new();
    check("cpu", cpu_check(), &mut failures);
    use rustls::NamedGroup::{secp256r1, secp384r1, X25519};
    let runs = [
        (
            "tls13_aes128gcm",
            nettls_crypto::TLS13_AES_128_GCM_SHA256,
            X25519,
        ),
        (
            "tls13_aes256gcm",
            nettls_crypto::TLS13_AES_256_GCM_SHA384,
            secp256r1,
        ),
        (
            "tls13_chacha20",
            nettls_crypto::TLS13_CHACHA20_POLY1305_SHA256,
            secp384r1,
        ),
        (
            "tls12_ecdsa_aes128gcm",
            nettls_crypto::TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256,
            X25519,
        ),
        (
            "tls12_ecdsa_aes256gcm",
            nettls_crypto::TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384,
            secp256r1,
        ),
        (
            "tls12_ecdsa_chacha20",
            nettls_crypto::TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256,
            X25519,
        ),
    ];
    for (name, suite, group) in runs {
        check(
            name,
            pipe::exchange(suite, Some(group), BODY),
            &mut failures,
        );
    }
    check(
        "wrong_name",
        pipe::refused(pipe::CA, "other.test"),
        &mut failures,
    );
    check(
        "unknown_ca",
        pipe::refused(pipe::OTHER_CA, "tlsfix.test"),
        &mut failures,
    );
    if failures.is_empty() {
        common::pass("tlsfix");
    } else {
        common::fail("tlsfix", &failures.join("; "));
    }
}
