//! The "switch" half of an install and what undoes or retires it: registering
//! an app's file types and loading its policy (the app's label and each of
//! its child labels, issue #832), shared by `Install` and boot
//! reconciliation, plus the roll-back and the clean-up after an upgrade.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use user::trace;

use lazypkg::Manifest;
use pkgstore::{layout, tree};
use user::messenger::pkgd::Installed;

use super::handlers::Pkgd;
use super::install::one_line;
use super::policy;
use super::reconcile::stored_manifest;
use super::store::{describe, SysFs};

impl Pkgd {
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
            // The evidence that the kernel holds the app's label policy, and
            // that of each child label (issue #832).
            Ok(loaded) => {
                trace::line!(
                    trace::Channel::Pkgd,
                    "POLICY:PASS app:{app} rules={}",
                    loaded.rules
                );
                for (label, rules, program) in &loaded.children {
                    trace::line!(
                        trace::Channel::Pkgd,
                        "POLICY:CHILD:PASS {label} rules={rules} program={program}"
                    );
                }
            }
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
    pub(crate) fn roll_back(&mut self, system_name: &str, previous: Option<&Installed>) {
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

    /// After an upgrade took effect: withdraw the old version's file types and
    /// child labels the new one no longer has, then delete the old directory.
    pub(crate) fn retire(&mut self, old: &Installed, new: &Manifest) {
        if let Ok(old_manifest) = stored_manifest(&old.install_dir) {
            // A child label the new version no longer declares goes with it.
            let revoked = policy::revoke_dropped(&old_manifest, Some(new));
            if revoked > 0 {
                trace::line!(
                    trace::Channel::Pkgd,
                    "POLICY:CHILD:DROPPED {} n={revoked}",
                    old.system_name
                );
            }
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
                trace::line!(
                    trace::Channel::Pkgd,
                    "UPGRADE:CLEANUP:FAIL {}",
                    one_line(&describe(&error))
                );
            }
        }
    }
}
