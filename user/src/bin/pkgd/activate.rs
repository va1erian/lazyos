//! The "switch" half of an install and what undoes or retires it: registering
//! an app's file types and loading its policy (the app's label and each of
//! its child labels, issue #832), shared by `Install` and boot
//! reconciliation, plus the roll-back and the clean-up after an upgrade.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use user::trace;

use lazypkg::Manifest;
use pkgstore::{layout, rules, tree};
use user::messenger::pkgd::Installed;

use super::handlers::Pkgd;
use super::install::{one_line, Installer};
use super::policy;
use super::reconcile::stored_manifest;
use super::store::{describe, SysFs};

impl Pkgd {
    /// Register the app's file types with `mimed` and load its policy: the
    /// "switch" half of an install, shared with boot reconciliation. `Err` is
    /// the step that failed, as text; nothing registered stays registered.
    ///
    /// `owner` is the uid whose app this is, `0` for everyone's (`mimed`
    /// shows a user's registration to that user alone).
    pub(crate) fn activate(&mut self, manifest: &Manifest, owner: u32) -> Result<(), String> {
        let app = manifest.app.system_name.as_str();
        let mut registered: Vec<(&str, &str)> = Vec::new();
        for handler in &manifest.mime {
            for verb in &handler.verbs {
                if let Err(error) =
                    self.registry
                        .mime_register(&handler.mime_type, app, verb, owner)
                {
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
                    "POLICY:PASS {} rules={}",
                    pkgstore::rules::label_of(manifest),
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

    /// Put the world back as it was before a failed install of `new`: the
    /// previous row and its registrations (an upgrade), or nothing (a first
    /// install). The label `new` would run under is withdrawn when it is not
    /// the one the previous version keeps.
    pub(crate) fn roll_back(&mut self, new: &Manifest, previous: Option<&Installed>) {
        match previous {
            Some(old) => {
                let _ = self.registry.put(old);
                let kept = rules::label_for_kind(&old.system_name, &old.kind);
                if kept != rules::label_of(new) {
                    let _ = policy::revoke(new);
                }
                if let Ok(manifest) = stored_manifest(&old.install_dir) {
                    let _ = self.activate(&manifest, Installer::of_row(old).owner());
                }
            }
            None => {
                let _ = self.registry.delete(&new.app.system_name);
                let _ = policy::revoke(new);
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

    /// After an upgrade took effect: end what still runs under the old
    /// version's label and withdraw it when the new one runs under another (an
    /// application that became a developer tool or the reverse), its file
    /// types and child labels the new one no longer has, then delete the old
    /// directory.
    ///
    /// A process keeps its label, so one left running would hold its old
    /// class (a tool's free names and unconfined native access, an app's
    /// registrations) beside a new version that runs under another: it is
    /// stopped first, as `Remove` does, and only then is the label revoked.
    pub(crate) fn retire(&mut self, old: &Installed, new: &Manifest) {
        if let Some(old_label) = rules::label_left_behind(&old.system_name, &old.kind, new) {
            match self.registry.stop(&old.system_name) {
                Ok(count) => trace::line!(
                    trace::Channel::Pkgd,
                    "UPGRADE:STOPPED {} {count}",
                    old.system_name
                ),
                Err(error) => trace::line!(
                    trace::Channel::Pkgd,
                    "UPGRADE:STOP:FAIL {}: {}",
                    old.system_name,
                    error.message()
                ),
            }
            if policy::revoke_label(&old_label).is_ok() {
                trace::line!(trace::Channel::Pkgd, "POLICY:REVOKED {old_label}");
            }
        }
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
