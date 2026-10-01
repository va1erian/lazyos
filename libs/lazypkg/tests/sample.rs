//! The sample package the image ships (`tools/pkg/build_samples.py`) is read by
//! the same reader the installer uses. Skipped (with a note) when the sample has
//! not been built, so the host suite never depends on the xui toolchain.

use std::path::PathBuf;

fn sample() -> Option<Vec<u8>> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/pkg/PKGDEMO.LZP");
    match std::fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(_) => {
            eprintln!("note: {} is not built; skipping", path.display());
            None
        }
    }
}

#[test]
fn the_counter_sample_opens_and_says_what_it_needs() {
    let Some(bytes) = sample() else { return };
    let package = lazypkg::Package::open(&bytes).expect("the sample is a valid package");
    let manifest = package.manifest();
    assert_eq!(manifest.app.system_name, "org.lazy.counter");
    assert_eq!(manifest.entry.binary, "bin/counter.elf");
    assert!(manifest.entry.is_linux());
    assert_eq!(manifest.entry.args, ["--client"]);
    assert_eq!(
        manifest.permissions.interfaces,
        ["os.lazy.display.v1", "os.lazy.input.v1"]
    );
    let elf = package
        .read("bin/counter.elf")
        .expect("the program reads back");
    assert!(elf.starts_with(b"\x7fELF"), "not an ELF");
    assert!(package.install_dir().starts_with("org.lazy.counter/1.0.0-"));
}
