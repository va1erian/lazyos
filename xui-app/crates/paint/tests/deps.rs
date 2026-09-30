//! Proves the library's app logic is `xui-core`-only: no `xui-win32`, no
//! `winit`, and `xui-canvas` is a dev-dependency used only by the tests' offscreen
//! renderer. The LazyOS binary in `xui-app/src/bin/paint.rs` supplies the
//! backend. Works offline.

use std::path::{Path, PathBuf};

const LIB_DIR: &str = env!("CARGO_MANIFEST_DIR");

fn files_under(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read_dir") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            files_under(&path, out);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn the_manifest_never_pulls_in_win32() {
    let manifest = std::fs::read_to_string(Path::new(LIB_DIR).join("Cargo.toml")).unwrap();
    assert!(
        !manifest.contains("xui-win32") && !manifest.contains("winit"),
        "the paint crate must not depend on a windowing backend"
    );
    // `xui-canvas` is only the tests' offscreen renderer.
    assert!(
        manifest.contains("[dev-dependencies]") && manifest.contains("xui-canvas"),
        "the offscreen renderer is a dev-dependency"
    );
    assert!(
        !manifest.contains("canvas = ["),
        "the library has no canvas feature to opt a backend into"
    );
}

#[test]
fn no_library_source_names_a_backend() {
    let mut files = Vec::new();
    files_under(&Path::new(LIB_DIR).join("src"), &mut files);
    assert!(!files.is_empty());
    for file in files {
        let source = std::fs::read_to_string(&file).unwrap();
        assert!(
            !source.contains("xui_canvas") && !source.contains("xui-canvas"),
            "backend selection must live in the LazyOS bin, not {file:?}"
        );
    }
}
