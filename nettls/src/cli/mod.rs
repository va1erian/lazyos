//! Command lines of the three personalities. Each turns argv into the same
//! [`Options`](crate::opts::Options); nothing here touches the network.

pub mod args;
mod curl;
mod fetch;
mod wget;

use crate::opts::{Command, Personality};

/// Parse `argv` (without `argv[0]`) the way `personality` does.
pub fn parse(personality: Personality, argv: &[String]) -> Result<Command, String> {
    match personality {
        Personality::Fetch => fetch::parse(argv),
        Personality::Curl => curl::parse(argv),
        Personality::Wget => wget::parse(argv),
    }
}

const HELP_FOOTER: &str = "
Certificates are always verified against the system CA bundle
(/etc/ssl/certs/ca-certificates.crt, or SSL_CERT_FILE); there is no insecure
mode. https -> http redirects are refused. Bodies are limited to 64 MiB.
";

fn version(name: &str) -> String {
    format!(
        "{name} (LazyOS nettls {}) rustls 0.23 (ring), ureq 3; https http\n",
        env!("CARGO_PKG_VERSION")
    )
}

/// The single URL a run fetches.
fn one_url(mut positional: Vec<String>) -> Result<String, String> {
    match positional.len() {
        0 => Err("no URL specified".into()),
        1 => Ok(positional.remove(0)),
        _ => Err("only one URL per run is supported".into()),
    }
}

/// Why a flag that disables certificate checks does not exist here.
fn insecure_refused(flag: &str, alternative: &str) -> String {
    format!(
        "{flag} is not supported: certificates are always verified, because an \
         unverified connection protects against nobody. To trust a private or \
         test CA, pass its certificate with {alternative}."
    )
}
