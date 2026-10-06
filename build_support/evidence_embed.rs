//! The evidence-only programs. `init` never starts them in the desktop
//! profile, so the image leaves their ELFs out entirely: the deliberate crash
//! service (issue #93), whose restart-with-backoff demo is the `flaky` row,
//! the clipboard demo pair (issue #115), which `clipboardd` spawns under
//! `demo=1`, and `async_echo` (#91, #309).

use std::path::PathBuf;

use crate::os_image::Sink;

/// Add the evidence programs unless this is the desktop profile.
pub fn embed(files: &mut dyn Sink, desktop: bool) {
    if desktop {
        return;
    }
    for (artifact, path) in [
        ("CARGO_BIN_FILE_USER_flaky", fhs::bin::FLAKY),
        ("CARGO_BIN_FILE_USER_clipcopy", fhs::bin::CLIPCP),
        ("CARGO_BIN_FILE_USER_clippaste", fhs::bin::CLIPPASTE),
        ("CARGO_BIN_FILE_USER_async_echo", fhs::bin::ASYNC_ECHO),
    ] {
        let elf = std::env::var_os(artifact).unwrap_or_else(|| panic!("{artifact} not found"));
        files.add_file(path, PathBuf::from(elf));
    }
}
