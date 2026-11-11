//! Typed client for `pkgd` (`os.lazy.pkgd.v1`), the application package
//! manager (`docs/packages.md`, phase 5).
//!
//! `pkgd` owns `/apps`; this app only asks it to inspect, install, remove
//! and list through the generated client. Every reply is untrusted: a decode
//! failure or a malformed field becomes a friendly [`String`] error, so a
//! hostile package or a buggy service cannot make the app panic.
//!
//! Every call waits what `idl/pkgd.midl` states: `pkgd` takes a reason to be
//! slow (`Install` unpacks and hashes the package, everything else reads real
//! files behind it), and a stalled package service must not hold the UI thread
//! hostage. The screens explain a timeout as "pkgd did not answer". The
//! install's bound is long (a slow machine, a big package; a timed-out install
//! still completes on disk, and a retry finds the app installed rather than
//! racing a second unpack), and the Installer's own install left the bounded
//! shape for the async lane ([`install_async`]).

use std::rc::Rc;

use libmessenger::client::Errno;
use messenger_generated::os_lazy_pkgd_v1 as wire;

use crate::installer::{Installed, MimeHandler, Package, Permission};

use super::messenger::{call_async, client, CallError, Deadline, Observed};

type Pkgd = wire::Client<Observed>;

/// The `pkgd` client, waiting for the service to register.
fn pkgd() -> Result<Rc<Pkgd>, String> {
    client(wire::Client::connect).map_err(|error| {
        format!(
            "The package service (pkgd) is unavailable (errno {}).",
            error.code()
        )
    })
}

/// One bounded call, turning a missing service, a refused request and a dead
/// peer into the same friendly error shape. A refusal shows `pkgd`'s own words
/// (why the install was declined) when it sent any.
fn call<R>(run: impl FnOnce(&Pkgd) -> Result<R, CallError>) -> Result<R, String> {
    let pkgd = pkgd()?;
    run(&pkgd).map_err(|error| refusal(&error))
}

/// The friendly text of a failed call.
fn refusal(error: &CallError) -> String {
    if let Some(words) = words(error) {
        return words.to_owned();
    }
    match error {
        CallError::Transport(errno) if *errno == Errno::BADMSG => {
            "pkgd sent a reply this app cannot read.".to_owned()
        }
        _ => format!(
            "The package service refused the request (errno {}).",
            error.code()
        ),
    }
}

/// Whether `pkgd` refused to replace a core app (docs/accounts-plan.md U2).
fn refused_core_app(error: &CallError) -> bool {
    -error.code() == crate::sys::errno::EPERM && error.message().contains("core app")
}

/// What `pkgd` said when it refused (its own words), if it said any.
fn words(error: &CallError) -> Option<&str> {
    match error {
        CallError::Remote { message, .. } if !message.is_empty() => Some(message),
        _ => None,
    }
}

/// Open and validate the `.lzp` at `path` without changing anything.
pub fn inspect(path: &str) -> Result<Package, String> {
    call(|pkgd| pkgd.inspect(path)).map(package_from_wire)
}

/// Install the package at `path` **on the installer's async call lane**
/// (issue #725's UI integration): the window keeps painting, resizing and
/// reacting while `pkgd` unpacks, and [`on_done`] -- on the UI thread, at a
/// later tick -- carries what [`install`] would have returned: the install's
/// own errors (pkgd's words winning), a core-app install re-asked through
/// `elevd` on the spot ("Not installed: ..."), and the transport failures.
///
/// The kernel-side wait is unbounded **by decision** (the bounded form's
/// "pkgd did not answer" after 2 minutes was exactly the failure this lane
/// replaces); the Installer's busy screen stands in for it, and a retry of
/// a wedged install finds the app already recorded (`pkgd.Installed`)
/// rather than racing a second unpack. The generated client has no
/// asynchronous form yet, so this is the one call that frames its request
/// through [`call_async`].
pub fn install_async(path: &str, on_done: impl FnOnce(Result<Installed, String>) + 'static) {
    let body = match wire::encode_install_args(&wire::InstallArgs {
        path: path.to_owned(),
    }) {
        Ok(body) => body,
        Err(_) => {
            // Not sent: the lane never sees it, so the answer goes to the
            // callback to keep the app's one result path whole.
            on_done(Err("The package path is too long for pkgd.".to_owned()));
            return;
        }
    };
    let pkgd = match pkgd() {
        Ok(pkgd) => pkgd,
        Err(message) => {
            on_done(Err(message));
            return;
        }
    };
    // The answer goes to the callback in every arm, so the app keeps one
    // result path; shared through `Rc` because both the reply arm (owned by
    // the lane) and a refusal on this side may fire it, at most once
    // between them (`Answer::fire` keeps count). `path` rides with them
    // (the core-app fallback re-asks by it).
    let path = path.to_owned();
    let answer = std::rc::Rc::new(Answer {
        fired: std::cell::Cell::new(false),
        callback: std::cell::Cell::new(Some(Box::new(on_done))),
    });
    let reply_answer = std::rc::Rc::clone(&answer);
    call_async(
        &pkgd,
        Pkgd::connection,
        wire::INTERFACE_ID,
        wire::METHOD_INSTALL,
        body,
        // Unbounded: `pkgd` unpacks at its own pace, and the bounded form's
        // "pkgd did not answer" is the failure this lane replaces; the UI
        // loop is never parked, so no window waits behind the unpack.
        Deadline::Never,
        Box::new(move |result| {
            let result = match result {
                // The install keeps its core-app rule (issue #529 /
                // U2): `pkgd` refuses the replace, and the lane's
                // refusal text says so, so the fallback runs here, on
                // the UI thread, as the bounded form it always was.
                Err(error) if refused_core_app(&error) => install_as_admin(&path),
                Err(error) => Err(words(&error).map(str::to_owned).unwrap_or_else(|| {
                    format!(
                        "The package service did not answer (errno {}).",
                        error.code()
                    )
                })),
                Ok(reply) => match wire::decode_install_reply(&reply.body) {
                    Ok(decoded) => Ok(installed_from_wire(decoded.app)),
                    Err(_) => Err("pkgd sent a reply this app cannot read.".to_owned()),
                },
            };
            reply_answer.fire(result);
        }),
    )
    // Refused before it registered (a dead peer): the same answer
    // through the callback, so the app keeps its one result path.
    .map_err(|errno| {
        answer.fire(Err(format!(
            "The package service (pkgd) is unavailable (errno {}).",
            errno.to_syscall()
        )))
    })
    .ok();
}

/// Install the package at `path`, returning the recorded app on success.
///
/// Replacing a core app is a system change (docs/accounts-plan.md U2):
/// `pkgd` refuses it from a session, so the install is asked again through
/// `elevd` (`pkg.update-core`), where an administrator approves on the
/// trusted prompt and `elevd` installs it.
pub fn install(path: &str) -> Result<Installed, String> {
    match pkgd()?.install(path) {
        Ok(app) => Ok(installed_from_wire(app)),
        Err(error) if refused_core_app(&error) => install_as_admin(path),
        Err(error) => Err(refusal(&error)),
    }
}

/// A core app's replacement, through `elevd` once an administrator approved.
fn install_as_admin(path: &str) -> Result<Installed, String> {
    println!("INSTALLER:ELEVATE pkg.update-core");
    super::elevd::request("pkg.update-core", &[path])
        .map_err(|error| format!("Not installed: {}", super::elevd::describe(&error)))?;
    let package = inspect(path)?;
    installed(&package.system_name)?
        .ok_or_else(|| String::from("The package was installed, but pkgd does not list it."))
}

/// `Develop(path, confirm)` (issue #529): approve a development run of the
/// package at `path`. Returns its `dev:` label and whether it is approved
/// (`false` only without `confirm`: the user must be asked first).
pub fn develop(path: &str, confirm: bool) -> Result<(String, bool), String> {
    call(|pkgd| pkgd.develop(path, confirm)).map(|reply| (reply.label, reply.approved))
}

/// `DevelopDeclined(path)`: the user refused the development consent, so
/// `pkgd` publishes the refusal the waiting IDE stops on.
pub fn develop_declined(path: &str) -> Result<(), String> {
    call(|pkgd| pkgd.develop_declined(path))
}

/// Remove `system_name`. `pkgd` keeps the user's documents under `/home`.
pub fn remove(system_name: &str) -> Result<(), String> {
    call(|pkgd| pkgd.remove(system_name))
}

/// Every installed app, in install order.
pub fn list() -> Result<Vec<Installed>, String> {
    let apps = call(|pkgd| pkgd.list())?;
    Ok(apps.into_iter().map(installed_from_wire).collect())
}

/// One installed app by `system_name`, if present.
pub fn installed(system_name: &str) -> Result<Option<Installed>, String> {
    let app = call(|pkgd| pkgd.installed(system_name))?;
    Ok(app.map(installed_from_wire))
}

/// The wire `PackageInfo` as the view-model's [`Package`].
fn package_from_wire(info: wire::PackageInfo) -> Package {
    Package {
        name: info.name,
        system_name: info.system_name,
        author: info.author,
        version: info.version,
        description: info.description,
        digest: info.digest,
        install_dir: info.install_dir,
        mime: info.mime.into_iter().map(mime_from_wire).collect(),
        permissions: info
            .permissions
            .into_iter()
            .map(permission_from_wire)
            .collect(),
        problems: info.problems,
        category: info.category,
        autostart: info.autostart,
    }
}

/// The wire `MimeHandler` as the view-model's [`MimeHandler`].
fn mime_from_wire(handler: wire::MimeHandler) -> MimeHandler {
    MimeHandler {
        mime_type: handler.mime_type,
        verbs: handler.verbs,
        has_icon: handler.has_icon,
    }
}

/// The wire `Permission` as the view-model's [`Permission`].
fn permission_from_wire(permission: wire::Permission) -> Permission {
    Permission {
        kind: permission.kind,
        value: permission.value,
        risk: permission.risk,
        explanation: permission.explanation,
    }
}

/// The wire `Installed` as the view-model's [`Installed`].
fn installed_from_wire(app: wire::Installed) -> Installed {
    Installed {
        system_name: app.system_name,
        name: app.name,
        version: app.version,
        install_dir: app.install_dir,
        digest: app.digest,
        binary: app.binary,
        installed_at: app.installed_at,
        core: app.origin == wire::ORIGIN_CORE,
    }
}

/// The install's answer, fired at most once ([`install_async`]'s reply arm and a refusal on this side may
/// both reach it).
struct Answer {
    fired: std::cell::Cell<bool>,
    callback: std::cell::Cell<Option<Box<dyn FnOnce(Result<Installed, String>)>>>,
}

impl Answer {
    fn fire(&self, result: Result<Installed, String>) {
        if self.fired.replace(true) {
            return;
        }
        if let Some(callback) = self.callback.take() {
            callback(result);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A lane answer that shows up twice (the entry's own terminal refire
    /// the kernel dedups, plus the caller's arm) runs the app's callback
    /// exactly once.
    #[test]
    fn the_answer_fires_exactly_once() {
        let fired = std::rc::Rc::new(std::cell::Cell::new(0));
        let answer = std::rc::Rc::new(Answer {
            fired: std::cell::Cell::new(false),
            callback: std::cell::Cell::new(Some(Box::new({
                let fired = std::rc::Rc::clone(&fired);
                move |result| {
                    fired.set(match result {
                        Ok(_) => 1,
                        Err(_) => 2,
                    });
                }
            }))),
        });
        answer.fire(Ok(Installed::default()));
        answer.fire(Err(String::from("late")));
        assert_eq!(fired.get(), 1, "the callback ran twice");
    }

    #[test]
    fn a_wire_package_converts_every_field() {
        let info = wire::PackageInfo {
            name: "Paint".into(),
            system_name: "org.lazy.paint".into(),
            author: "Valerian".into(),
            version: "1.2.0".into(),
            description: "A tiny raster painter".into(),
            digest: "abc123".into(),
            install_dir: "org.lazy.paint/1.2.0-abc12345".into(),
            mime: vec![wire::MimeHandler {
                mime_type: "image/png".into(),
                verbs: vec!["open".into(), "edit".into()],
                has_icon: true,
            }],
            permissions: vec![wire::Permission {
                kind: "file".into(),
                value: "read:$HOME/*".into(),
                risk: "high".into(),
                explanation: "read your documents".into(),
            }],
            problems: vec!["bad version".into()],
            category: "graphics".into(),
            autostart: true,
        };
        let package = package_from_wire(info);
        assert_eq!(package.name, "Paint");
        assert_eq!(package.system_name, "org.lazy.paint");
        assert_eq!(package.mime.len(), 1);
        assert_eq!(package.mime[0].verbs, vec!["open", "edit"]);
        assert!(package.mime[0].has_icon);
        assert_eq!(package.permissions[0].risk, "high");
        assert_eq!(package.problems, vec!["bad version"]);
        assert_eq!(package.category, "graphics");
        assert!(package.autostart);
    }

    #[test]
    fn an_empty_reply_decodes_to_a_default_package_not_a_panic() {
        // A `pkgd` reply missing every field is a valid (all-default) message;
        // the app shows blanks rather than crashing.
        let package = package_from_wire(wire::PackageInfo::default());
        assert_eq!(package, Package::default());
    }

    #[test]
    fn garbage_bytes_are_a_decode_error() {
        assert!(wire::decode_inspect_reply(&[0xFF, 0xFF]).is_err());
    }

    #[test]
    fn an_installed_app_converts_every_field() {
        let app = installed_from_wire(wire::Installed {
            system_name: "org.lazy.paint".into(),
            name: "Paint".into(),
            version: "1.2.0".into(),
            install_dir: "org.lazy.paint/1.2.0-abc12345".into(),
            digest: "abc123".into(),
            binary: "bin/paint.elf".into(),
            installed_at: 42,
            ..wire::Installed::default()
        });
        assert_eq!(app.system_name, "org.lazy.paint");
        assert_eq!(app.binary, "bin/paint.elf");
        assert_eq!(app.installed_at, 42);
    }
}

#[cfg(test)]
mod origin_tests {
    use super::*;

    #[test]
    fn the_core_origin_marks_a_built_in_app() {
        let core = installed_from_wire(wire::Installed {
            system_name: "os.lazy.paint".into(),
            origin: wire::ORIGIN_CORE,
            ..wire::Installed::default()
        });
        assert!(core.core);
        let user = installed_from_wire(wire::Installed {
            origin: wire::ORIGIN_USER,
            ..wire::Installed::default()
        });
        assert!(!user.core);
    }
}
