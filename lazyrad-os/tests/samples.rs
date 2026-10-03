//! The LazyOS-only sample projects (`lazyrad-os/samples/`): each must load
//! and compile cleanly with LazyRAD's own check, and declare the Messenger
//! permissions its scripts need when packaged.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use lazyrad_os::platform::{Home, LazyOsPlatform};
use lazyrad_runtime::platform::Platform;

fn samples() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("samples");
    let mut dirs: Vec<PathBuf> = fs::read_dir(&root)
        .expect("lazyrad-os/samples exists")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    dirs
}

#[test]
fn every_sample_checks_clean() {
    let dirs = samples();
    assert!(!dirs.is_empty());
    for dir in dirs {
        let report = lazyrad_runtime::check_project(&dir)
            .unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
        assert!(report.is_empty(), "{}: {report:?}", dir.display());
    }
}

#[test]
fn the_messenger_sample_declares_what_it_uses() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("samples/messenger");
    let script = fs::read_to_string(dir.join("main_form.rhai")).unwrap();
    let found = LazyOsPlatform::ide(Home::from_var(Some(OsStr::new("/home/user"))))
        .script_permissions(&[&script]);
    assert_eq!(found.interfaces, ["os.lazy.confd.v1", "os.lazy.input.v1"]);
    assert_eq!(found.topics, ["subscribe:system/confd/changed/#"]);
}
