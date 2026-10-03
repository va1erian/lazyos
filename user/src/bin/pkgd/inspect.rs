//! Turning an archive into what the installer shows: the `PackageInfo` of
//! `Inspect`, with every reason the package cannot be installed in `problems`.
//!
//! A package that fails validation is not an error reply: it is a `PackageInfo`
//! whose `problems` list says why, so a GUI can show all of them at once. The
//! same function feeds `Install`, which refuses on a non-empty list, so what was
//! shown and what is enforced cannot differ.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use lazyos_crypto::hex;
use lazypkg::{OpenError, Package};
use pkgstore::{explain, layout, rules};
use user::messenger::pkgd::wire::{MimeHandler, PackageInfo};

/// Most permission entries one package may request. The consent screen lists
/// every one (a truncated list would hide what was approved), and the reply
/// must fit one Messenger buffer.
pub(crate) const MAX_PERMISSIONS: usize = 24;
/// Most problems reported; the rest are summarised in one line.
const MAX_PROBLEMS: usize = 12;
/// Longest problem line kept, in characters.
const MAX_PROBLEM_CHARS: usize = 200;

/// A package that opened, with the info built from it.
pub(crate) struct Assessed<'a> {
    pub(crate) package: Package<'a>,
    pub(crate) info: PackageInfo,
}

/// Open and assess `bytes`. `Err` is the info of an archive that did not even
/// open (only `problems` is filled); `Ok` may still carry problems.
pub(crate) fn assess(bytes: &[u8]) -> Result<Assessed<'_>, Box<PackageInfo>> {
    let package = match Package::open(bytes) {
        Ok(package) => package,
        Err(error) => {
            return Err(Box::new(PackageInfo {
                problems: open_problems(&error),
                ..PackageInfo::default()
            }))
        }
    };
    let manifest = package.manifest();
    let mut problems = Vec::new();
    let permissions = explain::permissions(manifest);
    if permissions.len() > MAX_PERMISSIONS {
        problems.push(format!(
            "the package requests {} permissions; at most {MAX_PERMISSIONS} are allowed",
            permissions.len()
        ));
    }
    if let Err(error) = rules::compile(manifest) {
        problems.push(format!("{error}"));
    }
    let install_dir = package.install_dir();
    if !layout::valid_install_dir(&install_dir) {
        problems.push(String::from(
            "the package's install directory name is malformed",
        ));
    }
    let info = PackageInfo {
        name: manifest.app.name.clone(),
        system_name: manifest.app.system_name.clone(),
        author: manifest.app.author.clone(),
        version: manifest.app.version.clone(),
        description: manifest.app.description.clone().unwrap_or_default(),
        digest: hex::encode(&package.digest()),
        install_dir,
        mime: manifest
            .mime
            .iter()
            .map(|handler| MimeHandler {
                mime_type: handler.mime_type.clone(),
                verbs: handler.verbs.clone(),
                has_icon: handler.icon.is_some(),
            })
            .collect(),
        permissions,
        problems: cap(problems),
        category: String::from(manifest.app.category().as_str()),
        autostart: manifest.entry.autostart,
    };
    Ok(Assessed { package, info })
}

/// Every reason `Package::open` refused an archive, one string each.
fn open_problems(error: &OpenError) -> Vec<String> {
    match error {
        OpenError::Manifest(manifest) => cap(manifest
            .problems()
            .iter()
            .map(|problem| format!("manifest: {}", problem.message()))
            .collect()),
        other => alloc::vec![format!("{other}")],
    }
}

/// At most [`MAX_PROBLEMS`] lines, the overflow folded into one.
fn cap(problems: Vec<String>) -> Vec<String> {
    let mut problems: Vec<String> = problems
        .into_iter()
        .map(|problem| problem.chars().take(MAX_PROBLEM_CHARS).collect())
        .collect();
    if problems.len() > MAX_PROBLEMS {
        let more = problems.len() - MAX_PROBLEMS;
        problems.truncate(MAX_PROBLEMS);
        problems.push(format!("and {more} more problems"));
    }
    problems
}
