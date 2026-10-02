//! `Install`, `Remove` and boot reconciliation: the state-changing flows.
//!
//! **Build everything before switching.** An install extracts the whole package
//! into its own directory first; only then does it record the app, register its
//! file types and load its policy, so a failure at any step leaves the previous
//! state intact and the new directory is deleted again. An upgrade installs the
//! new directory beside the old one, switches the record and the policy, and
//! only then deletes the old directory.
//!
//! The documentation follows the same rule: the package's `docs/**.md` is
//! staged in `/docs/apps/<system_name>~new` with the extraction and replaces
//! the live copy only once the app is active (`pkgstore::tree`).
//!
//! Boot reconciliation is in [`reconcile`](super::reconcile), removal in
//! [`remove`](super::remove) and core package provisioning in
//! [`provision`](super::provision).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use lazypkg::{Manifest, Package};
use pkgstore::access::Caller;
use pkgstore::{layout, provision, tree};
use user::messenger::pkgd::{Failure, Installed, PkgEvent};
use user::sys;

use super::handlers::{
    fail, read_failure, registry_down, Pkgd, EAGAIN, EEXIST, EINVAL, EIO, ENODEV, EPERM,
    PROVISIONING,
};
use super::inspect::assess;
use super::policy;
use super::reconcile::stored_manifest;
use super::store::{self, describe, SysFs};

/// What an install is about, for audit records.
pub(crate) struct Subject {
    pub(crate) system_name: String,
    pub(crate) version: String,
    pub(crate) install_dir: String,
    pub(crate) digest: String,
}

impl Subject {
    pub(crate) fn none() -> Subject {
        Subject {
            system_name: String::new(),
            version: String::new(),
            install_dir: String::new(),
            digest: String::new(),
        }
    }

    pub(crate) fn of_row(row: &Installed) -> Subject {
        Subject {
            system_name: row.system_name.clone(),
            version: row.version.clone(),
            install_dir: row.install_dir.clone(),
            digest: row.digest.clone(),
        }
    }
}

/// One audit/serial record.
pub(crate) fn event(op: &str, subject: &Subject, uid: u64, ok: bool, detail: &str) -> PkgEvent {
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
pub(crate) fn one_line(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

impl Pkgd {
    /// Refuse a request before anything changed: audited as `denied`.
    pub(crate) fn refuse(
        &mut self,
        subject: &Subject,
        uid: u64,
        marker: &str,
        failure: Failure,
    ) -> Failure {
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
        if !self.provisioned.done {
            return Err(fail(EAGAIN, PROVISIONING));
        }
        let path = match self.check_source(caller, path) {
            Ok(path) => path,
            Err(failure) => return Err(self.refuse(&nobody, uid, "INSTALL", failure)),
        };
        if let Err(why) = store::probe_store() {
            let failure = fail(ENODEV, format!("Applications cannot be installed: {why}"));
            return Err(self.refuse(&nobody, uid, "INSTALL", failure));
        }
        // Never trust an earlier Inspect: read and validate again.
        if let Err(code) = store::read_package(&mut self.buffer, &path) {
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
        if let Some(shipped) = self.core_version(system_name) {
            if let Err(why) = provision::downgrade(&info.name, &shipped, &info.version) {
                return Err(self.refuse(&subject, uid, "INSTALL", fail(EPERM, why)));
            }
        }
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
        let origin = self.origin_of(system_name);
        match self.install_package(package, &subject, previous, origin) {
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
    /// `origin` is the row's `Origin` (core when the image ships the app).
    pub(crate) fn install_package(
        &mut self,
        package: &Package<'_>,
        subject: &Subject,
        previous: Option<Installed>,
        origin: u32,
    ) -> Result<Installed, Failure> {
        let manifest = package.manifest();
        let install_path = layout::install_path(&subject.install_dir)
            .map_err(|error| fail(EINVAL, format!("{error}")))?;
        // A directory left by an earlier interrupted attempt is not an install.
        tree::remove_tree(&mut SysFs, &install_path).map_err(|error| {
            fail(
                EIO,
                format!("clearing {install_path}: {}", describe(&error)),
            )
        })?;
        let mut scratch = core::mem::take(&mut self.scratch);
        let staged = tree::extract_with(&mut SysFs, package, &install_path, &mut scratch)
            .and_then(|_| tree::stage_docs(&mut SysFs, package, &subject.system_name));
        self.scratch = scratch;
        let staged = match staged {
            Ok(staged) => staged,
            Err(error) => {
                self.discard(&subject.system_name, &install_path);
                return Err(fail(
                    EIO,
                    format!("Installing failed while {}", describe(&error)),
                ));
            }
        };
        let row = row_of(
            manifest,
            &subject.install_dir,
            &subject.digest,
            sys::clock(),
            origin,
        );
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
        // The app is active: its documentation replaces the old one whole.
        if let Err(error) = tree::commit_docs(&mut SysFs, &row.system_name, staged) {
            sys::write_str(&format!("PKGD:DOCS:FAIL {}\n", one_line(&describe(&error))));
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
        match policy::load(manifest) {
            // The evidence that the kernel holds the app's label policy.
            Ok(rules) => sys::write_str(&format!("PKGD:POLICY:PASS app:{app} rules={rules}\n")),
            Err(error) => {
                self.unregister_all(app, &registered);
                return Err(format!("loading its permissions: {}", error.text()));
            }
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

    /// Delete a half-built install directory and its staged documentation,
    /// and the app directory above it when that left it empty.
    pub(crate) fn discard(&mut self, system_name: &str, install_path: &str) {
        let _ = tree::remove_tree(&mut SysFs, install_path);
        if let Ok(staging) = pkgstore::docs::staging_dir(system_name) {
            let _ = tree::remove_tree(&mut SysFs, &staging);
        }
        if let Ok(app_dir) = layout::app_dir(system_name) {
            tree::remove_if_empty(&mut SysFs, &app_dir);
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
            if let Err(error) = tree::remove_tree(&mut SysFs, &path) {
                sys::write_str(&format!(
                    "PKGD:UPGRADE:CLEANUP:FAIL {}\n",
                    one_line(&describe(&error))
                ));
            }
        }
    }
}

/// The `confd` row of an app installed from `manifest`.
pub(crate) fn row_of(
    manifest: &Manifest,
    install_dir: &str,
    digest: &str,
    installed_at: u64,
    origin: u32,
) -> Installed {
    let mut verbs: Vec<String> = Vec::new();
    for verb in manifest
        .mime
        .iter()
        .flat_map(|handler| handler.verbs.iter())
    {
        if !verbs.contains(verb) {
            verbs.push(verb.clone());
        }
    }
    Installed {
        system_name: manifest.app.system_name.clone(),
        name: manifest.app.name.clone(),
        version: manifest.app.version.clone(),
        install_dir: String::from(install_dir),
        digest: String::from(digest),
        binary: manifest.entry.binary.clone(),
        installed_at,
        abi: String::from(if manifest.entry.is_linux() {
            "linux"
        } else {
            "native"
        }),
        args: manifest.entry.args.clone(),
        origin,
        category: String::from(manifest.app.category().as_str()),
        autostart: manifest.entry.autostart,
        verbs,
    }
}

/// The friendly text for a package that cannot be installed.
pub(crate) fn problem_text(problems: &[String]) -> String {
    let mut text = String::from("This package cannot be installed: ");
    for (index, problem) in problems.iter().enumerate() {
        if index > 0 {
            text.push_str("; ");
        }
        text.push_str(problem);
    }
    text
}
