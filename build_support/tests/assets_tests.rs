//! Data assets (issue #454): the manifest, the path mapping, the size caps
//! and the checked-in `assets/` tree.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use crate::assets_embed::{
    check_licence, check_path, collect, image_path, parse_manifest, roots, select, Asset, Install,
    Limits, LIMITS, MANIFEST,
};

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// A fresh, empty directory unique to one test.
fn temp_root(tag: &str) -> PathBuf {
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "lazyos_assets_{}_{tag}_{unique}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(root: &Path, relative: &str, bytes: &[u8]) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// The repository root (this crate lives in `build_support/tests`).
fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The checked-in tree's installed assets for an image with or without the
/// desktop shell.
pub fn checked_in(shell: bool) -> Vec<(String, Asset)> {
    let tree = collect(&repo().join("assets"), LIMITS).expect("assets/ is valid");
    select(&[tree], shell, LIMITS).expect("assets/ fits")
}

const SMALL: Limits = Limits {
    file: 16,
    total: 24,
};

#[test]
fn a_manifest_line_has_four_fields() {
    let entries = parse_manifest(
        "# comment\n\nmods/song.mod | CC0-1.0 | all | made for the demo\n\
         wallpapers/a.jpg | CC-BY-4.0 OR MIT | desktop | someone, https://example.org\n",
    )
    .unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].path, "mods/song.mod");
    assert_eq!(entries[0].install, Install::All);
    assert_eq!(entries[1].install, Install::Desktop);
    assert_eq!(entries[1].source, "someone, https://example.org");
    for bad in [
        "a.mod | CC0-1.0 | all",
        "a.mod | CC0-1.0 | all | x | y",
        "a.mod | CC0-1.0 | sometimes | x",
        "a.mod | CC0-1.0 | all | ",
    ] {
        assert!(parse_manifest(bad).is_err(), "{bad}");
    }
}

#[test]
fn a_path_listed_twice_is_rejected() {
    let text = "a.mod | CC0-1.0 | all | x\nA.MOD | CC0-1.0 | all | x\n";
    assert!(parse_manifest(text).unwrap_err().contains("twice"));
}

#[test]
fn only_plain_relative_paths_map() {
    for good in ["a.mod", "mods/sub/a.mod", "fonts/Droid Sans.ttf"] {
        assert!(check_path(good).is_ok(), "{good}");
    }
    for bad in [
        "/etc/passwd",
        "../escape",
        "mods/../../x",
        "mods//a",
        "./a",
        "a\\b",
        "",
        MANIFEST,
    ] {
        assert!(check_path(bad).is_err(), "{bad}");
    }
    assert_eq!(image_path("mods/a.mod"), "/system/share/mods/a.mod");
    assert_eq!(
        image_path("samples/writer-sample.png"),
        fhs::share::WRITER_SAMPLE_IMAGE
    );
}

#[test]
fn only_free_licences_are_accepted() {
    for good in [
        "CC0-1.0",
        "OFL-1.1",
        "GPL-3.0-or-later",
        "MIT OR Apache-2.0",
    ] {
        assert!(check_licence(good).is_ok(), "{good}");
    }
    for bad in [
        "",
        "CC-BY-NC-4.0",
        "proprietary",
        "MIT OR CC-BY-ND-4.0",
        "unknown",
    ] {
        assert!(check_licence(bad).is_err(), "{bad}");
    }
}

#[test]
fn a_file_without_a_manifest_entry_fails_the_tree() {
    let root = temp_root("unlisted");
    write(&root, MANIFEST, b"mods/a.mod | CC0-1.0 | all | x\n");
    write(&root, "mods/a.mod", b"song");
    write(&root, "mods/b.mod", b"unlisted");
    let err = collect(&root, LIMITS).unwrap_err();
    assert!(
        err.contains("mods/b.mod") && err.contains("no entry"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn files_without_any_manifest_fail_the_tree() {
    let root = temp_root("nomanifest");
    write(&root, "mods/a.mod", b"song");
    assert!(collect(&root, LIMITS).unwrap_err().contains(MANIFEST));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_entry_without_a_file_fails_the_tree() {
    let root = temp_root("missing");
    write(&root, MANIFEST, b"mods/gone.mod | CC0-1.0 | all | x\n");
    assert!(collect(&root, LIMITS)
        .unwrap_err()
        .contains("does not exist"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_missing_or_empty_tree_is_empty() {
    let root = temp_root("empty");
    assert!(collect(&root.join("absent"), LIMITS).unwrap().is_empty());
    assert!(collect(&root, LIMITS).unwrap().is_empty());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_per_file_cap_is_enforced() {
    let root = temp_root("filecap");
    write(&root, MANIFEST, b"a.bin | CC0-1.0 | all | x\n");
    write(&root, "a.bin", &[0; 16]);
    assert_eq!(collect(&root, SMALL).unwrap()[0].len, 16);
    write(&root, "a.bin", &[0; 17]);
    assert!(collect(&root, SMALL).unwrap_err().contains("per-file"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_total_cap_counts_only_installed_assets() {
    let root = temp_root("totalcap");
    write(
        &root,
        MANIFEST,
        b"a.bin | CC0-1.0 | all | x\nb.bin | CC0-1.0 | desktop | x\nc.bin | CC0-1.0 | none | x\n",
    );
    for name in ["a.bin", "b.bin", "c.bin"] {
        write(&root, name, &[0; 16]);
    }
    let tree = collect(&root, SMALL).unwrap();
    let console = select(&[tree.clone()], false, SMALL).unwrap();
    assert_eq!(console.len(), 1, "only `all` without the shell");
    assert_eq!(console[0].0, "/system/share/a.bin");
    let err = select(&[tree], true, SMALL).unwrap_err();
    assert!(err.contains("add up to 32 bytes"), "{err}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn two_trees_may_not_install_one_destination() {
    let (one, two) = (temp_root("one"), temp_root("two"));
    for root in [&one, &two] {
        write(root, MANIFEST, b"mods/a.mod | CC0-1.0 | all | x\n");
        write(root, "mods/a.mod", b"song");
    }
    let trees = [
        collect(&one, LIMITS).unwrap(),
        collect(&two, LIMITS).unwrap(),
    ];
    assert!(select(&trees, false, LIMITS)
        .unwrap_err()
        .contains("both install"));
    let _ = std::fs::remove_dir_all(&one);
    let _ = std::fs::remove_dir_all(&two);
}

#[test]
fn extra_trees_follow_the_checked_in_one() {
    let base = Path::new("repo");
    assert_eq!(roots(base, None), [base.join("assets")]);
    let extra = std::env::join_paths(["x", "y"]).unwrap();
    assert_eq!(
        roots(base, Some(&extra)),
        [base.join("assets"), PathBuf::from("x"), PathBuf::from("y")]
    );
}

#[test]
fn the_checked_in_tree_is_fully_listed() {
    // Every file under assets/ has a manifest line with a free licence.
    let tree = collect(&repo().join("assets"), LIMITS).unwrap();
    assert!(tree.iter().any(|asset| asset.entry.path == "fonts/OFL.txt"));
    assert!(tree.iter().all(|asset| !asset.entry.source.is_empty()));
}

#[test]
fn the_checked_in_tree_installs_what_the_apps_open() {
    let console: Vec<String> = checked_in(false)
        .into_iter()
        .map(|(dest, _)| dest)
        .collect();
    for wanted in [
        fhs::share::WRITER_SAMPLE_IMAGE,
        fhs::share::ARCHIVER_SAMPLE_ZIP,
        fhs::share::ARCHIVER_SAMPLE_7Z,
    ] {
        assert!(console.iter().any(|dest| dest == wanted), "{wanted}");
    }
    assert!(!console
        .iter()
        .any(|dest| dest.starts_with(fhs::share::WALLPAPERS)));
    assert!(
        !console.iter().any(|dest| dest.contains("/fonts/")),
        "fonts are compiled in"
    );
    let desktop = checked_in(true);
    let pictures = desktop
        .iter()
        .filter(|(dest, _)| dest.starts_with(fhs::share::WALLPAPERS))
        .count();
    assert_eq!(pictures, 4);
}
