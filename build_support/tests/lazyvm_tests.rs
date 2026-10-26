//! `LAZYOS_LAZYVM=1` (docs/agents/lazyvm.md): LazyVM's stage lands as it is under
//! `fhs::LAZYVM`, its `bin/` executable and nothing else, and a missing stage or VM is an error.

use std::path::{Path, PathBuf};

use crate::linuxapps_embed::lazyvm::{place, stage_dir};
use crate::os_image::OsFiles;

/// A stage holding `names`, each file's bytes its own name.
fn staged(tag: &str, names: &[&str]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lazyvm-embed-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for name in names {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, name.as_bytes()).unwrap();
    }
    dir
}

fn placed(dir: &Path) -> Vec<(String, u16)> {
    let mut files = OsFiles::default();
    place(&mut files, dir).unwrap();
    files
        .files()
        .into_iter()
        .map(|file| (file.path, file.mode))
        .collect()
}

#[test]
fn the_stage_lands_under_system_lazyvm_with_only_bin_executable() {
    let dir = staged(
        "layout",
        &[
            "bin/lazyvm",
            "lib/lazyrt.jar",
            "lib/lazyrt/java/lang/Object.class",
            "tests/s1/Hello.class",
            "tests/bin/Data.class",
        ],
    );
    let files = placed(&dir);
    let mode = |rel: &str| {
        let path = format!("{}/{rel}", fhs::LAZYVM);
        files.iter().find(|(p, _)| *p == path).map(|(_, m)| *m)
    };
    assert_eq!(mode("bin/lazyvm"), Some(0o755));
    assert_eq!(mode("lib/lazyrt.jar"), Some(0o644));
    assert_eq!(mode("lib/lazyrt/java/lang/Object.class"), Some(0o644));
    assert_eq!(mode("tests/s1/Hello.class"), Some(0o644));
    // Only the stage's own top-level `bin/` holds programs.
    assert_eq!(mode("tests/bin/Data.class"), Some(0o644));
    assert_eq!(files.len(), 5);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_missing_stage_or_vm_is_an_error_that_names_it() {
    let mut files = OsFiles::default();
    let gone = std::env::temp_dir().join("lazyvm-embed-no-such-stage");
    let error = place(&mut files, &gone).unwrap_err();
    assert!(error.contains("no LazyVM stage") && error.contains("LAZYOS_LAZYVM_STAGE"));
    let dir = staged("novm", &["lib/lazyrt.jar"]);
    assert!(place(&mut files, &dir).unwrap_err().contains("bin/lazyvm"));
    assert_eq!(files.len(), 0);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_stage_defaults_under_target_and_follows_the_override() {
    let root = Path::new("checkout");
    assert_eq!(
        stage_dir(root, None),
        root.join("target").join("lazyvm").join("stage")
    );
    assert_eq!(stage_dir(root, Some("mine".into())), root.join("mine"));
    let absolute = std::env::temp_dir().join("stage");
    assert_eq!(stage_dir(root, Some(absolute.clone().into())), absolute);
}
