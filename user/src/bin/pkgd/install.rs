//! `Install`, `Remove` and boot reconciliation: the state-changing flows.
//!
//! **Build everything before switching.** An install extracts the whole package
//! into its own directory first; only then does it record the app, register its
//! file types and load its policy, so a failure at any step leaves the previous
//! state intact and the new directory is deleted again. An upgrade installs the
//! new directory beside the old one, switches the record and the policy, and
//! only then deletes the old directory.
//!
//! **Reconciliation.** The `confd` rows and `/data/apps` survive a reboot, but
//! the kernel's policy and `mimed`'s registrations do not, so at startup `pkgd`
//! replays every installed app's stored manifest through the same
//! [`activate`](Pkgd::activate) an install uses.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use lazypkg::{Manifest, Package};
use pkgstore::access::Caller;
use pkgstore::layout;
use user::files;
use user::messenger::pkgd::{Failure, Installed, PkgEvent};
use user::sys;

use super::handlers::{
    fail, read_failure, registry_down, Pkgd, EEXIST, EINVAL, EIO, ENODEV, ENOENT, EPERM,
};
use super::inspect::assess;
use super::policy;
use super::store;

/// Largest stored manifest read back (the format's own cap is 1 MiB).
const MAX_MANIFEST: usize = 1024 * 1024;

/// What an install is about, for audit records.
struct Subject {
    system_name: String,
    version: String,
    install_dir: String,
    digest: String,
}

impl Subject {
    fn none() -> Subject {
        Subject {
            system_name: String::new(),
            version: String::new(),
            install_dir: String::new(),
            digest: String::new(),
        }
    }

    fn of_row(row: &Installed) -> Subject {
        Subject {
            system_name: row.system_name.clone(),
            version: row.version.clone(),
            install_dir: row.install_dir.clone(),
            digest: row.digest.clone(),
        }
    }
}

/// One audit/serial record.
fn event(op: &str, subject: &Subject, uid: u64, ok: bool, detail: &str) -> PkgEvent {
    PkgEvent {
        op: String::from(op),
        system_name: subject.system_name.clone(),
        version: subject.version.clone(),
        install_dir: subject.install_dir.clone(),
        digest: subject.digest.clone(),
        actor_uid: uid,
        ok,
        detail: String::from(detail),
    }
}

/// The text of a serial marker: one line.
fn one_line(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

impl Pkgd {
    /// Refuse a request before anything changed: audited as `denied`.
    fn refuse(&mut self, subject: &Subject, uid: u64, marker: &str, failure: Failure) -> Failure {
        self.audit
            .record(&event("denied", subject, uid, false, &failure.text));
        sys::write_str(&format!("PKGD:{marker}:FAIL {}\n", one_line(&failure.text)));
        failure
    }

    /// `Install(path)`.
    pub(crate) fn install(&mut self, caller: &Caller, path: &str) -> Result<Installed, Failure> {
        let uid = u64::from(caller.uid);
        let nobody = Subject::none();
        if let Err(why) = pkgstore::access::may_manage(caller) {
            return Err(self.refuse(&nobody, uid, "INSTALL", fail(EPERM, why)));
        }
        if let Err(failure) = self.check_source(caller, path) {
            return Err(self.refuse(&nobody, uid, "INSTALL", failure));
        }
        if !store::data_mounted() || store::prepare_volume().is_err() {
            let failure = fail(
                ENODEV,
                "Applications cannot be installed because there is no writable data disk",
            );
            return Err(self.refuse(&nobody, uid, "INSTALL", failure));
        }
        // Never trust an earlier Inspect: read and validate again.
        if let Err(code) = store::read_package(&mut self.buffer, path) {
            return Err(self.refuse(&nobody, uid, "INSTALL", read_failure(code)));
        }
        let bytes = core::mem::take(&mut self.buffer);
        let outcome = self.install_bytes(&bytes, uid);
        self.buffer = bytes;
        outcome
    }

    fn install_bytes(&mut self, bytes: &[u8], uid: u64) -> Result<Installed, Failure> {
        let assessed = match assess(bytes) {
            Ok(assessed) => assessed,
            Err(info) => {
                let failure = fail(EINVAL, problem_text(&info.problems));
                return Err(self.refuse(&Subject::none(), uid, "INSTALL", failure));
            }
        };
        let info = &assessed.info;
        let subject = Subject {
            system_name: info.system_name.clone(),
            version: info.version.clone(),
            install_dir: info.install_dir.clone(),
            digest: info.digest.clone(),
        };
        if !info.problems.is_empty() {
            let failure = fail(EINVAL, problem_text(&info.problems));
            return Err(self.refuse(&subject, uid, "INSTALL", failure));
        }
        let package = &assessed.package;
        let system_name = &info.system_name;
        let previous = match self.registry.get(system_name) {
            Ok(previous) => previous,
            Err(error) => {
                return Err(self.refuse(&subject, uid, "INSTALL", registry_down(error)));
            }
        };
        if previous
            .as_ref()
            .is_some_and(|row| row.digest == info.digest)
        {
            let failure = fail(
                EEXIST,
                format!("{} is already installed at this version", info.name),
            );
            return Err(self.refuse(&subject, uid, "INSTALL", failure));
        }
        match self.install_package(package, &subject, previous, uid) {
            Ok(row) => {
                self.audit
                    .record(&event("install", &subject, uid, true, ""));
                sys::write_str(&format!(
                    "PKGD:INSTALL:PASS {} {}\n",
                    row.system_name, row.install_dir
                ));
                Ok(row)
            }
            Err(failure) => {
                self.audit
                    .record(&event("install", &subject, uid, false, &failure.text));
                sys::write_str(&format!("PKGD:INSTALL:FAIL {}\n", one_line(&failure.text)));
                Err(failure)
            }
        }
    }

    /// Extract, record, register, load policy; undo everything on a failure.
    fn install_package(
        &mut self,
        package: &Package<'_>,
        subject: &Subject,
        previous: Option<Installed>,
        _uid: u64,
    ) -> Result<Installed, Failure> {
        let manifest = package.manifest();
        let install_path = layout::install_path(&subject.install_dir)
            .map_err(|error| fail(EINVAL, format!("{error}")))?;
        // A directory left by an earlier interrupted attempt is not an install.
        store::remove_tree(&install_path).map_err(|code| {
            fail(
                EIO,
                format!("clearing {install_path}: {}", files::describe(code)),
            )
        })?;
        if let Err(error) = store::extract(package, &install_path) {
            self.discard(&subject.system_name, &install_path);
            return Err(fail(EIO, format!("Installing failed while {}", error.step)));
        }
        let row = Installed {
            system_name: manifest.app.system_name.clone(),
            name: manifest.app.name.clone(),
            version: manifest.app.version.clone(),
            install_dir: subject.install_dir.clone(),
            digest: subject.digest.clone(),
            binary: manifest.entry.binary.clone(),
            installed_at: sys::clock(),
            abi: String::from(if manifest.entry.is_linux() {
                "linux"
            } else {
                "native"
            }),
            args: manifest.entry.args.clone(),
        };
        if let Err(error) = self.registry.put(&row) {
            self.discard(&subject.system_name, &install_path);
            return Err(fail(
                EIO,
                format!(
                    "Installing failed while recording the application: {}",
                    error.message()
                ),
            ));
        }
        if let Err(text) = self.activate(manifest) {
            self.roll_back(&row.system_name, previous.as_ref());
            self.discard(&subject.system_name, &install_path);
            return Err(fail(EIO, format!("Installing failed while {text}")));
        }
        if let Some(old) = previous {
            self.retire(&old, manifest);
        }
        Ok(row)
    }

    /// Register the app's file types with `mimed` and load its policy: the
    /// "switch" half of an install, shared with boot reconciliation. `Err` is
    /// the step that failed, as text; nothing registered stays registered.
    pub(crate) fn activate(&mut self, manifest: &Manifest) -> Result<(), String> {
        let app = manifest.app.system_name.as_str();
        let mut registered: Vec<(&str, &str)> = Vec::new();
        for handler in &manifest.mime {
            for verb in &handler.verbs {
                if let Err(error) = self.registry.mime_register(&handler.mime_type, app, verb) {
                    self.unregister_all(app, &registered);
                    return Err(format!(
                        "registering the {} file type: {}",
                        handler.mime_type,
                        error.message()
                    ));
                }
                registered.push((handler.mime_type.as_str(), verb.as_str()));
            }
        }
        if let Err(error) = policy::load(manifest) {
            self.unregister_all(app, &registered);
            return Err(format!("loading its permissions: {}", error.text()));
        }
        Ok(())
    }

    fn unregister_all(&mut self, app: &str, registered: &[(&str, &str)]) {
        for (mime, verb) in registered {
            let _ = self.registry.mime_unregister(mime, app, verb);
        }
    }

    /// Put the world back as it was before a failed install: the previous row
    /// and its registrations (an upgrade), or nothing (a first install).
    fn roll_back(&mut self, system_name: &str, previous: Option<&Installed>) {
        match previous {
            Some(old) => {
                let _ = self.registry.put(old);
                if let Ok(manifest) = stored_manifest(&old.install_dir) {
                    let _ = self.activate(&manifest);
                }
            }
            None => {
                let _ = self.registry.delete(system_name);
                let _ = policy::revoke(system_name);
            }
        }
    }

    /// Delete a half-built or superseded install directory, and the app
    /// directory above it when that left it empty.
    fn discard(&mut self, system_name: &str, install_path: &str) {
        let _ = store::remove_tree(install_path);
        if let Ok(app_dir) = layout::app_dir(system_name) {
            store::remove_if_empty(&app_dir);
        }
    }

    /// After an upgrade took effect: withdraw the old version's file types the
    /// new one no longer handles, then delete the old directory.
    fn retire(&mut self, old: &Installed, new: &Manifest) {
        if let Ok(old_manifest) = stored_manifest(&old.install_dir) {
            for handler in &old_manifest.mime {
                for verb in &handler.verbs {
                    let kept = new.mime.iter().any(|m| {
                        m.mime_type == handler.mime_type && m.verbs.iter().any(|v| v == verb)
                    });
                    if !kept {
                        let _ = self.registry.mime_unregister(
                            &handler.mime_type,
                            &old.system_name,
                            verb,
                        );
                    }
                }
            }
        }
        if let Ok(path) = layout::install_path(&old.install_dir) {
            if let Err(code) = store::remove_tree(&path) {
                sys::write_str(&format!(
                    "PKGD:UPGRADE:CLEANUP:FAIL {path}: {}\n",
                    files::describe(code)
                ));
            }
        }
    }

    /// `Remove(system_name)`.
    pub(crate) fn remove(&mut self, caller: &Caller, system_name: &str) -> Result<(), Failure> {
        let uid = u64::from(caller.uid);
        let mut subject = Subject::none();
        subject.system_name = String::from(system_name);
        if let Err(why) = pkgstore::access::may_manage(caller) {
            return Err(self.refuse(&subject, uid, "REMOVE", fail(EPERM, why)));
        }
        if !layout::valid_system_name(system_name) {
            let failure = fail(EINVAL, "that is not a valid application name");
            return Err(self.refuse(&subject, uid, "REMOVE", failure));
        }
        let row = match self.registry.get(system_name) {
            Ok(Some(row)) => row,
            Ok(None) => {
                let failure = fail(ENOENT, "that application is not installed");
                return Err(self.refuse(&subject, uid, "REMOVE", failure));
            }
            Err(error) => return Err(self.refuse(&subject, uid, "REMOVE", registry_down(error))),
        };
        let subject = Subject::of_row(&row);
        match self.remove_row(&row) {
            Ok(()) => {
                self.audit.record(&event("remove", &subject, uid, true, ""));
                sys::write_str(&format!("PKGD:REMOVE:PASS {}\n", row.system_name));
                Ok(())
            }
            Err(failure) => {
                self.audit
                    .record(&event("remove", &subject, uid, false, &failure.text));
                sys::write_str(&format!("PKGD:REMOVE:FAIL {}\n", one_line(&failure.text)));
                Err(failure)
            }
        }
    }

    fn remove_row(&mut self, row: &Installed) -> Result<(), Failure> {
        // Stop it first: nothing running should outlive its files and policy.
        match self.registry.stop(&row.system_name) {
            Ok(count) => sys::write_str(&format!(
                "PKGD:REMOVE:STOPPED {} {count}\n",
                row.system_name
            )),
            Err(error) => sys::write_str(&format!(
                "PKGD:REMOVE:STOP:FAIL {}: {}\n",
                row.system_name,
                error.message()
            )),
        }
        if let Ok(manifest) = stored_manifest(&row.install_dir) {
            for handler in &manifest.mime {
                for verb in &handler.verbs {
                    let _ =
                        self.registry
                            .mime_unregister(&handler.mime_type, &row.system_name, verb);
                }
            }
        }
        policy::revoke(&row.system_name).map_err(|error| {
            fail(
                EIO,
                format!(
                    "Removing failed while revoking its permissions: {}",
                    error.message()
                ),
            )
        })?;
        let path = layout::install_path(&row.install_dir)
            .map_err(|error| fail(EINVAL, format!("{error}")))?;
        store::remove_tree(&path).map_err(|code| {
            fail(
                EIO,
                format!(
                    "Removing failed while deleting its files: {}",
                    files::describe(code)
                ),
            )
        })?;
        if let Ok(app_dir) = layout::app_dir(&row.system_name) {
            store::remove_if_empty(&app_dir);
        }
        self.registry.delete(&row.system_name).map_err(|error| {
            fail(
                EIO,
                format!(
                    "Removing failed while forgetting the application: {}",
                    error.message()
                ),
            )
        })
    }

    /// Replay every installed app's registrations and policy after a boot (the
    /// kernel and `mimed` keep them in memory only). A row whose files are gone
    /// is dropped, unless the volume itself is absent.
    pub(crate) fn reconcile(&mut self, volume_ok: bool) {
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
            match stored_manifest(&row.install_dir) {
                Ok(manifest) => match self.activate(&manifest) {
                    Ok(()) => activated += 1,
                    Err(text) => sys::write_str(&format!(
                        "PKGD:RECONCILE:FAIL {}: {}\n",
                        row.system_name,
                        one_line(&text)
                    )),
                },
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

/// The manifest `pkgd` stored when it installed `install_dir`.
fn stored_manifest(install_dir: &str) -> Result<Manifest, String> {
    let root = layout::install_path(install_dir).map_err(|error| format!("{error}"))?;
    let path = format!("{root}/{}", layout::MANIFEST_FILE);
    let bytes = files::read_up_to(&path, MAX_MANIFEST)
        .map_err(|code| format!("{path}: {}", files::describe(code)))?;
    let text = String::from_utf8(bytes).map_err(|_| format!("{path} is not text"))?;
    lazypkg::parse_manifest(&text).map_err(|error| format!("{error}"))
}

/// The friendly text for a package that cannot be installed.
fn problem_text(problems: &[String]) -> String {
    let mut text = String::from("This package cannot be installed: ");
    for (index, problem) in problems.iter().enumerate() {
        if index > 0 {
            text.push_str("; ");
        }
        text.push_str(problem);
    }
    text
}
