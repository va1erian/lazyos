//! `Remove`: stop the app, withdraw its file types and policy, delete its
//! files and its row. A core app (one the image ships in `/system/packages`)
//! is refused for everyone, root included: the core set changes with image
//! updates, not with `pkgctl remove`.

use alloc::format;
use alloc::string::String;

use pkgstore::access::Caller;
use pkgstore::{layout, provision, tree};
use user::messenger::pkgd::{Failure, Installed};
use user::sys;

use super::handlers::{fail, registry_down, Pkgd, EAGAIN, EINVAL, EIO, ENOENT, EPERM, PROVISIONING};
use super::install::{event, one_line, Subject};
use super::policy;
use super::reconcile::stored_manifest;
use super::store::{describe, SysFs};

impl Pkgd {
    /// `Remove(system_name)`.
    pub(crate) fn remove(&mut self, caller: &Caller, system_name: &str) -> Result<(), Failure> {
        let uid = u64::from(caller.uid);
        let mut subject = Subject::none();
        subject.system_name = String::from(system_name);
        if let Err(why) = pkgstore::access::may_manage(caller) {
            return Err(self.refuse(&subject, uid, "REMOVE", fail(EPERM, why)));
        }
        if !self.provisioned.done {
            return Err(fail(EAGAIN, PROVISIONING));
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
        if let Err(why) = provision::removal(&row.name, self.is_core(system_name)) {
            return Err(self.refuse(&subject, uid, "REMOVE", fail(EPERM, why)));
        }
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
        tree::remove_tree(&mut SysFs, &path)
            .and_then(|()| tree::withdraw_docs(&mut SysFs, &row.system_name))
            .map_err(|error| {
                fail(
                    EIO,
                    format!(
                        "Removing failed while deleting its files: {}",
                        describe(&error)
                    ),
                )
            })?;
        if let Ok(app_dir) = layout::app_dir(&row.system_name) {
            tree::remove_if_empty(&mut SysFs, &app_dir);
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
}
