//! `fetch`, `curl` and `wget` for LazyOS: one blocking HTTPS client with
//! three command-line personalities (docs/tls-plan.md §7).
//!
//! The program is a static musl `std` binary: TLS runs in this process
//! (rustls + webpki + the pure-Rust `nettls-crypto` provider), over
//! `std::net` and the kernel's `AF_INET` shim.
//! Everything runs on the calling thread, because the shim gives each thread
//! its own descriptor table (plan §4.2): see [`resolve`].
//!
//! Layout: [`cli`] turns argv into [`opts::Options`]; [`app`] builds the
//! trust store ([`roots`]), the TLS connector ([`tls`]) and the `ureq` agent,
//! follows redirects under [`redirect`]'s rules ([`client`]) and writes the
//! body through [`body`] (gzip, size cap) to [`output`]'s destination.
//! Failures carry their exit codes in [`report`].

pub mod app;
pub mod body;
pub mod cli;
pub mod client;
pub mod opts;
pub mod output;
pub mod redirect;
pub mod report;
pub mod resolve;
pub mod roots;
pub mod sanitize;
pub mod timefmt;
pub mod tls;
pub mod writeout;
pub mod x509info;
