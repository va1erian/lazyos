//! Boot reconciliation. The `confd` rows, `/apps` and `/docs/apps` survive a
//! reboot, but the kernel's policy and `mimed`'s registrations do not, so at
//! startup `pkgd` replays every installed app's stored manifest through the
//! same [`activate`](Pkgd::activate) an install uses. It also repairs what a
//! stop in the middle of a documentation replacement left in `/docs/apps`
//! (`pkgstore::tree::repair_docs`).

use alloc::format;
use alloc::string::String;

use lazypkg::Manifest;
use pkgstore::{layout, tree};
use user::files;
use user::messenger::pkgd::Installed;
use user::sys;

use super::handlers::Pkgd;
use super::install::{one_line, row_of};
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
                Ok(repairs) => sys::write_str(&format!("PKGD:DOCS:REPAIRED n={repairs}\n")),
                Err(error) => {
                    sys::write_str(&format!("PKGD:DOCS:FAIL {}\n", one_line(&describe(&error))))
                }
            }
        }
        let rows = match self.registry.list() {
            Ok(rows) => rows,
            Err(error) => {
                sys::write_str(&format!(
                    "PKGD:RECONCILE:SKIP the application list is unavailable: {}\n",
                    error.message()
                ));
                return;
            }
        };
        let mut activated = 0;
        for row in &rows {
            let present = layout::install_path(&row.install_dir)
                .map(|path| store::exists(&path))
                .unwrap_or(false);
            if !present {
                sys::write_str(&format!("PKGD:RECONCILE:MISSING {}\n", row.system_name));
                if volume_ok {
                    let _ = self.registry.delete(&row.system_name);
                }
                continue;
            }
            let outcome = stored_manifest(&row.install_dir).and_then(|manifest| {
                self.activate(&manifest)?;
                self.refresh_row(row, &manifest);
                Ok(())
            });
            match outcome {
                Ok(()) => activated += 1,
                Err(text) => sys::write_str(&format!(
                    "PKGD:RECONCILE:FAIL {}: {}\n",
                    row.system_name,
                    one_line(&text)
                )),
            }
        }
        sys::write_str(&format!(
            "PKGD:RECONCILE:PASS n={activated} of={}\n",
            rows.len()
        ));
    }
}

impl Pkgd {
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
