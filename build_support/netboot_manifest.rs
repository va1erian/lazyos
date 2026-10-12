//! The pure half of `LAZYOS_NETBOOT=1` (docs/netboot-plan.md N1): the build
//! id, `manifest.json` and its Ed25519 signature `manifest.sig`. Nothing here
//! touches the disk or the environment, so the host tests drive it directly
//! (`build_support/tests/netboot_tests.rs`); `netboot.rs` gathers the inputs
//! and writes the files.
//!
//! The formats are a contract with the registry (`tools/netboot/`):
//!
//! * the build id is `<yyyymmdd-HHMMSS>-<rev8>[-dirty]`: the UTC build time,
//!   the first 8 hex digits of the revision (`00000000` when unknown);
//! * `manifest.json` is one JSON object with `id`, `revision` (40 hex or
//!   `unknown`), `dirty`, `built_at` (`2026-10-12T09:30:00Z`), `host`,
//!   `switches` (the `LAZYOS_*` variable names set at build time, sorted) and
//!   `files` (name to `{"size", "sha256"}`);
//! * `manifest.sig` is the pure Ed25519 (RFC 8032) signature over the exact
//!   bytes of `manifest.json`: 128 lowercase hex characters and a newline. The
//!   key file `key.sec` is the 32-byte seed as 64 hex characters and a
//!   newline.

use ed25519_dalek::{Signer, SigningKey};

/// The kernel's rule for an image id (`kernel/src/boot_image.rs`): 1 to 64 of
/// `[0-9A-Za-z._-]`. Checked here so a build never writes a line the kernel
/// would refuse.
pub fn valid_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// A UTC calendar time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Utc {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

impl Utc {
    /// The calendar time of `secs` seconds after the Unix epoch (civil date
    /// from days, after Howard Hinnant's algorithm).
    pub fn from_unix(secs: i64) -> Utc {
        let days = secs.div_euclid(86_400);
        let rem = secs.rem_euclid(86_400) as u32;
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
        let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
        let year = yoe + era * 400 + i64::from(month <= 2);
        Utc {
            year,
            month,
            day,
            hour: rem / 3600,
            minute: rem % 3600 / 60,
            second: rem % 60,
        }
    }

    /// `2026-10-12T09:30:00Z`.
    pub fn iso(&self) -> String {
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }

    /// `20261012-093000`.
    pub fn compact(&self) -> String {
        format!(
            "{:04}{:02}{:02}-{:02}{:02}{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }
}

/// What identifies one build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildInfo {
    pub id: String,
    /// 40 lowercase hex digits, or `unknown`.
    pub revision: String,
    pub dirty: bool,
    pub built_at: String,
    pub host: String,
    pub switches: Vec<String>,
}

/// The revision the kernel embeds (`kernel/build.rs`: `LAZYOS_REVISION`, else
/// `git rev-parse HEAD`, else `unknown`) as the manifest states it: exactly 40
/// hex digits (lower-cased) or `unknown`. Anything else is `unknown` with the
/// reason in the second field, for the caller to print.
pub fn revision(env: Option<&str>, git_head: Option<&str>) -> (String, Option<String>) {
    let text = env
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .or_else(|| git_head.map(str::trim).filter(|text| !text.is_empty()));
    match text {
        None => ("unknown".into(), None),
        Some(text) if text.len() == 40 && text.bytes().all(|b| b.is_ascii_hexdigit()) => {
            (text.to_ascii_lowercase(), None)
        }
        Some(text) => (
            "unknown".into(),
            Some(format!(
                "the revision {text:?} is not 40 hex digits; the manifest says unknown"
            )),
        ),
    }
}

impl BuildInfo {
    /// Assemble the identity. `vars` are the build's environment variables;
    /// `switches` keeps the names starting `LAZYOS_` whose value is not empty.
    pub fn new(
        now_unix: i64,
        revision: &str,
        dirty: bool,
        host: &str,
        vars: impl IntoIterator<Item = (String, String)>,
    ) -> BuildInfo {
        let when = Utc::from_unix(now_unix);
        let rev8 = revision.get(..8).filter(|_| revision != "unknown");
        let mut id = format!("{}-{}", when.compact(), rev8.unwrap_or("00000000"));
        if dirty {
            id += "-dirty";
        }
        let mut switches: Vec<String> = vars
            .into_iter()
            .filter(|(name, value)| name.starts_with("LAZYOS_") && !value.is_empty())
            .map(|(name, _)| name)
            .collect();
        switches.sort();
        switches.dedup();
        BuildInfo {
            id,
            revision: revision.into(),
            dirty,
            built_at: when.iso(),
            host: host.into(),
            switches,
        }
    }
}

/// One payload file in the manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    pub name: String,
    pub size: u64,
    /// 64 lowercase hex digits.
    pub sha256: String,
}

/// A JSON string literal for `text` (quotes, backslashes and control
/// characters escaped; other characters stay UTF-8).
fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The bytes of `manifest.json`: the keys in the registry's documented order,
/// two-space indent, a final newline. These exact bytes are signed.
pub fn manifest_json(info: &BuildInfo, files: &[FileEntry]) -> String {
    let mut out = String::from("{\n");
    out += &format!("  \"id\": {},\n", json_string(&info.id));
    out += &format!("  \"revision\": {},\n", json_string(&info.revision));
    out += &format!("  \"dirty\": {},\n", info.dirty);
    out += &format!("  \"built_at\": {},\n", json_string(&info.built_at));
    out += &format!("  \"host\": {},\n", json_string(&info.host));
    if info.switches.is_empty() {
        out += "  \"switches\": [],\n";
    } else {
        let names: Vec<String> = info
            .switches
            .iter()
            .map(|name| format!("    {}", json_string(name)))
            .collect();
        out += &format!("  \"switches\": [\n{}\n  ],\n", names.join(",\n"));
    }
    out += "  \"files\": {\n";
    let entries: Vec<String> = files
        .iter()
        .map(|file| {
            format!(
                "    {}: {{\"size\": {}, \"sha256\": {}}}",
                json_string(&file.name),
                file.size,
                json_string(&file.sha256)
            )
        })
        .collect();
    out += &entries.join(",\n");
    out += "\n  }\n}\n";
    out
}

/// The signing key from the text of `key.sec`: 64 hex digits (the 32-byte
/// seed) and optional surrounding whitespace.
pub fn parse_key(text: &str) -> Result<SigningKey, String> {
    let hex = text.trim();
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!(
            "the signing key must be the 32-byte seed as 64 hex characters, got {} characters",
            hex.len()
        ));
    }
    let mut seed = [0u8; 32];
    for (byte, pair) in seed.iter_mut().zip(hex.as_bytes().chunks(2)) {
        let digits = std::str::from_utf8(pair).map_err(|e| e.to_string())?;
        *byte = u8::from_str_radix(digits, 16).map_err(|e| e.to_string())?;
    }
    Ok(SigningKey::from_bytes(&seed))
}

/// `manifest.sig`: the Ed25519 signature over `manifest` as 128 lowercase hex
/// characters and a newline.
pub fn signature_file(key: &SigningKey, manifest: &[u8]) -> String {
    let signature = key.sign(manifest);
    let mut out = hex(&signature.to_bytes());
    out.push('\n');
    out
}

/// Lowercase hex of `bytes`.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
