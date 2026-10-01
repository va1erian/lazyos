//! `pkgd` (`PKGD.ELF`): the application package manager (`docs/packages.md`,
//! phase 3).
//!
//! `pkgd` is the only task that writes `/data/apps`, records installed apps in
//! `confd`, registers their file types with `mimed` and loads their Messenger
//! policy into the kernel. It serves `os.lazy.pkgd.v1` (`idl/pkgd.midl`):
//! `Inspect` (validate a `.lzp`, list what it asks for, change nothing),
//! `Install`, `Remove`, `List` and `Installed`. A GUI installer is an
//! unprivileged client that shows the user the `Inspect` result and forwards the
//! user's yes as `Install`; `pkgd` re-validates and re-checks the caller itself.
//!
//! # Privilege
//!
//! `init` spawns `pkgd` with its own identity: **root with every capability
//! except raw input**. That is deliberate and minimal in effect: `CAP_IPC_CONTROL`
//! is what the kernel's `acl_load` operation needs to give an installed app its
//! policy, and uid 0 is what writes `/data/apps` and `sys/apps` in `confd` and
//! calls `mimed.Register`/`Unregister`. Because it is root, every request is
//! checked against the kernel-stamped identity of the sender
//! (`pkgstore::access`): only root or the owner of a login session may install or
//! remove, a sandboxed application never may, and an unprivileged caller may
//! only name package files in places it could read itself.
//!
//! # What lives where
//!
//! * `/data/apps/<system_name>/<version>-<digest8>/`: the extracted package;
//! * `confd` `sys/apps/<system_name>`: one generated `Installed` record per app;
//! * the kernel: the label `app:<system_name>` and its rules (in memory only, so
//!   `pkgd` replays them at startup, see `install::reconcile`);
//! * `/data/log/pkg.log`: the hash-chained audit trail (`pkgstore::audit`), also
//!   published as `system/events/pkg/<op>`.
//!
//! # Boot evidence
//!
//! `PKGD:UP:PASS`, `PKGD:AUDIT:PASS n=<count>` (or `FAIL`), `PKGD:RECONCILE:PASS`,
//! `PKGD:INSTALL:PASS <system_name> <install_dir>` / `PKGD:INSTALL:FAIL <why>`,
//! `PKGD:REMOVE:PASS <system_name>` / `...:FAIL`, and `PKGD:STORE:ABSENT` when
//! there is no writable data disk (`Inspect` and `List` still answer).
//!
//! # Memory
//!
//! The user heap never returns blocks over 64 KiB, and extracting a package
//! allocates its largest file. So `pkgd` ends itself (and `init`, which supervises
//! it with `Restart::Always`, starts a fresh one) once its heap has grown past
//! [`RECYCLE_BYTES`] and it is idle between two requests.

#![no_std]
#![no_main]

extern crate alloc;

#[path = "pkgd/audit.rs"]
mod audit;
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
#[path = "pkgd/registry.rs"]
mod registry;
#[path = "pkgd/store.rs"]
mod store;

use core::panic::PanicInfo;
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
    names::register(pkgd::NAME, &published, &[pkgd::INTERFACE], 0)?;

    let mut state = Pkgd::new();
    let volume_ok = store::data_mounted() && store::prepare_volume().is_ok();
    if volume_ok {
        state.audit.load();
    } else {
        state.audit.set_volatile();
        sys::write_str("PKGD:STORE:ABSENT there is no writable data disk; installs are refused\n");
    }
    sys::write_str("PKGD:UP:PASS\n");
    state.reconcile(volume_ok);

    // One receive buffer for the life of the service: the heap never reclaims
    // large per-request blocks.
    let mut buffer = alloc::vec![0u8; messenger::DEFAULT_BUFFER];
    loop {
        let message = server.recv_with(&mut buffer, None)?;
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

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
