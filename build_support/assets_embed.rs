//! Binary data assets (issue #454): every file under the checked-in `assets/`
//! tree, and under each directory `LAZYOS_ASSETS` names, is copied to
//! [`fhs::SYSTEM_SHARE`]`/<same relative path>` on the OS volume, so a demo
//! that needs a module file, a sample, a picture or a texture ships it as data
//! the user can swap, not as `include_bytes!` in an ELF.
//!
//! **Manifest.** Each tree has a `manifest.txt` with one line per file:
//!
//! ```text
//! <path> | <licence> | <install> | <provenance>
//! samples/writer-sample.png | GPL-3.0-or-later | all | LazyOS original (#533)
//! ```
//!
//! `path` is relative to the tree, `/`-separated; `licence` is an SPDX id (or
//! `A OR B`) from [`LICENCES`], the freely licensed ones, so a file of unknown
//! or non-free terms cannot land; `install` is `all` (every image), `desktop`
//! (images with the desktop shell) or `none` (kept in the tree but compiled
//! into a program instead, like the fonts); `provenance` says where the file
//! came from. A file without an entry, an entry without a file, an unknown
//! licence or an oversized file fails the build with the reason: assets are
//! never dropped silently. `#` starts a comment line.
//!
//! **Sizes.** [`MAX_FILE_BYTES`] and [`MAX_TOTAL_BYTES`] ([`LIMITS`]) keep a stray video
//! from filling the OS volume (512 MiB by default, `LAZYOS_OS_SIZE`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::os_image::Sink;

/// The checked-in tree, relative to the repository root.
pub const CHECKED_IN: &str = "assets";
/// Each tree's manifest, at its root.
pub const MANIFEST: &str = "manifest.txt";
/// The largest one asset may be (Freedoom's IWAD is 28 MiB).
pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
/// The most all installed assets of one image may add up to.
pub const MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;

/// The size caps a build checks, in bytes (tests pass small ones).
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub file: u64,
    pub total: u64,
}

/// The caps of a real build.
pub const LIMITS: Limits = Limits {
    file: MAX_FILE_BYTES,
    total: MAX_TOTAL_BYTES,
};

/// The licences an asset may carry: free content and free software licences
/// (SPDX ids). `GPL-3.0-or-later` is the project's own, for original work.
pub const LICENCES: &[&str] = &[
    "CC0-1.0",
    "CC-BY-3.0",
    "CC-BY-4.0",
    "CC-BY-SA-3.0",
    "CC-BY-SA-4.0",
    "Apache-2.0",
    "OFL-1.1",
    "MIT",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "ISC",
    "Zlib",
    "Unlicense",
    "GPL-2.0-only",
    "GPL-2.0-or-later",
    "GPL-3.0-only",
    "GPL-3.0-or-later",
    "LGPL-2.1-or-later",
];

/// Which images install an asset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Install {
    /// Every image.
    All,
    /// Images with the desktop shell.
    Desktop,
    /// No image: the file is compiled into a program.
    None,
}

/// One manifest line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub licence: String,
    pub install: Install,
    pub source: String,
}

/// A file of a tree with its manifest entry.
#[derive(Clone, Debug)]
pub struct Asset {
    pub entry: Entry,
    pub file: PathBuf,
    pub len: u64,
}

/// Parse a manifest. Every line is checked; the first bad one is the error.
pub fn parse_manifest(text: &str) -> Result<Vec<Entry>, String> {
    let mut entries: Vec<Entry> = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let entry = parse_line(line).map_err(|why| format!("line {}: {why}", index + 1))?;
        if entries
            .iter()
            .any(|seen| seen.path.eq_ignore_ascii_case(&entry.path))
        {
            return Err(format!(
                "line {}: {} is listed twice",
                index + 1,
                entry.path
            ));
        }
        entries.push(entry);
    }
    Ok(entries)
}

fn parse_line(line: &str) -> Result<Entry, String> {
    let fields: Vec<&str> = line.split('|').map(str::trim).collect();
    let [path, licence, install, source] = fields[..] else {
        return Err(format!(
            "expected `path | licence | install | provenance`, got {} field(s)",
            fields.len()
        ));
    };
    check_path(path)?;
    check_licence(licence)?;
    let install = match install {
        "all" => Install::All,
        "desktop" => Install::Desktop,
        "none" => Install::None,
        other => return Err(format!("install `{other}` is not all, desktop or none")),
    };
    if source.is_empty() {
        return Err(format!("{path} has no provenance"));
    }
    Ok(Entry {
        path: path.to_string(),
        licence: licence.to_string(),
        install,
        source: source.to_string(),
    })
}

/// A relative `/`-separated path of plain names (no `.`, `..`, `\`, empty
/// component, or the manifest itself).
pub fn check_path(path: &str) -> Result<(), String> {
    let plain =
        |part: &str| !part.is_empty() && part != "." && part != ".." && !part.contains('\\');
    if path.starts_with('/') || !path.split('/').all(plain) {
        return Err(format!("`{path}` is not a relative path of plain names"));
    }
    if path == MANIFEST {
        return Err(format!("{MANIFEST} cannot list itself"));
    }
    Ok(())
}

/// Every alternative of `A OR B` must be a licence of [`LICENCES`].
pub fn check_licence(licence: &str) -> Result<(), String> {
    let free = |id: &str| LICENCES.contains(&id.trim());
    if licence.is_empty() || !licence.split(" OR ").all(free) {
        return Err(format!(
            "licence `{licence}` is not one of the free licences assets may carry ({})",
            LICENCES.join(", ")
        ));
    }
    Ok(())
}

/// Where an asset lands: `samples/a.png` -> `/system/share/samples/a.png`.
pub fn image_path(path: &str) -> String {
    format!("{}/{path}", fhs::SYSTEM_SHARE)
}

/// Collect the tree at `root` against its manifest, sorted by path. A missing
/// `root` is an empty tree; files without a manifest are an error.
pub fn collect(root: &Path, limits: Limits) -> Result<Vec<Asset>, String> {
    let mut files = BTreeMap::new();
    match std::fs::symlink_metadata(root) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(format!("cannot stat {}: {err}", root.display())),
        Ok(meta) if !meta.is_dir() => return Err(format!("{} is not a directory", root.display())),
        Ok(_) => walk(root, "", &mut files)?,
    }
    let manifest = root.join(MANIFEST);
    let entries = match std::fs::read_to_string(&manifest) {
        Ok(text) => {
            parse_manifest(&text).map_err(|why| format!("{}: {why}", manifest.display()))?
        }
        Err(_) if files.is_empty() => return Ok(Vec::new()),
        Err(err) => {
            return Err(format!(
                "{}: {err} (every asset tree needs one)",
                manifest.display()
            ))
        }
    };
    let mut assets = Vec::with_capacity(entries.len());
    for entry in entries {
        let Some((file, len)) = files.remove(&entry.path) else {
            return Err(format!(
                "{} lists {}, which does not exist",
                manifest.display(),
                entry.path
            ));
        };
        if len > limits.file {
            return Err(format!(
                "{} is {len} bytes, over the {}-byte per-file limit",
                file.display(),
                limits.file
            ));
        }
        assets.push(Asset { entry, file, len });
    }
    if let Some(path) = files.keys().next() {
        return Err(format!(
            "{}/{path} has no entry in {} (add `path | licence | install | provenance`)",
            root.display(),
            manifest.display()
        ));
    }
    assets.sort_by(|a, b| a.entry.path.cmp(&b.entry.path));
    Ok(assets)
}

/// Add every regular file under `dir` (as `prefix/name`) except the manifest.
fn walk(
    dir: &Path,
    prefix: &str,
    files: &mut BTreeMap<String, (PathBuf, u64)>,
) -> Result<(), String> {
    let entries =
        std::fs::read_dir(dir).map_err(|err| format!("cannot read {}: {err}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|err| format!("cannot list {}: {err}", dir.display()))?;
        let path = entry.path();
        let name = entry
            .file_name()
            .into_string()
            .map_err(|name| format!("{}: name {name:?} is not UTF-8", dir.display()))?;
        let relative = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let meta = std::fs::symlink_metadata(&path)
            .map_err(|err| format!("cannot stat {}: {err}", path.display()))?;
        if meta.is_dir() {
            walk(&path, &relative, files)?;
        } else if meta.is_file() {
            if relative != MANIFEST {
                files.insert(relative, (path, meta.len()));
            }
        } else {
            return Err(format!(
                "{} is not a regular file or directory",
                path.display()
            ));
        }
    }
    Ok(())
}

/// The assets an image installs, as `(destination, asset)`: `desktop` ones
/// only with the shell, none of `none`. Two trees may not install one
/// destination, and the total stays within `limits.total`.
pub fn select(
    trees: &[Vec<Asset>],
    shell: bool,
    limits: Limits,
) -> Result<Vec<(String, Asset)>, String> {
    let mut chosen: BTreeMap<String, Asset> = BTreeMap::new();
    let mut total = 0u64;
    for asset in trees.iter().flatten() {
        let wanted = match asset.entry.install {
            Install::All => true,
            Install::Desktop => shell,
            Install::None => false,
        };
        if !wanted {
            continue;
        }
        let dest = image_path(&asset.entry.path);
        if let Some(first) = chosen.get(&dest) {
            return Err(format!(
                "{} and {} both install {dest}",
                first.file.display(),
                asset.file.display()
            ));
        }
        total += asset.len;
        chosen.insert(dest, asset.clone());
    }
    if total > limits.total {
        return Err(format!(
            "the assets add up to {total} bytes, over the {}-byte limit",
            limits.total
        ));
    }
    Ok(chosen.into_iter().collect())
}

/// The trees of a build: the checked-in one, then each `LAZYOS_ASSETS` entry
/// (a path list, `;`-separated on Windows and `:` elsewhere).
pub fn roots(manifest_dir: &Path, extra: Option<&std::ffi::OsStr>) -> Vec<PathBuf> {
    let mut roots = vec![manifest_dir.join(CHECKED_IN)];
    if let Some(extra) = extra {
        roots.extend(std::env::split_paths(extra).filter(|path| !path.as_os_str().is_empty()));
    }
    roots
}

/// Add the assets of every tree to the OS file list (`shell`: the image has
/// the desktop shell). A bad tree fails the build with the reason.
pub fn embed(sink: &mut dyn Sink, manifest_dir: &Path, shell: bool) {
    println!("cargo:rerun-if-changed=build_support/assets_embed.rs");
    println!("cargo:rerun-if-env-changed=LAZYOS_ASSETS");
    let extra = std::env::var_os("LAZYOS_ASSETS");
    let mut trees = Vec::new();
    for root in roots(manifest_dir, extra.as_deref()) {
        // A directory is watched recursively: an added asset reruns the build.
        println!("cargo:rerun-if-changed={}", root.display());
        trees.push(collect(&root, LIMITS).unwrap_or_else(|why| panic!("assets: {why}")));
    }
    let chosen = select(&trees, shell, LIMITS).unwrap_or_else(|why| panic!("assets: {why}"));
    let extras: usize = trees.iter().skip(1).map(Vec::len).sum();
    if extras > 0 {
        println!("cargo:warning=assets: {extras} file(s) from LAZYOS_ASSETS");
    }
    for (dest, asset) in chosen {
        sink.add_file(&dest, asset.file);
    }
}
