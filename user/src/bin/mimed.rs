//! `mimed` (`/system/bin/mimed`): the MIME database and open-with registry (issue
//! #116), and the shell-integration middle of the S4 stack.
//!
//! `mimed` owns two tables:
//!
//! * a **MIME database**: a built-in extension/filename table (`.txt`, `.md`,
//!   `.rs`, `.elf`, `.png`, `Makefile`, ...) plus a `mime.types`-style
//!   override read through the native file API at boot from
//!   `/system/share/mime.types` (`fhs::share::MIME_TYPES`), which the image
//!   ships;
//! * an **open-with registry** mapping `(mime, verb)` to an app id, with the
//!   shell verbs `open`, `edit` and `reveal` seeded for the built-in types and
//!   `Register`/`Lookup`/`Verbs`/`Open` served over Messenger.
//!
//! ## Launch path
//!
//! `Open` resolves the app and, when the supervisor (`init`) is reachable,
//! additionally calls `os.lazy.init`'s `Launch(app, path, session)` (issue
//! #158): the app id comes from the open-with registry, the argument is the
//! opened path, and the session is the caller's kernel-stamped session (read
//! through `sys::cred_get`, which the service's root identity permits). The
//! call is best-effort and gated: when `init` is absent, the app id is unknown
//! to its registry, or the program is not installed, `Open` falls back to the
//! original publish-only behavior. Either way `Open` publishes a
//! fire-and-forget `system/events/open/<app>` event on `messengerd`'s central
//! broker ([`user::central`]) - the broker `logd` subscribes to - carrying the
//! typed `OpenEvent { path, mime, verb }` payload (`idl/mimed.midl`). An app
//! id is a short lowercase name (`editor` is `/system/bin/editor`), the same
//! ids `init`'s app registry serves (`ListApps`). `messengerctl log` still shows the event
//! as the observable launch record.
//!
//! ## Boot evidence
//!
//! The service self-tests its database, registry and open walk at startup and
//! prints machine-parseable markers, so a headless boot proves the path:
//! `MIME:GUESS:PASS <path> <mime>`, `MIME:REGISTER:PASS` and
//! `MIME:OPEN:PASS <path> <app>` (with `MIME:...:FAIL` lines if a check
//! breaks). The open walk attempts the `init` launch too; a not-yet-installed
//! app (`/system/bin/editor`) exercises the fallback and still prints `MIME:OPEN:PASS`.
//!
//! `init` starts the service from its manifest.

#![no_std]
#![no_main]

extern crate alloc;

#[path = "mimed/apps.rs"]
mod apps;
#[path = "mimed/db.rs"]
mod db;
#[path = "mimed/handlers.rs"]
mod handlers;
#[path = "mimed/selftest.rs"]
mod selftest;
#[path = "mimed/validate.rs"]
mod validate;

use alloc::format;
use core::panic::PanicInfo;
use user::central;
use user::messenger::{self, mime, registry};
use user::sys;

use apps::{seed_default_apps, AppRegistry};
use db::{MimeDb, OVERRIDE_BUFFER};
use handlers::dispatch;
use selftest::selftest;

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
    // Serving: what waits for this service may start (init.Ready, P7.3).
    user::messenger::services::init::notify_ready();

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

    let mut bus: Option<central::Bus> = None;
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

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
