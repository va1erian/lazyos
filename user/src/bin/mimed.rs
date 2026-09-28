//! `mimed` (`MIMED.ELF`): the MIME database and open-with registry (issue
//! #116), and the shell-integration middle of the S4 stack.
//!
//! `mimed` owns two tables:
//!
//! * a **MIME database**: a built-in extension/filename table (`.txt`, `.md`,
//!   `.rs`, `.elf`, `.png`, `Makefile`, ...) plus a `/etc/mime.types`-style
//!   override read through the native file API at boot. The VFS tries
//!   `/etc/mime.types` first (the path an ext2 volume would carry) and falls
//!   back to `MIME.TYP`, an 8.3-safe file the boot image ships because the FAT
//!   reader cannot resolve long names;
//! * an **open-with registry** mapping `(mime, verb)` to an app id, with the
//!   shell verbs `open`, `edit` and `reveal` seeded for the built-in types and
//!   `Register`/`Lookup`/`Verbs`/`Open` served over Messenger.
//!
//! ## Launch path (interim)
//!
//! The supervisor (`init`) has no launch interface yet: it only spawns its
//! static manifest and watches exits. `Open` therefore resolves the app and
//! publishes a fire-and-forget `system/events/open/<app>` event on `init`'s
//! topic router - the same broker `logd` subscribes to - with a
//! `path=<path> mime=<mime> verb=<verb>` payload. An app id is the program's
//! 8.3 stem in lowercase (`editor` is `EDITOR.ELF`), so when `init` grows a
//! launch method (or an app registers for the topic) it can spawn
//! `APP.ELF <path>` from the same event. Until then the event is the
//! observable launch record: `messengerctl log` shows it.
//!
//! ## Boot evidence
//!
//! The service self-tests its database, registry and open walk at startup and
//! prints machine-parseable markers, so a headless boot proves the path:
//! `MIME:GUESS:PASS <path> <mime>`, `MIME:REGISTER:PASS` and
//! `MIME:OPEN:PASS <path> <app>` (with `MIME:...:FAIL` lines if a check
//! breaks).
//!
//! The on-disk name is `MIMED.ELF` (8.3-safe: the kernel's FAT reader only
//! resolves short names). `init` starts the service from its manifest.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::panic::PanicInfo;
use user::messenger::{self, errno, mime, registry, router, services, Error, Message, Parcel};
use user::sys;

/// Boot-time MIME database: extension (lowercase, no dot) to type.
const BUILTIN_TYPES: &[(&str, &str)] = &[
    ("txt", "text/plain"),
    ("text", "text/plain"),
    ("log", "text/plain"),
    ("md", "text/markdown"),
    ("markdown", "text/markdown"),
    ("rs", "text/x-rust"),
    ("elf", "application/x-elf"),
    ("png", "image/png"),
    ("html", "text/html"),
    ("htm", "text/html"),
    ("json", "application/json"),
    ("sh", "text/x-shellscript"),
    ("toml", "application/toml"),
    ("c", "text/x-c"),
    ("h", "text/x-c"),
];

/// Boot-time MIME database: exact filename to type (checked before the
/// extension, so `Makefile` is not `application/octet-stream`).
const BUILTIN_NAMES: &[(&str, &str)] = &[
    ("Makefile", "text/x-makefile"),
    ("README", "text/plain"),
    ("LICENSE", "text/plain"),
];

/// Override files tried in order at boot. The first is the conventional
/// `/etc/mime.types` path (an ext2 volume can carry it); the second is the
/// 8.3-safe name the FAT boot image ships, because the FAT reader only
/// resolves short names in the root directory.
const OVERRIDE_PATHS: &[&str] = &["/etc/mime.types", "MIME.TYP"];

/// Largest override file read at boot.
const OVERRIDE_BUFFER: usize = 4096;

/// Open-with defaults seeded at boot: `(mime, app, verbs)`. The app ids are
/// program stems (`editor` -> `EDITOR.ELF`); the apps themselves are S4/S5
/// work, so the launch event is the observable outcome for now.
const DEFAULT_APPS: &[(&str, &str, &[&str])] = &[
    ("text/plain", "editor", &["open", "edit"]),
    ("text/plain", "files", &["reveal"]),
    ("text/markdown", "editor", &["open", "edit"]),
    ("text/x-rust", "editor", &["open", "edit"]),
    ("text/x-shellscript", "editor", &["open", "edit"]),
    ("image/png", "viewer", &["open", "reveal"]),
    ("application/x-elf", "runner", &["open"]),
    ("application/octet-stream", "files", &["reveal"]),
];

/// The MIME database: built-in and override entries. A lookup checks the
/// exact filename first, then the override extensions, then the built-in
/// extensions, and falls back to `application/octet-stream`.
struct MimeDb {
    names: Vec<(String, String)>,
    builtin: Vec<(String, String)>,
    overrides: Vec<(String, String)>,
    /// Path the override table was read from, when one loaded.
    source: Option<String>,
}

impl MimeDb {
    /// The built-in table.
    fn builtin() -> MimeDb {
        MimeDb {
            names: BUILTIN_NAMES
                .iter()
                .map(|(name, mime)| (name.to_string(), mime.to_string()))
                .collect(),
            builtin: BUILTIN_TYPES
                .iter()
                .map(|(extension, mime)| (extension.to_string(), mime.to_string()))
                .collect(),
            overrides: Vec::new(),
            source: None,
        }
    }

    /// Apply a `/etc/mime.types`-style override table and remember the
    /// source; `false` when every candidate path is missing.
    fn load_overrides(&mut self, buffer: &mut [u8]) -> bool {
        for path in OVERRIDE_PATHS {
            let Some(length) = read_override(path, buffer) else {
                continue;
            };
            let text = core::str::from_utf8(&buffer[..length]).unwrap_or("");
            self.parse_overrides(text);
            self.source = Some(String::from(*path));
            return true;
        }
        false
    }

    /// Parse `<mime> <ext>...` lines; `#` starts a comment.
    fn parse_overrides(&mut self, text: &str) {
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let mut tokens = line.split_whitespace();
            let Some(mime_type) = tokens.next() else {
                continue;
            };
            if !valid_mime(mime_type) {
                continue;
            }
            for extension in tokens {
                if valid_extension(extension) {
                    self.overrides
                        .push((extension.to_ascii_lowercase(), mime_type.to_string()));
                }
            }
        }
    }

    /// The MIME type for a path.
    fn guess(&self, path: &str) -> String {
        let name = file_name(path);
        if let Some(mime_type) = lookup(&self.names, name) {
            return mime_type.to_string();
        }
        if let Some(extension) = extension(name) {
            if let Some(mime_type) = lookup(&self.overrides, extension) {
                return mime_type.to_string();
            }
            if let Some(mime_type) = lookup(&self.builtin, extension) {
                return mime_type.to_string();
            }
        }
        String::from(mime::FALLBACK_MIME)
    }

    /// Number of override entries loaded (boot diagnostics).
    fn override_count(&self) -> usize {
        self.overrides.len()
    }
}

/// The last path component (`/` and `\` both separate, so a Linux-style path
/// works on the console too).
fn file_name(path: &str) -> &str {
    path.rsplit(|character| character == '/' || character == '\\')
        .next()
        .unwrap_or(path)
}

/// The extension after the last dot of a file name, if it has one.
fn extension(name: &str) -> Option<&str> {
    let (stem, extension) = name.rsplit_once('.')?;
    if stem.is_empty() || extension.is_empty() {
        return None;
    }
    Some(extension)
}

/// The last (case-insensitive) `key` match in a table, so an override
/// appended later replaces a built-in entry.
fn lookup<'a>(table: &'a [(String, String)], key: &str) -> Option<&'a str> {
    table
        .iter()
        .rev()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(key))
        .map(|(_, mime_type)| mime_type.as_str())
}

/// One open-with registration.
struct Registration {
    mime: String,
    verb: String,
    app: String,
}

/// The open-with registry: `(mime, verb)` to app. Re-registering a pair
/// replaces its app, so a later policy wins.
struct AppRegistry {
    entries: Vec<Registration>,
}

impl AppRegistry {
    fn new() -> AppRegistry {
        AppRegistry {
            entries: Vec::new(),
        }
    }

    /// Add or replace the app for `(mime, verb)`.
    fn register(&mut self, mime_type: &str, app: &str, verb: &str) {
        let mime_type = mime_type.trim().to_ascii_lowercase();
        let verb = verb.trim().to_ascii_lowercase();
        let app = app.trim();
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.mime == mime_type && entry.verb == verb)
        {
            entry.app = app.to_string();
        } else {
            self.entries.push(Registration {
                mime: mime_type,
                verb,
                app: app.to_string(),
            });
        }
    }

    /// The app registered for a type and verb (both case-insensitive).
    fn lookup(&self, mime_type: &str, verb: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|entry| {
                entry.mime.eq_ignore_ascii_case(mime_type.trim())
                    && entry.verb.eq_ignore_ascii_case(verb.trim())
            })
            .map(|entry| entry.app.as_str())
    }

    /// The verbs registered for a type, in registration order.
    fn verbs(&self, mime_type: &str) -> Vec<String> {
        let mut verbs: Vec<String> = Vec::new();
        for entry in &self.entries {
            if entry.mime.eq_ignore_ascii_case(mime_type.trim()) && !verbs.contains(&entry.verb) {
                verbs.push(entry.verb.clone());
            }
        }
        verbs
    }
}

/// A request token (app or verb): no whitespace and no topic separators.
fn valid_token(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 64
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'+'))
}

/// A MIME type: `type/subtype`, no whitespace.
fn valid_mime(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 128
        && text.contains('/')
        && !text.contains("//")
        && text.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'_' | b'-' | b'.' | b'+')
        })
}

/// An override extension token: a bare extension, no dot or separator.
fn valid_extension(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 16
        && !text.contains('.')
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'+'))
}

/// Read `path` through the native file API (the VFS, permission-checked);
/// `None` when the file is missing or the path is too long.
fn read_override(path: &str, buffer: &mut [u8]) -> Option<usize> {
    let mut name = [0u8; 64];
    if path.len() + 1 > name.len() {
        return None;
    }
    name[..path.len()].copy_from_slice(path.as_bytes());
    sys::read_file(&name[..path.len() + 1], buffer)
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    sys::write_str("mimed: MIME database and open-with registry (issue #116)\n");
    if let Err(error) = run() {
        sys::write_str("mimed: fatal: ");
        sys::write_str(error.message());
        sys::write_str("\n");
        sys::exit(1);
    }
    sys::exit(0)
}

/// Register the service, build the tables, run the boot self-test, and serve.
fn run() -> messenger::Result<()> {
    let (published, server) = messenger::create_pair()?;
    registry::register(mime::NAME, &published, &[mime::INTERFACE], 0)?;
    sys::write_str("mimed: registered as ");
    sys::write_str(mime::NAME);
    sys::write_str("\n");

    let mut db = MimeDb::builtin();
    let mut override_buffer = [0u8; OVERRIDE_BUFFER];
    if db.load_overrides(&mut override_buffer) {
        let source = db.source.clone().unwrap_or_default();
        let count = db.override_count();
        sys::write_str(&format!(
            "mimed: {count} override entry(ies) from {source}\n"
        ));
    } else {
        sys::write_str("mimed: built-in MIME table only\n");
    }

    let mut apps = AppRegistry::new();
    seed_default_apps(&mut apps);
    selftest(&db, &mut apps);
    sys::write_str("mimed: serving\n");

    let mut bus: Option<router::Bus> = None;
    // One receive buffer for the life of the service: the user bump allocator
    // never reclaims memory, so the loop must not allocate one per request.
    let mut buffer = alloc::vec![0u8; messenger::DEFAULT_BUFFER];
    loop {
        let message = server.recv_with(&mut buffer, None)?;
        let reply = match dispatch(&db, &mut apps, &mut bus, &message) {
            Ok(parcel) => parcel,
            // A malformed request still gets an answer, or its caller would
            // wait forever.
            Err(error) => mime::error_reply(message.method(), error),
        };
        if let Some(txn) = message.txn {
            // A failed reply means the caller timed out and its transaction is
            // gone; that is a normal race, not a fatal service error.
            let _ = server.reply(txn, &reply);
        }
    }
}

/// Seed the open-with defaults.
fn seed_default_apps(apps: &mut AppRegistry) {
    for (mime_type, app, verbs) in DEFAULT_APPS {
        for verb in *verbs {
            apps.register(mime_type, app, verb);
        }
    }
}

/// Dispatch one inbound message on the MIME interface.
fn dispatch(
    db: &MimeDb,
    apps: &mut AppRegistry,
    bus: &mut Option<router::Bus>,
    message: &Message,
) -> messenger::Result<Parcel> {
    if message.interface_id() != mime::INTERFACE {
        return Err(Error::Errno(-errno::EINVAL));
    }
    match message.method() {
        mime::method::GUESS => {
            let path = mime::string_field(&message.parcel, mime::field::PATH)?;
            mime::guess_reply(&db.guess(&path))
        }
        mime::method::LOOKUP => {
            let mime_type = mime::string_field(&message.parcel, mime::field::MIME)?;
            let verb = mime::string_field(&message.parcel, mime::field::VERB)?;
            mime::lookup_reply(apps.lookup(&mime_type, &verb))
        }
        mime::method::VERBS => {
            let mime_type = mime::string_field(&message.parcel, mime::field::MIME)?;
            mime::verbs_reply(&apps.verbs(&mime_type))
        }
        mime::method::OPEN => {
            let path = mime::string_field(&message.parcel, mime::field::PATH)?;
            let verb = mime::string_field(&message.parcel, mime::field::VERB)?;
            let result = open_path(db, apps, bus, &path, &verb)?;
            mime::open_reply(&result)
        }
        mime::method::REGISTER => {
            let mime_type = mime::string_field(&message.parcel, mime::field::MIME)?;
            let app = mime::string_field(&message.parcel, mime::field::APP)?;
            let verb = mime::string_field(&message.parcel, mime::field::VERB)?;
            if !valid_mime(&mime_type) || !valid_token(&app) || !valid_token(&verb) {
                return Err(Error::Errno(-errno::EINVAL));
            }
            apps.register(&mime_type, &app, &verb);
            Ok(mime::ok_reply(mime::method::REGISTER))
        }
        _ => Err(Error::Errno(-errno::EINVAL)),
    }
}

/// Guess the path, resolve the app (`verb`, then the default verb), and
/// publish the launch event on `init`'s router.
fn open_path(
    db: &MimeDb,
    apps: &AppRegistry,
    bus: &mut Option<router::Bus>,
    path: &str,
    verb: &str,
) -> messenger::Result<mime::OpenResult> {
    let mime_type = db.guess(path);
    let app = apps
        .lookup(&mime_type, verb)
        .or_else(|| apps.lookup(&mime_type, mime::DEFAULT_VERB))
        .ok_or(Error::Errno(-errno::ENOENT))?
        .to_string();
    let topic = format!("system/events/open/{app}");
    let payload = format!("path={path} mime={mime_type} verb={verb}");
    let published = publish_event(bus, &topic, &payload);
    Ok(mime::OpenResult {
        app,
        mime: mime_type,
        topic,
        published,
    })
}

/// Publish a fire-and-forget launch event.
///
/// `init`'s topic router is shared by every client, and the kernel refuses a
/// second synchronous call on a channel while another transaction is still
/// open (`-EDEADLK`); at boot, `logd`/`healthd` are subscribing while this
/// service starts, so a publish can race one of their calls. Retry that
/// specific error (the pending call clears promptly), and give up when the
/// router is unreachable.
fn publish_event(bus: &mut Option<router::Bus>, topic: &str, payload: &str) -> bool {
    const ATTEMPTS: usize = 32;
    for _ in 0..ATTEMPTS {
        if bus.is_none() {
            *bus = router::Bus::connect(services::INIT_NAME).ok();
        }
        let Some(active) = bus else {
            park_tick();
            continue;
        };
        match active.publish(topic, payload.as_bytes(), false) {
            Ok(()) => return true,
            Err(Error::Errno(code)) if code == -errno::EDEADLK => park_tick(),
            Err(_) => return false,
        }
    }
    false
}

/// The boot self-test: database guesses, a registry round trip, and the open
/// walk. Every check prints one machine-parseable marker.
fn selftest(db: &MimeDb, apps: &mut AppRegistry) {
    for (path, expected) in [
        ("NOTES.TXT", "text/plain"),
        ("README.MD", "text/markdown"),
        ("MAIN.RS", "text/x-rust"),
        ("APP.ELF", "application/x-elf"),
        ("LOGO.PNG", "image/png"),
        ("DATA.BIN", mime::FALLBACK_MIME),
    ] {
        let got = db.guess(path);
        if got == expected {
            sys::write_str(&format!("MIME:GUESS:PASS {path} {got}\n"));
        } else {
            sys::write_str(&format!(
                "MIME:GUESS:FAIL {path} got={got} want={expected}\n"
            ));
        }
    }
    // The override file is shipped in the services image; a custom image
    // without it still passes, with an informational line instead.
    match &db.source {
        Some(source) => {
            let path = "SAMPLE.LZT";
            let expected = "text/x-lazy-test";
            let got = db.guess(path);
            if got == expected {
                sys::write_str(&format!("MIME:GUESS:PASS {path} {got} ({source})\n"));
            } else {
                sys::write_str(&format!(
                    "MIME:GUESS:FAIL {path} got={got} want={expected} ({source})\n"
                ));
            }
        }
        None => sys::write_str("MIME:GUESS:INFO no override file; built-ins only\n"),
    }

    apps.register("text/x-lazy-test", "lazytest", "open");
    apps.register("text/x-lazy-test", "lazytest", "edit");
    let looked_up = apps.lookup("text/x-lazy-test", "open") == Some("lazytest");
    let verbs = apps.verbs("text/x-lazy-test");
    let verbs_ok =
        verbs.iter().any(|verb| verb == "open") && verbs.iter().any(|verb| verb == "edit");
    if looked_up && verbs_ok {
        sys::write_str("MIME:REGISTER:PASS\n");
    } else {
        sys::write_str("MIME:REGISTER:FAIL lookup or verbs mismatch\n");
    }

    let mut bus = None;
    for (path, expected) in [
        ("NOTES.TXT", "editor"),
        ("LOGO.PNG", "viewer"),
        ("SAMPLE.LZT", "lazytest"),
    ] {
        match open_path(db, apps, &mut bus, path, mime::DEFAULT_VERB) {
            Ok(result) if result.app == expected => {
                sys::write_str(&format!("MIME:OPEN:PASS {path} {}\n", result.app));
                if !result.published {
                    sys::write_str(&format!(
                        "MIME:OPEN:INFO {path} resolved; launch event not published\n"
                    ));
                }
            }
            Ok(result) => sys::write_str(&format!(
                "MIME:OPEN:FAIL {path} got={} want={expected}\n",
                result.app
            )),
            Err(error) => sys::write_str(&format!("MIME:OPEN:FAIL {path} {}\n", error.message())),
        }
    }
}

/// Sleep one PIT tick by parking on a private channel pair with an expired
/// deadline (userspace has no sleep syscall). The pair is closed again so no
/// channel leaks.
fn park_tick() {
    if let Ok((probe, peer)) = messenger::create_pair() {
        let mut scratch = [0u8; 16];
        let _ = probe.recv_into(&mut scratch, Some(messenger::EXPIRED_DEADLINE));
        let _ = probe.close();
        let _ = peer.close();
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
