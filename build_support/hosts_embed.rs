//! The host table at `fhs::etc::HOSTS` (Linux programs read it as
//! `/etc/hosts`; docs/tls-plan.md §5.1): `localhost` for IPv4 and IPv6 in
//! every image.
//!
//! `LAZYOS_TLS_TEST_HOSTS=<file>` is a test-only switch: the TLS harness names
//! its servers on the host (QEMU user networking's `10.0.2.2`) without DNS.
//! Each line of the file must be `<IPv4-or-IPv6> <name> [names...]` with names
//! in lowercase `[a-z0-9.-]`; blank lines and `#` comment lines are skipped
//! and anything else fails the build. The entries are rewritten in a canonical
//! form, so only addresses and names reach the image.

use std::net::IpAddr;
use std::path::Path;

use crate::os_image::Sink;

/// What every image's host table says.
/// (`lazyos` is the machine's name, `/etc/hostname`.)
pub const BASE: &str = "127.0.0.1\tlocalhost lazyos\n::1\tlocalhost\n";

/// The longest DNS name (RFC 1035 §2.3.4, without the trailing dot).
const MAX_NAME: usize = 253;

/// Whether `name` is an acceptable host name: lowercase letters, digits, `.`
/// and `-`, not starting or ending with `.` or `-`, no empty label.
pub fn valid_name(name: &str) -> bool {
    let allowed = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-';
    !name.is_empty()
        && name.len() <= MAX_NAME
        && name.bytes().all(allowed)
        && !name.starts_with(['.', '-'])
        && !name.ends_with(['.', '-'])
        && !name.contains("..")
}

/// Parse a hosts file into canonical lines (`<address>\t<name> <name>...\n`).
/// Errors name the offending line.
pub fn parse_hosts(text: &str) -> Result<String, String> {
    let mut out = String::new();
    for (index, raw) in text.lines().enumerate() {
        let number = index + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split_ascii_whitespace();
        let address = fields.next().unwrap_or_default();
        let address: IpAddr = address
            .parse()
            .map_err(|_| format!("line {number}: {address:.40} is not an IP address"))?;
        let names: Vec<&str> = fields.collect();
        if names.is_empty() {
            return Err(format!("line {number}: no host name"));
        }
        if let Some(bad) = names.iter().find(|name| !valid_name(name)) {
            return Err(format!(
                "line {number}: {bad:.40} is not a lowercase host name ([a-z0-9.-])"
            ));
        }
        out.push_str(&format!("{address}\t{}\n", names.join(" ")));
    }
    Ok(out)
}

/// The test entries `LAZYOS_TLS_TEST_HOSTS` names, or none when it is unset or
/// empty. A missing or malformed file fails the build.
fn test_hosts() -> String {
    println!("cargo:rerun-if-env-changed=LAZYOS_TLS_TEST_HOSTS");
    let Some(path) = std::env::var_os("LAZYOS_TLS_TEST_HOSTS").filter(|v| !v.is_empty()) else {
        return String::new();
    };
    let path = Path::new(&path);
    println!("cargo:rerun-if-changed={}", path.display());
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("LAZYOS_TLS_TEST_HOSTS: cannot read {}: {e}", path.display()));
    let lines = parse_hosts(&text)
        .unwrap_or_else(|e| panic!("LAZYOS_TLS_TEST_HOSTS {}: {e}", path.display()));
    println!(
        "cargo:warning=LAZYOS_TLS_TEST_HOSTS: {} test host line(s) from {} appended to {} \
         (a test image)",
        lines.lines().count(),
        path.display(),
        fhs::etc::HOSTS
    );
    lines
}

/// The whole table: [`BASE`] then the test entries.
pub fn hosts_file(test: &str) -> String {
    format!("{BASE}{test}")
}

/// Write the table to `fhs::etc::HOSTS`.
pub fn embed(sink: &mut dyn Sink) {
    sink.add_bytes(fhs::etc::HOSTS, hosts_file(&test_hosts()).into_bytes());
}
