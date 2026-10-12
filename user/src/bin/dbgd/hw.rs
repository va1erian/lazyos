//! `pci.config` and `mmio.read`: read-only looks at a PCI function's
//! configuration space and allowlisted registers, without claiming it.
//!
//! The rules are `libs/pciprobe`'s and the kernel enforces them
//! (`kernel/src/dev/peek.rs`); this module applies the same rules first so a
//! refusal can say *why* (the allowed ranges of the device, which `lazyos.cfg`
//! key turns the tier on) instead of an errno. Nothing here writes, and no
//! request can name a physical address: the kernel resolves the BAR from the
//! function's own enumerated resources.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use dbgwire::auth;
use dbgwire::config::Config;
use dbgwire::json::{Object, Value};
use dbgwire::pciprobe::{self, Denied};
use dbgwire::rpc::code;
use user::dev::inspect;

use super::handlers::Failure;

/// Errno values the kernel's hardware ops return (`kernel/src/dev/errno.rs`).
mod errno {
    pub const EPERM: i64 = 1;
    pub const ENXIO: i64 = 6;
    pub const ENOMEM: i64 = 12;
    pub const EACCES: i64 = 13;
    pub const EFAULT: i64 = 14;
    pub const EBUSY: i64 = 16;
    pub const ENODEV: i64 = 19;
    pub const EINVAL: i64 = 22;
    pub const ERANGE: i64 = 34;
}

fn number(params: &Value, key: &str, default: u64) -> u64 {
    params.get(key).and_then(Value::as_u64).unwrap_or(default)
}

fn bad(message: &str) -> Failure {
    (code::INVALID_PARAMS, String::from(message))
}

/// The function's address and ids from the inventory (the kernel checks the
/// same thing again; this is for the message and for the allowlist lookup).
struct Function {
    bdf: u16,
    vendor: u16,
    device: u16,
}

fn locate(params: &Value) -> Result<Function, Failure> {
    let text = params
        .get("bdf")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("needs a bdf such as \"00:02.0\""))?;
    let bdf = pciprobe::parse_bdf(text)
        .ok_or_else(|| bad("bdf is bus:device.function in hex, such as \"00:02.0\""))?;
    let devices = inspect::inventory().map_err(|errno| {
        (
            code::UNAVAILABLE,
            format!("device inventory: errno {}", errno),
        )
    })?;
    let found = devices.iter().find(|d| d.bdf == Some(bdf)).ok_or_else(|| {
        (
            code::UNAVAILABLE,
            format!("no PCI function at {} (see devices.list)", pciprobe::format_bdf(bdf)),
        )
    })?;
    Ok(Function {
        bdf,
        vendor: found.vendor,
        device: found.device,
    })
}

/// A kernel errno as the JSON-RPC failure that explains it.
fn kernel_failure(errno: i64, what: &str) -> Failure {
    let (rpc, why) = match errno {
        errno::EPERM => (code::DENIED, "the kernel refuses this uid (only dbgd and root may read hardware)"),
        errno::EACCES => (
            code::DENIED,
            "refused by the kernel's rules: MMIO is off, or the span is not in an allowed range",
        ),
        errno::EBUSY => (
            code::DENIED,
            "a driver holds this device; diag.dbg.mmio.claimed=1 in lazyos.cfg allows reading it anyway",
        ),
        errno::ENODEV => (code::UNAVAILABLE, "no such PCI function"),
        errno::ENXIO => (
            code::UNAVAILABLE,
            "the BAR is not readable memory (an I/O or unassigned BAR, memory decode off, or it overlaps RAM)",
        ),
        errno::ERANGE => (code::INVALID_PARAMS, "the span runs past the end of the BAR"),
        errno::ENOMEM => (code::UNAVAILABLE, "the kernel's MMIO window budget for dbgd is used up"),
        errno::EFAULT => (code::INTERNAL, "the kernel could not copy the answer out"),
        errno::EINVAL => (
            code::UNAVAILABLE,
            "the kernel has no hardware-read op (build kernel and image with LAZYOS_DBGD=1)",
        ),
        _ => (code::UNAVAILABLE, "the kernel failed the read"),
    };
    (rpc, format!("{what}: {why} (errno {errno})"))
}

fn denied(why: Denied, function: &Function, bar: u8, config: &Config) -> Failure {
    match why {
        Denied::Off => (code::DENIED, String::from(why.text())),
        Denied::NotAllowed => {
            let ranges = config.probe.ranges(function.vendor, function.device, bar);
            let listed = if ranges.is_empty() {
                format!(
                    "none listed for {:04x}:{:04x} BAR{bar} (add one with diag.dbg.mmio.allow= in lazyos.cfg)",
                    function.vendor, function.device
                )
            } else {
                ranges
                    .iter()
                    .map(|r| format!("{:#x}-{:#x}", r.start, r.end))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            (
                code::DENIED,
                format!("{}; allowed for BAR{bar}: {listed}", why.text()),
            )
        }
        _ => (code::INVALID_PARAMS, String::from(why.text())),
    }
}

/// `pci.config {bdf, offset, len}`.
pub(crate) fn pci_config(params: &Value) -> Result<String, Failure> {
    let offset = number(params, "offset", 0);
    let len = number(params, "len", 64);
    pciprobe::check_config(offset, len).map_err(|why| bad(why.text()))?;
    let function = locate(params)?;
    let bytes = inspect::config_read(function.bdf, offset, len as usize)
        .map_err(|errno| kernel_failure(errno, "pci.config"))?;
    Ok(Object::new()
        .str("bdf", &pciprobe::format_bdf(function.bdf))
        .str("vendor", &format!("{:04x}", function.vendor))
        .str("device", &format!("{:04x}", function.device))
        .uint("offset", offset)
        .uint("len", bytes.len() as u64)
        .str("hex", &auth::hex(&bytes))
        .finish())
}

/// `mmio.read {bdf, bar, offset, len}`.
pub(crate) fn mmio_read(params: &Value, config: &Config) -> Result<String, Failure> {
    let bar = number(params, "bar", 0) as u8;
    let offset = number(params, "offset", 0);
    let len = number(params, "len", 4);
    let function = locate(params)?;
    config
        .probe
        .plan(function.vendor, function.device, bar, offset, len)
        .map_err(|why| denied(why, &function, bar, config))?;
    let bytes = inspect::mmio_read(function.bdf, bar, offset, len as usize)
        .map_err(|errno| kernel_failure(errno, "mmio.read"))?;
    Ok(Object::new()
        .str("bdf", &pciprobe::format_bdf(function.bdf))
        .str("vendor", &format!("{:04x}", function.vendor))
        .str("device", &format!("{:04x}", function.device))
        .uint("bar", u64::from(bar))
        .uint("offset", offset)
        .uint("len", bytes.len() as u64)
        .str("hex", &auth::hex(&bytes))
        .finish())
}
