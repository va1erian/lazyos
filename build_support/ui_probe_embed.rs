//! The UI probe switch (issue #538): `LAZYOS_UI_PROBE=1` writes the marker
//! file `fhs::etc::UI_PROBE`, and the shell and the LazyRAD player, which
//! find it, print the screen rectangles of their named widgets as `UI:RECT` /
//! `UI:WIDGET` serial lines (`tools/screenshot/README.md`, "Clicking by
//! position or name"); the same variable compiles the compositor's window
//! lines in (`cfg(lazyos_ui_probe)`, `user/build.rs`). A debug switch like
//! `LAZYOS_LABEL_TRACE`: off by default, and a normal image prints nothing.
//! The marker is listed in the image manifest like every embedded file, so a
//! rebuild without the switch removes it again.

use crate::os_image::Sink;

/// Add the marker when `LAZYOS_UI_PROBE=1`.
pub fn embed(sink: &mut dyn Sink) {
    println!("cargo:rerun-if-env-changed=LAZYOS_UI_PROBE");
    if std::env::var_os("LAZYOS_UI_PROBE").as_deref() != Some(std::ffi::OsStr::new("1")) {
        return;
    }
    println!(
        "cargo:warning=LAZYOS_UI_PROBE: {} written; windows and widgets print UI: lines",
        fhs::etc::UI_PROBE
    );
    sink.add_bytes(fhs::etc::UI_PROBE, b"1\n".to_vec());
}
