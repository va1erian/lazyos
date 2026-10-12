//! The generated sample files every image ships in [`fhs::share::SAMPLES`]:
//! two text files and the Docs app's test document. LazyWriter's sample
//! picture and the Archiver's sample archives are data assets
//! (`assets/samples`, `assets_embed.rs`); the sample packages (`pkgdemo.lzp`,
//! Doom, the MOD player) have their own embed modules. The Terminal's terminfo
//! entry, the other generated file under `/system/share`, is compiled here too
//! (`libs/terminfo`; no `tic` is needed on the build host).

use crate::os_image::Sink;

/// The Docs app's test document (`xui-app/docs/testdata/`), relative to the
/// manifest dir: opened by the Docs screenshot session through the Open
/// dialog, and by hand in the Docs app or the Editor.
const TESTDOC: &str = "xui-app/docs/testdata/testdoc.md";

/// The Terminal's terminfo source (docs/terminal-plan.md T5).
const TERMINFO_SRC: &str = "assets/terminfo/xterm-256color.src";

/// Add the samples (and the terminfo entry) to the OS file list.
pub fn embed(sink: &mut dyn Sink) {
    println!("cargo:rerun-if-changed={TERMINFO_SRC}");
    sink.add_bytes(
        fhs::share::TERMINFO_XTERM_256COLOR,
        terminfo::compile_source(include_str!("../assets/terminfo/xterm-256color.src"))
            .expect("assets/terminfo/xterm-256color.src compiles"),
    );
    println!("cargo:rerun-if-changed=build_support/samples_embed.rs");
    println!("cargo:rerun-if-changed={TESTDOC}");
    let sample = |name: &str| format!("{}/{name}", fhs::share::SAMPLES);
    sink.add_bytes(
        &sample("hello.txt"),
        b"Hello from LazyOS!\n\nThis file lives on the ext2 OS volume.\nYou are reading it through the block driver and the ext2 reader.\n".to_vec(),
    );
    sink.add_bytes(
        &sample("notes.txt"),
        b"LazyOS notes\n-----------\n- single-tasking x86_64 kernel\n- tiny-skia graphics\n- PS/2 keyboard + mouse\n- ext2 OS volume plus a FAT /boot\n".to_vec(),
    );
    sink.add_bytes(
        fhs::share::TESTDOC,
        include_bytes!("../xui-app/docs/testdata/testdoc.md").to_vec(),
    );
}
