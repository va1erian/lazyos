//! The install handoff: LazyRAD's [`Installer`] on LazyOS (plan P4, and
//! `docs/lazyrad-package-plan.md` section 2.2).
//!
//! The IDE is a core package, so the kernel labels it, and `pkgd` refuses every
//! labelled caller (`pkgstore::access`): it can neither `Inspect` nor `Install`.
//! It never calls `pkgd`. File → Make LazyOS App does what the Terminal does
//! for a downloaded `.lzp`:
//!
//! 1. **Pre-check, in process.** `pkgstore::inspect::assess`, the function
//!    `pkgd`'s `Inspect` runs, opens the built archive with `lazypkg` and lists
//!    every permission and problem for the IDE's own consent step
//!    ([`Installer::review`]). Nothing is sent anywhere.
//! 2. **Stage** the archive as `/transient/lazyrad-<sn>-<version>.lzp`.
//! 3. **`mimed.Open(path, "install")`.** `mimed` asks `init` to start the
//!    Package Installer, which runs unlabelled in the caller's session and
//!    shows the trusted consent screen (permissions explained by `pkgd`'s own
//!    table), then calls `pkgd.Install`.
//! 4. **Observe the outcome** on `system/events/pkg/+`: the `install` or
//!    `denied` event whose `system_name` and `digest` are this package's. The
//!    subscription is made before step 3 so the event cannot be missed.
//! 5. **Start the app** with `init.Launch(system_name)`; `init` stamps an
//!    installed app with its own label, not the IDE's.
//!
//! Every body is built and read with the `midlc`-generated stubs
//! ([`client`]). The wait for the outcome blocks the IDE's UI thread (the
//! window does not repaint until it ends): the portable IDE calls [`Installer`]
//! synchronously and LazyOS threads cannot share descriptors. It ends when the
//! user finishes in the Installer, or after [`OUTCOME_TICKS`]; cancelling in
//! the Installer publishes no event, so a cancel is reported when the wait runs
//! out.
//!
//! The IDE's package needs `os.lazy.mimed.v1`, `os.lazy.init.v1` and
//! `subscribe:system/events/pkg/+` for this (`xui-app/packages/lazyrad`).

mod client;
#[cfg(test)]
mod tests;

use std::fs;
use std::path::PathBuf;

use lazyrad_packager::lzp::{
    BuiltPackage, InstallError, InstallState, InstalledApp, Installer, PackageReview,
    PermissionNote,
};
use messenger_generated::os_lazy_pkgd_v1 as pkgd;
use pkgstore::inspect::assess;

pub use client::{Client, BROKER_NAME, INIT_NAME, MIMED_NAME};

use crate::transport::{Failure, MessengerTransport, Transport};

/// The Installer's `mimed` verb (`xui-app`'s Installer registers it).
pub const INSTALL_VERB: &str = "install";
/// The Installer reads at most this much (`pkgd`'s package limit).
pub const MAX_PACKAGE_BYTES: usize = 8 * 1024 * 1024;
/// Where packages are staged: readable by the Installer, which runs as root.
pub const STAGING_DIR: &str = fhs::mount::TRANSIENT;
/// How long to wait for the Installer's outcome: 5 minutes at 100 Hz.
pub const OUTCOME_TICKS: u64 = 5 * 60 * 100;
/// The pause between two polls of the event queue.
const POLL_MILLIS: u64 = 25;

/// The friendly sentence for a failure.
pub fn friendly(failure: &Failure) -> String {
    let text = failure.text.as_str();
    if failure.code < 0 {
        return format!("A system service is not available: {text}");
    }
    if text.contains("already installed") {
        return "This version of the app is already installed.".to_owned();
    }
    text.to_owned()
}

/// LazyRAD's [`Installer`] over the Package Installer.
pub struct HandoffInstaller<T: Transport> {
    client: Client<T>,
    staging: PathBuf,
    outcome_ticks: u64,
}

impl HandoffInstaller<MessengerTransport> {
    /// The installer on the real services, staging in [`STAGING_DIR`].
    pub fn on_lazyos() -> Self {
        HandoffInstaller::new(
            MessengerTransport::default(),
            PathBuf::from(STAGING_DIR),
            OUTCOME_TICKS,
        )
    }
}

impl<T: Transport> HandoffInstaller<T> {
    /// An installer over `transport` that stages packages in `staging` and
    /// waits `outcome_ticks` for the Installer's answer.
    pub fn new(transport: T, staging: PathBuf, outcome_ticks: u64) -> Self {
        HandoffInstaller {
            client: Client::new(transport),
            staging,
            outcome_ticks,
        }
    }

    /// The pre-check of `package`: what the consent step shows.
    fn assess(package: &BuiltPackage) -> pkgd::PackageInfo {
        let mut info = match assess(&package.bytes) {
            Ok(assessed) => assessed.info,
            Err(info) => *info,
        };
        if package.bytes.len() > MAX_PACKAGE_BYTES {
            info.problems.push(format!(
                "The package is {} bytes; the installer reads at most {MAX_PACKAGE_BYTES}.",
                package.bytes.len()
            ));
        }
        info
    }

    /// Writes the package where the Installer can read it; the file is removed
    /// when the returned guard drops.
    fn stage(&self, package: &BuiltPackage) -> Result<Staged, InstallError> {
        let path = self.staging.join(format!(
            "lazyrad-{}-{}.lzp",
            package.system_name, package.version
        ));
        fs::write(&path, &package.bytes).map_err(|error| {
            InstallError::Refused(vec![format!(
                "The package could not be written to {}: {error}",
                path.display()
            )])
        })?;
        Ok(Staged(path))
    }

    /// Polls for the `install`/`denied` event of this package.
    fn await_outcome(
        &self,
        subscription: u64,
        info: &pkgd::PackageInfo,
    ) -> Result<pkgd::PkgEvent, InstallError> {
        let deadline = self
            .client
            .transport
            .ticks()
            .saturating_add(self.outcome_ticks);
        loop {
            match self
                .client
                .next_pkg_event(subscription)
                .map_err(|f| refused(&f))?
            {
                Some(event) if is_outcome(&event, info) => return Ok(event),
                // Another package's record: look at the next one at once.
                Some(_) => {}
                None if self.client.transport.ticks() >= deadline => {
                    return Err(not_answered(info))
                }
                None => self.client.transport.pause(POLL_MILLIS),
            }
        }
    }
}

/// Whether `event` is the answer to installing `info`'s package.
fn is_outcome(event: &pkgd::PkgEvent, info: &pkgd::PackageInfo) -> bool {
    matches!(event.op.as_str(), "install" | "denied")
        && event.system_name == info.system_name
        && event.digest == info.digest
}

fn refused(failure: &Failure) -> InstallError {
    InstallError::Refused(vec![friendly(failure)])
}

fn not_answered(info: &pkgd::PackageInfo) -> InstallError {
    InstallError::Refused(vec![format!(
        "{} was not installed: the Package Installer reported nothing. If you cancelled \
         there, that is expected.",
        info.name
    )])
}

/// A staged package file, deleted on drop.
struct Staged(PathBuf);

impl Staged {
    fn path(&self) -> &str {
        self.0.to_str().unwrap_or_default()
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// A broker subscription, dropped when the install is over.
struct Subscribed<'a, T: Transport> {
    client: &'a Client<T>,
    id: u64,
}

impl<T: Transport> Drop for Subscribed<'_, T> {
    fn drop(&mut self) {
        self.client.unsubscribe(self.id);
    }
}

impl<T: Transport> Installer for HandoffInstaller<T> {
    fn review(&self, package: &BuiltPackage) -> Result<Option<PackageReview>, InstallError> {
        let info = Self::assess(package);
        Ok(Some(PackageReview {
            name: info.name,
            system_name: info.system_name,
            author: info.author,
            version: info.version,
            permissions: info
                .permissions
                .into_iter()
                .map(|p| PermissionNote {
                    kind: p.kind,
                    value: p.value,
                    risk: p.risk,
                    explanation: p.explanation,
                })
                .collect(),
            problems: info.problems,
        }))
    }

    fn install(&self, package: &BuiltPackage) -> Result<InstalledApp, InstallError> {
        let info = Self::assess(package);
        if !info.problems.is_empty() {
            return Err(InstallError::Refused(info.problems));
        }
        let staged = self.stage(package)?;
        // Before the Installer starts, so its event cannot be missed.
        let id = self
            .client
            .subscribe_pkg_events()
            .map_err(|f| refused(&f))?;
        let _subscription = Subscribed {
            client: &self.client,
            id,
        };
        let opened = self
            .client
            .open(staged.path(), INSTALL_VERB)
            .map_err(|f| refused(&f))?;
        if !opened.launched {
            return Err(InstallError::Refused(vec![format!(
                "The Package Installer could not be started (handler: {}).",
                if opened.app.is_empty() {
                    "none"
                } else {
                    &opened.app
                }
            )]));
        }
        let event = self.await_outcome(id, &info)?;
        if event.op != "install" || !event.ok {
            return Err(InstallError::Refused(vec![friendly(&Failure {
                code: 1,
                text: event.detail,
            })]));
        }
        Ok(InstalledApp {
            system_name: event.system_name,
            version: event.version,
            state: InstallState::Installed,
            location: Some(PathBuf::from(fhs::state::APPS_ROOT).join(event.install_dir)),
        })
    }

    fn launch(&self, system_name: &str) -> Result<(), InstallError> {
        self.client.launch(system_name).map_err(|f| {
            InstallError::Refused(vec![format!(
                "The app could not be started: {}",
                friendly(&f)
            )])
        })
    }
}
