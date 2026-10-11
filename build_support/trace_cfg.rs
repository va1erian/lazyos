//! Tracing settings for `lazyos.cfg` (docs/tracing-plan.md, stage T1):
//! `LAZYOS_TRACE_MASK=init,pkgd,-shell` becomes `trace.mask=init,pkgd,-shell`,
//! read by the kernel's `trace::apply_config` at boot (`libs/tracefmt`'s
//! `Mask`).
//!
//! `LAZYOS_TRACE=0` compiles the trace macros out (the kernel's own
//! `build.rs` reads it); the old switches are aliases that imply
//! `LAZYOS_TRACE=1` and add their channel to the mask: `LAZYOS_LABEL_TRACE=1`
//! (`label`), `LAZYOS_USB_TRACE=1` (`usbd`) and `LAZYOS_UI_PROBE=1` (`ui`).
//! The build checks the mask with the parser the kernel uses, so a misspelt
//! channel fails here instead of being ignored with a log line at boot.

/// `(variable, channel)` of the old switches.
pub const ALIASES: [(&str, &str); 3] = [
    ("LAZYOS_LABEL_TRACE", "label"),
    ("LAZYOS_USB_TRACE", "usbd"),
    ("LAZYOS_UI_PROBE", "ui"),
];

fn is_on(name: &str) -> bool {
    println!("cargo:rerun-if-env-changed={name}");
    std::env::var_os(name).as_deref() == Some(std::ffi::OsStr::new("1"))
}

/// The `trace.mask=` value the build writes: the `LAZYOS_TRACE_MASK` text
/// (or `default`) plus the channels of the aliases that are set. `Ok(None)`
/// when nothing asks for a mask. `Err` names the channel nobody knows.
pub fn mask_text(explicit: Option<&str>, aliases: &[&str]) -> Result<Option<String>, String> {
    let explicit = explicit.map(str::trim).filter(|text| !text.is_empty());
    if explicit.is_none() && aliases.is_empty() {
        return Ok(None);
    }
    let mut parts = vec![explicit.unwrap_or("default").to_string()];
    parts.extend(aliases.iter().map(|alias| alias.to_string()));
    let text = parts.join(",");
    let mut unknown = Vec::new();
    tracefmt::Mask::parse(&text, |problem| {
        if let tracefmt::Problem::Unknown(name) = problem {
            unknown.push(name.to_string());
        }
    });
    if unknown.is_empty() {
        Ok(Some(text))
    } else {
        Err(format!(
            "LAZYOS_TRACE_MASK: no such channel: {} (see docs/trace/channels.md)",
            unknown.join(", ")
        ))
    }
}

/// The `lazyos.cfg` line (with its newline) for the environment, or an empty
/// string. Panics, failing the build, on a mask that names no channel.
pub fn from_env() -> String {
    println!("cargo:rerun-if-env-changed=LAZYOS_TRACE_MASK");
    let explicit = std::env::var("LAZYOS_TRACE_MASK").ok();
    let aliases: Vec<&str> = ALIASES
        .iter()
        .filter(|(name, _)| is_on(name))
        .map(|(_, channel)| *channel)
        .collect();
    match mask_text(explicit.as_deref(), &aliases) {
        Ok(Some(text)) => format!("trace.mask={text}\n"),
        Ok(None) => String::new(),
        Err(error) => panic!("{error}"),
    }
}
