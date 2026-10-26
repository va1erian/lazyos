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

/// Make a link at `link` to `target`, or `None` where the host does not allow it (Windows
/// without developer mode).
fn link_file(target: &Path, link: &Path) -> Option<()> {
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(target, link);
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_file(target, link);
    made.ok()
}

#[test]
fn a_link_in_the_stage_is_followed_and_a_broken_one_fails_the_build() {
    let dir = staged("links", &["bin/lazyvm", "real/lazyrt.jar"]);
    std::fs::create_dir_all(dir.join("lib")).unwrap();
    let Some(()) = link_file(&dir.join("real/lazyrt.jar"), &dir.join("lib/lazyrt.jar")) else {
        eprintln!("skipped: this host cannot make symbolic links");
        return;
    };
    let files = placed(&dir);
    let jar = format!("{}/lib/lazyrt.jar", fhs::LAZYVM);
    assert!(files
        .iter()
        .any(|(path, mode)| *path == jar && *mode == 0o644));
    link_file(&dir.join("gone"), &dir.join("lib/broken")).unwrap();
    let error = place(&mut OsFiles::default(), &dir).unwrap_err();
    assert!(error.contains("broken"), "{error}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_name_that_is_not_utf8_fails_the_build_naming_it() {
    let dir = staged("names", &["bin/lazyvm"]);
    #[cfg(unix)]
    let name: std::ffi::OsString =
        <std::ffi::OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(b"bad\xff").to_os_string();
    #[cfg(windows)]
    let name: std::ffi::OsString = std::os::windows::ffi::OsStringExt::from_wide(&[0x62, 0xD800]);
    std::fs::create_dir_all(dir.join("tests")).unwrap();
    if std::fs::write(dir.join("tests").join(&name), b"x").is_err() {
        eprintln!("skipped: this filesystem refuses the name");
        return;
    }
    let error = place(&mut OsFiles::default(), &dir).unwrap_err();
    assert!(error.contains("not UTF-8"), "{error}");
    let _ = std::fs::remove_dir_all(dir);
}

/// Make a directory link at `link` to `target` (a junction on Windows, which needs no
/// privilege), or `None` where the host does not allow it.
fn link_dir(target: &Path, link: &Path) -> Option<()> {
    #[cfg(unix)]
    return std::os::unix::fs::symlink(target, link).ok();
    #[cfg(windows)]
    {
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .ok()?;
        made.status.success().then_some(())
    }
}

#[test]
fn a_branching_link_cycle_fails_at_once_naming_the_directory() {
    let dir = staged("cycle", &["bin/lazyvm", "tests/loop/A.class"]);
    let looped = dir.join("tests").join("loop");
    for name in ["a", "b"] {
        if link_dir(&looped, &looped.join(name)).is_none() {
            eprintln!("skipped: this host cannot make directory links");
            return;
        }
    }
    let error = place(&mut OsFiles::default(), &dir).unwrap_err();
    assert!(error.contains("reached a second time"), "{error}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_linked_directory_is_walked_as_if_it_were_there() {
    let dir = staged("dirlink", &["bin/lazyvm"]);
    let outside = staged("dirlink-target", &["java/lang/Object.class"]);
    std::fs::create_dir_all(dir.join("lib")).unwrap();
    if link_dir(&outside, &dir.join("lib").join("lazyrt")).is_none() {
        eprintln!("skipped: this host cannot make directory links");
        return;
    }
    let files = placed(&dir);
    let object = format!("{}/lib/lazyrt/java/lang/Object.class", fhs::LAZYVM);
    assert!(files
        .iter()
        .any(|(path, mode)| *path == object && *mode == 0o644));
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_dir_all(outside);
}
