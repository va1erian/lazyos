//! Boot reconciliation. The `confd` rows, `/apps` and `/docs/apps` survive a
//! reboot, but the kernel's policy and `mimed`'s registrations do not, so at
//! startup `pkgd` replays every installed app's stored manifest through the
//! same [`activate`](Pkgd::activate) an install uses. It also repairs what a
//! stop in the middle of a documentation replacement left in `/docs/apps`
//! (`pkgstore::tree::repair_docs`).
//!
//! A developer tool is replayed only when its row says an administrator (or
//! the image) installed it and the stored manifest still says what the row
//! does (`pkgstore::toolgate::loadable`); otherwise its label is revoked, the
//! refusal audited and the row left for `Remove`.

use alloc::format;
use alloc::string::String;
use user::trace;

use lazypkg::Manifest;
use pkgstore::{layout, toolgate, tree};
use user::files;
use user::messenger::pkgd::Installed;

use super::handlers::Pkgd;
use super::install::{event, one_line, row_of, Installer, Subject};
use super::policy;
use super::store::{self, describe, SysFs};

/// Largest stored manifest read back (the format's own cap is 1 MiB).
const MAX_MANIFEST: usize = 1024 * 1024;

impl Pkgd {
    /// Replay every installed app's registrations and policy after a boot (the
    /// kernel and `mimed` keep them in memory only). A row whose files are gone
    /// is dropped, unless the store itself is unusable.
    pub(crate) fn reconcile(&mut self, volume_ok: bool) {
        if volume_ok {
            match tree::repair_docs(&mut SysFs) {
                Ok(0) => {}
                Ok(repairs) => trace::line!(trace::Channel::Pkgd, "DOCS:REPAIRED n={repairs}"),
                Err(error) => {
                    trace::line!(
                        trace::Channel::Pkgd,
                        "DOCS:FAIL {}",
                        one_line(&describe(&error))
                    )
                }
            }
        }
        let rows = match self.registry.list() {
            Ok(rows) => rows,
            Err(error) => {
                trace::line!(
                    trace::Channel::Pkgd,
                    "RECONCILE:SKIP the application list is unavailable: {}",
                    error.message()
                );
                return;
            }
        };
        // Core apps first: `mimed`'s registry is bounded and global, and a user
        // package activated before them must not be able to take the room
        // their handlers need (the confd list order is by name).
        let mut rows = rows;
        rows.sort_by_key(|row| row.origin != user::messenger::pkgd::wire::ORIGIN_CORE);
        let mut activated = 0;
        for row in &rows {
            let present = layout::install_path(&row.install_dir)
                .map(|path| store::exists(&path))
                .unwrap_or(false);
            if !present {
                trace::line!(
                    trace::Channel::Pkgd,
                    "RECONCILE:MISSING {}",
                    row.system_name
                );
                if volume_ok {
                    let _ = self.registry.delete(&row.system_name);
                }
                continue;
            }
            let outcome = stored_manifest(&row.install_dir).and_then(|manifest| {
                let installer = Installer::of_row(row);
                let core = row.origin == user::messenger::pkgd::wire::ORIGIN_CORE;
                // The same decision `init` makes before it starts the app: the
                // class agrees, a tool is approved, and a manifest that claims
                // the shell role (it lifts the label's credential denies)
                // belongs to a package the image ships.
                if let Err(why) =
                    toolgate::stored_loadable(&row.kind, &manifest, installer.machine_wide, core)
                {
                    self.deny_load(row, &manifest, why);
                    return Err(String::from(why));
                }
                self.activate(&manifest, installer.owner())?;
                self.refresh_row(row, &manifest);
                Ok(())
            });
            match outcome {
                Ok(()) => activated += 1,
                Err(text) => trace::line!(
                    trace::Channel::Pkgd,
                    "RECONCILE:FAIL {}: {}",
                    row.system_name,
                    one_line(&text)
                ),
            }
        }
        trace::line!(
            trace::Channel::Pkgd,
            "RECONCILE:PASS n={activated} of={}",
            rows.len()
        );
    }
}

impl Pkgd {
    /// A row whose app must not be loaded (`toolgate::loadable`): withdraw any
    /// policy a previous `pkgd` left in the kernel under either label the row
    /// or the stored manifest names, and audit the refusal.
    fn deny_load(&mut self, row: &Installed, manifest: &Manifest, why: &str) {
        let _ = policy::revoke_row(row);
        let _ = policy::revoke(manifest);
        self.audit
            .record(&event("denied", &Subject::of_row(row), 0, false, why));
    }

    /// Bring a row's manifest-derived fields (menu category, autostart,
    /// resident, MIME verbs) up to date with its stored manifest: a row
    /// recorded before they existed gets them without a reinstall.
    fn refresh_row(&mut self, row: &Installed, manifest: &Manifest) {
        let fresh = row_of(
            manifest,
            &row.install_dir,
            &row.digest,
            row.installed_at,
            row.origin,
            Installer::of_row(row),
        );
        if fresh != *row {
            let _ = self.registry.put(&fresh);
        }
    }
}

/// The manifest `pkgd` stored when it installed `install_dir`.
pub(crate) fn stored_manifest(install_dir: &str) -> Result<Manifest, String> {
    let root = layout::install_path(install_dir).map_err(|error| format!("{error}"))?;
    let path = format!("{root}/{}", layout::MANIFEST_FILE);
    let bytes = files::read_up_to(&path, MAX_MANIFEST)
        .map_err(|code| format!("{path}: {}", files::describe(code)))?;
    let text = String::from_utf8(bytes).map_err(|_| format!("{path} is not text"))?;
    lazypkg::parse_manifest(&text).map_err(|error| format!("{error}"))
}
