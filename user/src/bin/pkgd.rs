//! `pkgd` (`/system/bin/pkgd`): the application package manager (`docs/packages.md`,
//! phase 3).
//!
//! `pkgd` is the only task that writes `/apps` and `/docs/apps`, records
//! installed apps in `confd`, registers their file types with `mimed` and loads
//! their Messenger policy into the kernel. It serves `os.lazy.pkgd.v1` (`idl/pkgd.midl`):
//! `Inspect` (validate a `.lzp`, list what it asks for, change nothing),
//! `Install`, `Remove`, `List`, `Installed` and `Develop` (approve the rules of
//! a development run, `dev:<system_name>`; see `develop`). A GUI installer is an
//! unprivileged client that shows the user the `Inspect` result and forwards the
//! user's yes as `Install`; `pkgd` re-validates and re-checks the caller itself.
//!
//! # Privilege
//!
//! `init` spawns `pkgd` with its own identity: **root with every capability
//! except raw input**. That is deliberate and minimal in effect: `CAP_IPC_CONTROL`
//! is what the kernel's `acl_load` operation needs to give an installed app its
//! policy, and uid 0 is what writes `/apps`, `/docs/apps`, `/logs/pkg.log` and
//! `sys/apps` in `confd` and calls `mimed.Register`/`Unregister`. Because it is
//! root, every request is checked against the kernel-stamped identity of the
//! sender (`pkgstore::access`): only root or the owner of a login session may
//! install or remove, a sandboxed application never may, and an unprivileged
//! caller may only install from `/transient` or its own home folder.
//!
//! # What lives where
//!
//! * `/apps/<system_name>/<version>-<digest8>/`: the extracted package;
//! * `/docs/apps/<system_name>/`: its `docs/**.md`, replaced whole on upgrade
//!   and deleted on removal (`pkgstore::tree`);
//! * `confd` `sys/apps/<system_name>`: one generated `Installed` record per app;
//! * the kernel: the label `app:<system_name>` and its rules (in memory only, so
//!   `pkgd` replays them at startup, see `reconcile`);
//! * `/logs/pkg.log`: the hash-chained audit trail (`pkgstore::audit`), also
//!   published as `system/events/pkg/<op>`; `logd`'s rotation leaves it alone.
//!
//! # Boot evidence
//!
//! `PKGD:UP:PASS`, `PKGD:AUDIT:PASS n=<count>` (or `FAIL`), `PKGD:RECONCILE:PASS`,
//! `PKGD:PROVISION:DONE installed=<n> upgraded=<n> kept=<n> failed=<n> free=<bytes>` (core
//! packages, see `provision`),
//! `PKGD:PROVISION:DONE installed=<n> upgraded=<n> kept=<n> failed=<n> free=<bytes>` (see
//! `provision`),
//! `PKGD:INSTALL:PASS <system_name> <install_dir>` / `PKGD:INSTALL:FAIL <why>`,
//! `PKGD:REMOVE:PASS <system_name>` / `...:FAIL`, `PKGD:STORE:ABSENT reason=<..>`
//! when `/apps`, `/docs/apps` or `/logs` cannot be written (a recovery boot
//! with a read-only `/`; `Inspect` and `List` still answer, installs are
//! refused), and `PKGD:STOP` on shutdown.
//!
//! # Shutdown
//!
//! `pkgd` serves `os.lazy.lifecycle.v1` (docs/shutdown.md): on `init`'s
//! `Shutdown` it finishes the request it is serving (every operation is
//! synchronous), fsyncs `/logs/pkg.log` so the tail of the hash chain is on
//! disk, prints `PKGD:STOP` and exits 0. It is stopped before `confd` and
//! `mimed`, which it depends on.
//!
//! # Memory
//!
//! The user heap never reuses a block over 1 MiB, and reading a package holds
//! the whole archive (one buffer, kept and reused, sized to the largest
//! package seen); its files are unpacked to disk through a 1 MiB window
//! (`lazypkg::CHUNK`), never whole. So a provisioning pass grows the heap by
//! about the largest package, and `pkgd` ends itself (and `init`, which
//! supervises it with `Restart::Always`, starts a fresh one) once its heap has
//! grown past [`RECYCLE_BYTES`] and it is idle between two requests.

#![no_std]
#![no_main]

extern crate alloc;

#[path = "pkgd/approval.rs"]
mod approval;
#[path = "pkgd/audit.rs"]
mod audit;
#[path = "pkgd/develop.rs"]
mod develop;
#[path = "pkgd/handlers.rs"]
mod handlers;
#[path = "pkgd/inspect.rs"]
mod inspect;
#[path = "pkgd/install.rs"]
mod install;
#[path = "pkgd/peers.rs"]
mod peers;
#[path = "pkgd/policy.rs"]
mod policy;
#[path = "pkgd/provision.rs"]
mod provision;
#[path = "pkgd/reconcile.rs"]
mod reconcile;
#[path = "pkgd/registry.rs"]
mod registry;
#[path = "pkgd/remove.rs"]
mod remove;
#[path = "pkgd/store.rs"]
mod store;

use alloc::format;
use alloc::string::String;
use core::panic::PanicInfo;
use pkgstore::layout;
use user::messenger::services::lifecycle;
use user::messenger::{self, pkgd, registry as names};
use user::sys;

use handlers::Pkgd;

/// Heap growth after which an idle `pkgd` restarts to give its memory back.
const RECYCLE_BYTES: u64 = 32 * 1024 * 1024;

#[no_mangle]
pub extern "C" fn _start() -> ! {
    sys::write_str("pkgd: application package manager\n");
    if let Err(error) = run() {
        sys::write_str("pkgd: fatal: ");
        sys::write_str(error.message());
        sys::write_str("\n");
        sys::exit(1);
    }
    sys::exit(0)
}

fn run() -> messenger::Result<()> {
    let start_break = sys::sbrk(0);
    let (published, server) = messenger::create_pair()?;
    names::register(
        pkgd::NAME,
        &published,
        &[pkgd::INTERFACE, lifecycle::INTERFACE],
        0,
    )?;
    // Serving: what waits for this service may start (init.Ready, P7.3).
    user::messenger::services::init::notify_ready();

    let mut state = Pkgd::new();
    let volume_ok = match store::probe_store() {
        Ok(()) => true,
        Err(reason) => {
            sys::write_str(&format!(
                "PKGD:STORE:ABSENT reason=\"{reason}\" installs are refused\n"
            ));
            false
        }
    };
    if volume_ok {
        state.audit.load();
    } else {
        state.audit.set_volatile();
    }
    sys::write_str("PKGD:UP:PASS\n");
    // Before any request: no development rule set outlives the approvals a
    // previous `pkgd` held in memory.
    state.revoke_stale_dev_labels();
    state.reconcile(volume_ok);
    state.begin_provisioning(volume_ok);

    // One receive buffer for the life of the service: the heap never reclaims
    // large per-request blocks.
    let mut buffer = alloc::vec![0u8; messenger::DEFAULT_BUFFER];
    loop {
        // While the core packages are provisioned, one step runs between two
        // polls, so `Provisioned` (and the read-only methods) still answer.
        // While a development label is approved, wake up now and then to
        // revoke the approvals of a session that logged out.
        state.watch_logouts();
        let message = if state.provisioned.done && state.holds_dev_approvals() {
            let deadline = Some(sys::clock() + develop::LOGOUT_POLL_TICKS);
            match server.recv_with(&mut buffer, deadline) {
                Ok(message) => message,
                Err(messenger::Error::Errno(code)) if code == -messenger::errno::ETIMEDOUT => {
                    continue
                }
                Err(error) => return Err(error),
            }
        } else if state.provisioned.done {
            server.recv_with(&mut buffer, None)?
        } else {
            state.provision_step();
            match server.poll_recv_with(&mut buffer)? {
                Some(message) => message,
                None => continue,
            }
        };
        // An orderly shutdown (docs/shutdown.md): every operation is
        // synchronous, so none is in flight between two messages.
        if let Some(reason) = lifecycle::stop_requested(&message) {
            stop(&reason);
            return Ok(());
        }
        let reply = state.dispatch(&message);
        if let Some(txn) = message.txn {
            // A failed reply means the caller timed out and its transaction is
            // gone: a normal race, not a fatal error.
            let _ = server.reply(txn, &reply);
        }
        if sys::sbrk(0).saturating_sub(start_break) > RECYCLE_BYTES {
            sys::write_str("PKGD:RECYCLE restarting to release memory\n");
            return Ok(());
        }
    }
}

/// The lifecycle stop: every audit record was appended before its reply, so
/// syncing `pkg.log` puts the whole hash chain on disk.
fn stop(reason: &str) {
    let synced = match user::files::fsync(layout::LOG_FILE) {
        Ok(()) => String::from("ok"),
        Err(code) if code == store::ENOENT => String::from("none"),
        Err(code) => format!("errno {code}"),
    };
    sys::write_str(&format!("PKGD:STOP sync={synced} reason=\"{reason}\"\n"));
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
