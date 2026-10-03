//! The sample files every image ships in [`fhs::share::SAMPLES`]: two text
//! files, the Docs app's test document and LazyWriter's sample picture. The
//! sample packages (`pkgdemo.lzp`, Doom, the MOD player) have their own embed
//! modules.

use crate::os_image::Sink;

/// The Docs app's test document (`xui-app/docs/testdata/`), relative to the
/// manifest dir: opened by the Docs screenshot session through the Open
/// dialog, and by hand in the Docs app or the Editor.
const TESTDOC: &str = "xui-app/docs/testdata/testdoc.md";
/// LazyWriter's sample picture (issue #533), relative to the manifest dir:
/// inserted into a document by its screenshot session.
const WRITER_SAMPLE_IMAGE: &str = "assets/samples/writer-sample.png";

/// Add the samples to the OS file list.
pub fn embed(sink: &mut dyn Sink) {
    println!("cargo:rerun-if-changed=build_support/samples_embed.rs");
    println!("cargo:rerun-if-changed={TESTDOC}");
    println!("cargo:rerun-if-changed={WRITER_SAMPLE_IMAGE}");
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
    sink.add_bytes(
        fhs::share::WRITER_SAMPLE_IMAGE,
        include_bytes!("../assets/samples/writer-sample.png").to_vec(),
    );
}
