//! Typed client for `pkgd` (`os.lazy.pkgd.v1`), the application package
//! manager (`docs/packages.md`, phase 5).
//!
//! `pkgd` owns `/apps`; this app only asks it to inspect, install, remove
//! and list. Bodies are built and parsed with the generated
//! `messenger-generated` stubs, never by hand. Every reply is untrusted: a
//! decode failure or a malformed field becomes a friendly [`String`] error, so
//! a hostile package or a buggy service cannot make the app panic.
//!
//! The registered service name is `os.lazy.pkgd` (the interface name differs
//! from the registered name, as with `mimed`). Failures arrive as the shared
//! structured error field (id 15), which [`Service::call`] turns into a
//! negative errno.

use messenger_generated::os_lazy_pkgd_v1 as wire;

use crate::installer::{Installed, MimeHandler, Package, Permission};

use super::messenger::Service;

/// The registered service name (not the `os.lazy.pkgd.v1` interface id).
const NAME: &str = "os.lazy.pkgd";
/// The structured-error field id every service uses (outside the generated
/// range; see `user/src/messenger/services/mod.rs`).
const ERROR_FIELD: u16 = 15;

/// Resolves `pkgd` and performs one call, turning a missing service, a refused
/// request and a dead peer into the same friendly error shape. A refusal shows
/// `pkgd`'s own words (why the install was declined) when it sent any.
fn call(method: u32, body: Vec<u8>) -> Result<libmessenger::Parcel, String> {
    let service = Service::connect(NAME)
        .map_err(|code| format!("The package service (pkgd) is unavailable (errno {code})."))?;
    service
        .call_detailed(wire::INTERFACE_ID, method, ERROR_FIELD, body)
        .map_err(|error| {
            if error.message.is_empty() {
                format!(
                    "The package service refused the request (errno {}).",
                    error.code
                )
            } else {
                error.message
            }
        })
}

/// Open and validate the `.lzp` at `path` without changing anything.
pub fn inspect(path: &str) -> Result<Package, String> {
    let body = wire::encode_inspect_args(&wire::InspectArgs {
        path: path.to_owned(),
    })
    .map_err(|_| "The package path is too long for pkgd.".to_owned())?;
    let reply = call(wire::METHOD_INSPECT, body)?;
    let decoded = wire::decode_inspect_reply(&reply.body)
        .map_err(|_| "pkgd sent a reply this app cannot read.".to_owned())?;
    Ok(package_from_wire(decoded.info))
}

/// Install the package at `path`, returning the recorded app on success.
pub fn install(path: &str) -> Result<Installed, String> {
    let body = wire::encode_install_args(&wire::InstallArgs {
        path: path.to_owned(),
    })
    .map_err(|_| "The package path is too long for pkgd.".to_owned())?;
    let reply = call(wire::METHOD_INSTALL, body)?;
    let decoded = wire::decode_install_reply(&reply.body)
        .map_err(|_| "pkgd sent a reply this app cannot read.".to_owned())?;
    Ok(installed_from_wire(decoded.app))
}

/// Remove `system_name`. `pkgd` keeps the user's documents under `/home`.
pub fn remove(system_name: &str) -> Result<(), String> {
    let body = wire::encode_remove_args(&wire::RemoveArgs {
        system_name: system_name.to_owned(),
    })
    .map_err(|_| "The application id is too long for pkgd.".to_owned())?;
    call(wire::METHOD_REMOVE, body).map(|_| ())
}

/// Every installed app, in install order.
pub fn list() -> Result<Vec<Installed>, String> {
    let reply = call(wire::METHOD_LIST, Vec::new())?;
    let decoded = wire::decode_list_reply(&reply.body)
        .map_err(|_| "pkgd sent a reply this app cannot read.".to_owned())?;
    Ok(decoded.apps.into_iter().map(installed_from_wire).collect())
}

/// One installed app by `system_name`, if present.
pub fn installed(system_name: &str) -> Result<Option<Installed>, String> {
    let body = wire::encode_installed_args(&wire::InstalledArgs {
        system_name: system_name.to_owned(),
    })
    .map_err(|_| "The application id is too long for pkgd.".to_owned())?;
    let reply = call(wire::METHOD_INSTALLED, body)?;
    let decoded = wire::decode_installed_reply(&reply.body)
        .map_err(|_| "pkgd sent a reply this app cannot read.".to_owned())?;
    Ok(decoded.app.map(installed_from_wire))
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

#[cfg(test)]
mod tests {
    use super::*;

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
