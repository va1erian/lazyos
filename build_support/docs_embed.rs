//! Embed the documentation tree in the OS volume.
//!
//! Every `*.md` under `docs/` (recursively, so `docs/architecture/boot.md`
//! included) and the repository `README.md` are stored under `docs/<same
//! relative path>` on the OS volume, so the Docs app and the Editor can read
//! them through the VFS at `/docs/...` (the root `README.md` becomes
//! `/docs/README.md`).
//!
//! **Case.** The OS volume is ext2, which is case-sensitive: the Docs app and the
//! Editor open `/docs/README.md` by exactly that spelling (`fhs::docs`), so the
//! destination keeps the case the source file has, and `docs/` wins over the
//! root `README.md` under any case. Directories and long names are created as
//! needed by the image composer.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::os_image::Sink;

/// The directory the docs tree is copied to, under the OS volume root.
const DISK_ROOT: &str = "docs";

/// The repository README, embedded as `docs/README.md`.
const README: &str = "README.md";

/// Skip a file larger than this. The bootloader reads the whole image over slow
/// BIOS calls at every boot, so a runaway document must not bloat it.
const MAX_FILE_BYTES: u64 = 256 * 1024;

/// One document to place in the image: its `/`-separated destination under the
/// image root and its bytes.
pub struct Doc {
    pub dest: String,
    pub bytes: Vec<u8>,
}

/// Collect the embeddable docs, sorted by destination.
///
/// `manifest_dir` is the crate root (here the repository root). Every `*.md`
/// under `docs/` keeps its relative path; the root `README.md` maps to
/// `docs/README.md` (a real `docs/README.md` wins if both exist). Files whose
/// path or bytes are not valid UTF-8, and files larger than [`MAX_FILE_BYTES`],
/// are skipped with a `cargo:warning` so one bad file can never fail a build. A
/// missing `docs/` is not an error.
pub fn collect(manifest_dir: &Path) -> Result<Vec<Doc>, String> {
    // A BTreeMap sorts destinations and de-duplicates them deterministically.
    let mut found: BTreeMap<String, PathBuf> = BTreeMap::new();
    collect_dir(&manifest_dir.join(DISK_ROOT), manifest_dir, &mut found)?;

    let readme = manifest_dir.join(README);
    if readme.is_file() {
        // A real `docs/readme.MD` (any case) wins over the root README: two
        // spellings of one document must not both reach the volume, since
        // the readers open `/docs/README.md` by exactly that name.
        let dest = format!("{DISK_ROOT}/{README}");
        if !found.keys().any(|key| key.eq_ignore_ascii_case(&dest)) {
            found.insert(dest, readme);
        }
    }

    let mut docs = Vec::with_capacity(found.len());
    for (dest, source) in found {
        match read_file(&source) {
            Ok(bytes) => docs.push(Doc { dest, bytes }),
            Err(reason) => println!(
                "cargo:warning=docs: skipping {}: {reason}",
                source.display()
            ),
        }
    }
    Ok(docs)
}

/// Recursively add every `*.md` under `dir` (its relative path is the
/// destination). A missing top-level directory is fine, but an unreadable
/// directory or entry is an error (a build must not silently lose documents); a
/// symlinked directory is not followed, so the walk cannot loop. Two files that
/// differ only by case would be two spellings of one document (and collide on a
/// case-folding host checkout), so the second is skipped with a warning.
fn collect_dir(
    dir: &Path,
    manifest_dir: &Path,
    found: &mut BTreeMap<String, PathBuf>,
) -> Result<(), String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound && dir.ends_with(DISK_ROOT) => {
            return Ok(()); // no docs/ at all: nothing to embed
        }
        Err(err) => return Err(format!("cannot read {}: {err}", dir.display())),
    };
    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|err| format!("cannot list {}: {err}", dir.display()))?;
        paths.push(entry.path());
    }
    paths.sort(); // deterministic walk (the map sorts again at the end)
    for path in paths {
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_dir() => collect_dir(&path, manifest_dir, found)?,
            Ok(meta) if meta.is_file() && is_markdown(&path) => {
                match destination(&path, manifest_dir) {
                    Some(dest) if found.keys().any(|key| key.eq_ignore_ascii_case(&dest)) => {
                        println!(
                            "cargo:warning=docs: skipping {}: collides with another file on a case-insensitive volume",
                            path.display()
                        );
                    }
                    Some(dest) => {
                        found.insert(dest, path);
                    }
                    None => println!(
                        "cargo:warning=docs: skipping non-UTF-8 path {}",
                        path.display()
                    ),
                }
            }
            Err(err) => return Err(format!("cannot stat {}: {err}", path.display())),
            _ => {}
        }
    }
    Ok(())
}

/// `NAME.EXT`-style markdown check, case-insensitive (`README.MD` counts).
fn is_markdown(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
}

/// The `/`-separated image destination for a path under `manifest_dir`
/// (`...\docs\architecture\boot.md` -> `docs/architecture/boot.md`). `None`
/// when a path component is not valid UTF-8 and so cannot name an image path.
fn destination(path: &Path, manifest_dir: &Path) -> Option<String> {
    let relative = path.strip_prefix(manifest_dir).ok()?;
    let mut dest = String::new();
    for component in relative.components() {
        let part = component.as_os_str().to_str()?;
        if !dest.is_empty() {
            dest.push('/');
        }
        dest.push_str(part);
    }
    Some(dest)
}

/// Read a document, rejecting oversized and non-UTF-8 files. Markdown is text,
/// so a byte stream that is not UTF-8 is not something the Docs app can render.
fn read_file(path: &Path) -> Result<Vec<u8>, String> {
    let len = std::fs::metadata(path)
        .map_err(|err| err.to_string())?
        .len();
    if len > MAX_FILE_BYTES {
        return Err(format!(
            "{len} bytes exceeds the {} KiB limit",
            MAX_FILE_BYTES / 1024
        ));
    }
    let bytes = std::fs::read(path).map_err(|err| err.to_string())?;
    if std::str::from_utf8(&bytes).is_err() {
        return Err("not valid UTF-8".to_string());
    }
    Ok(bytes)
}

/// Add the docs tree to the image.
///
/// The host tests (`build_support/tests`) exercise [`collect`] and this.
pub fn embed(sink: &mut dyn Sink, manifest_dir: &Path) {
    // A directory is watched recursively, so an added or removed doc triggers a
    // rebuild; the README is watched separately (it lives outside `docs/`).
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join(DISK_ROOT).display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join(README).display()
    );

    let docs = match collect(manifest_dir) {
        Ok(docs) => docs,
        Err(reason) => panic!("docs: {reason}"),
    };
    if docs.is_empty() {
        println!("cargo:warning=docs: no markdown found under {}", DISK_ROOT);
        return;
    }
    let count = docs.len();
    for doc in docs {
        sink.add_bytes(&doc.dest, doc.bytes);
    }
    println!("cargo:warning=docs: embedded {count} markdown file(s) at /{DISK_ROOT}/");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    /// A fresh, empty directory unique to one test.
    fn temp_root(tag: &str) -> PathBuf {
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "lazyos_docs_embed_{}_{tag}_{unique}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(root: &Path, relative: &str, bytes: &[u8]) -> PathBuf {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn dests(docs: &[Doc]) -> Vec<&str> {
        docs.iter().map(|doc| doc.dest.as_str()).collect()
    }

    #[test]
    fn collects_nested_docs_and_readme_sorted() {
        let root = temp_root("sorted");
        write(&root, "README.md", b"readme");
        write(&root, "docs/zz.md", b"zz");
        write(&root, "docs/architecture/boot.md", b"boot");
        write(&root, "docs/a.md", b"a");

        let docs = collect(&root).unwrap();

        assert_eq!(
            dests(&docs),
            [
                "docs/README.md",
                "docs/a.md",
                "docs/architecture/boot.md",
                "docs/zz.md",
            ]
        );
        assert_eq!(docs[2].bytes, b"boot");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_docs_dir_is_empty_not_an_error() {
        let root = temp_root("empty");
        assert!(collect(&root).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn empty_docs_dir_is_empty() {
        let root = temp_root("emptydir");
        std::fs::create_dir_all(root.join("docs")).unwrap();
        assert!(collect(&root).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn extension_match_is_case_insensitive() {
        let root = temp_root("case");
        write(&root, "docs/UPPER.MD", b"upper");
        assert_eq!(dests(&collect(&root).unwrap()), ["docs/UPPER.MD"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn ignores_non_markdown_files() {
        let root = temp_root("nonmd");
        write(&root, "docs/notes.txt", b"nope");
        write(&root, "docs/keep.md", b"yes");
        assert_eq!(dests(&collect(&root).unwrap()), ["docs/keep.md"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn skips_oversized_file() {
        let root = temp_root("huge");
        let big = vec![b'x'; MAX_FILE_BYTES as usize + 1];
        write(&root, "docs/big.md", &big);
        write(&root, "docs/small.md", b"ok");
        assert_eq!(dests(&collect(&root).unwrap()), ["docs/small.md"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn skips_non_utf8_content() {
        let root = temp_root("utf8");
        write(&root, "docs/binary.md", &[0xff, 0xfe, 0x00]);
        write(&root, "docs/text.md", b"ok");
        assert_eq!(dests(&collect(&root).unwrap()), ["docs/text.md"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn docs_readme_wins_over_root_readme() {
        let root = temp_root("readme");
        write(&root, "README.md", b"root");
        write(&root, "docs/README.md", b"docs");
        let docs = collect(&root).unwrap();
        assert_eq!(dests(&docs), ["docs/README.md"]);
        assert_eq!(docs[0].bytes, b"docs");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn root_readme_loses_to_any_case_docs_readme() {
        let root = temp_root("readmecase");
        write(&root, "README.md", b"root");
        write(&root, "docs/README.MD", b"docs");
        let docs = collect(&root).unwrap();
        assert_eq!(dests(&docs), ["docs/README.MD"]);
        assert_eq!(docs[0].bytes, b"docs");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn case_colliding_docs_keep_only_the_first() {
        let root = temp_root("collide");
        write(&root, "docs/Guide.md", b"one");
        write(&root, "docs/guide.MD", b"two");
        assert_eq!(collect(&root).unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_docs_path_that_is_a_file_is_an_error() {
        let root = temp_root("notdir");
        write(&root, "docs", b"not a directory");
        assert!(collect(&root).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn many_files_are_all_collected_once_sorted() {
        let root = temp_root("many");
        for index in 0..200 {
            write(
                &root,
                &format!("docs/f{index:03}.md"),
                format!("body {index}").as_bytes(),
            );
        }
        let docs = collect(&root).unwrap();
        assert_eq!(docs.len(), 200);
        let mut sorted = dests(&docs);
        let original = sorted.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted, original, "destinations must be unique and sorted");
        let _ = std::fs::remove_dir_all(&root);
    }
}
