//! Package layout.
//!
//! Beyond per-name safety, the archive has to look like a LazyOS package: only
//! a known set of top-level paths, extensions per directory, and the three
//! `app-*.png` icons. This is the structural half of the contract the Python
//! builder in `tools/pkg/build.py` also enforces; both sides have tests for it.

use alloc::collections::BTreeSet;

use crate::error::OpenError;
use crate::zip::ZipEntry;

/// The manifest, the one top-level file.
pub(crate) const MANIFEST: &str = "manifest.toml";

/// The icons every package must carry, in the sizes the shell uses.
pub(crate) const REQUIRED_ICONS: [&str; 3] =
    ["icons/app-16.png", "icons/app-32.png", "icons/app-128.png"];

/// Validate the archive layout, returning the set of file (non-directory)
/// names for the manifest's existence checks.
pub(crate) fn validate<'a>(entries: &[ZipEntry<'a>]) -> Result<BTreeSet<&'a str>, OpenError> {
    let mut files: BTreeSet<&str> = BTreeSet::new();
    let mut has_manifest = false;
    for entry in entries {
        let name = entry.info.name;
        if entry.info.is_dir {
            check_directory(name)?;
            continue;
        }
        files.insert(name);
        if name == MANIFEST {
            has_manifest = true;
            continue;
        }
        let top = name.split('/').next().unwrap_or("");
        let extension_ok = match top {
            "bin" => name.ends_with(".elf"),
            "icons" => name.ends_with(".png"),
            "idl" => name.ends_with(".midl"),
            "docs" => name.ends_with(".md"),
            "resources" => true,
            _ => {
                return Err(OpenError::Layout {
                    name: name.into(),
                    reason: "is not under an allowed top-level directory",
                })
            }
        };
        if !extension_ok {
            return Err(OpenError::Layout {
                name: name.into(),
                reason: "has the wrong file extension",
            });
        }
    }
    if !has_manifest {
        return Err(OpenError::NoManifest);
    }
    for icon in REQUIRED_ICONS {
        if !files.contains(icon) {
            return Err(OpenError::Layout {
                name: icon.into(),
                reason: "is missing",
            });
        }
    }
    Ok(files)
}

/// Directories may only sit under the allowed top-level trees.
fn check_directory(name: &str) -> Result<(), OpenError> {
    let top = name.split('/').next().unwrap_or("");
    if matches!(top, "bin" | "icons" | "idl" | "docs" | "resources") {
        Ok(())
    } else {
        Err(OpenError::Layout {
            name: name.into(),
            reason: "is not under an allowed top-level directory",
        })
    }
}
