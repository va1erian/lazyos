//! Which build booted: the `image.id=<id>` line of `lazyos.cfg`
//! (docs/netboot-plan.md N1).
//!
//! `LAZYOS_NETBOOT=1` names every build `<yyyymmdd-HHMMSS>-<rev8>[-dirty]` and
//! writes that id into the ramdisk's `lazyos.cfg`; the kernel keeps it
//! ([`get`]) and prints `BOOT:IMAGE:<id>` once, when the boot volume's config
//! is read. About shows it and a harness requires the booted system to report
//! the id it pushed. The id is only a name: a line that is missing logs
//! nothing, and one that is not `[0-9A-Za-z._-]{1,64}` is refused with a log
//! line and changes nothing else, least of all the mount (the file is
//! untrusted input, and `bootcfg::parse` skips every `image.` line so a bad id
//! can never cost the boot its root volume).

use alloc::string::String;
use spin::Mutex;

/// Prefix of the keys this module owns (`bootcfg::parse` skips them).
pub const PREFIX: &str = "image.";
/// The one key it reads.
const KEY: &str = "image.id";
/// Longest accepted id.
pub const MAX_LEN: usize = 64;

/// Why an id was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refused {
    Empty,
    TooLong,
    BadCharacter,
}

impl Refused {
    fn message(self) -> &'static str {
        match self {
            Refused::Empty => "empty",
            Refused::TooLong => "longer than 64 bytes",
            Refused::BadCharacter => "characters outside 0-9 A-Z a-z . _ -",
        }
    }
}

/// `^[0-9A-Za-z._-]{1,64}$`.
pub fn validate(id: &str) -> Result<&str, Refused> {
    if id.is_empty() {
        return Err(Refused::Empty);
    }
    if id.len() > MAX_LEN {
        return Err(Refused::TooLong);
    }
    let allowed = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-');
    if id.bytes().all(allowed) {
        Ok(id)
    } else {
        Err(Refused::BadCharacter)
    }
}

static IMAGE_ID: Mutex<Option<String>> = Mutex::new(None);
/// How many `BOOT:IMAGE` lines were printed (the kernel suite counts them).
#[cfg(lazyos_tests)]
static ANNOUNCED: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Read the `image.id=` line of a `lazyos.cfg` text: remember a valid id and
/// print `BOOT:IMAGE:<id>`, log a refused one and keep nothing. A later line
/// replaces an earlier one, as with the other prefixed keys.
pub fn apply_config(text: &str) {
    let mut chosen: Option<&str> = None;
    for line in text.lines() {
        let Some((key, value)) = line.trim().split_once('=') else {
            continue;
        };
        let key = key.trim();
        if !key.starts_with(PREFIX) {
            continue;
        }
        if key != KEY {
            serial_println!("boot: lazyos.cfg: unknown key {key} ignored");
            continue;
        }
        match validate(value.trim()) {
            Ok(id) => chosen = Some(id),
            Err(why) => serial_println!("boot: image.id refused ({})", why.message()),
        }
    }
    let Some(id) = chosen else {
        return;
    };
    let mut kept = IMAGE_ID.lock();
    // The mount reads the config more than once; the line is printed once.
    if kept.as_deref() == Some(id) {
        return;
    }
    *kept = Some(String::from(id));
    drop(kept);
    #[cfg(lazyos_tests)]
    ANNOUNCED.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    crate::trace::line!(tracefmt::Channel::Boot, "IMAGE:{}", id);
}

/// The id the boot config named, if it named a valid one.
pub fn get() -> Option<String> {
    IMAGE_ID.lock().clone()
}

/// Test hook: forget the id and the count of printed lines.
#[cfg(lazyos_tests)]
pub fn reset_for_test() {
    *IMAGE_ID.lock() = None;
    ANNOUNCED.store(0, core::sync::atomic::Ordering::Relaxed);
}

/// Test hook: how many `BOOT:IMAGE` lines were printed since the reset.
#[cfg(lazyos_tests)]
pub fn announced_for_test() -> u32 {
    ANNOUNCED.load(core::sync::atomic::Ordering::Relaxed)
}
