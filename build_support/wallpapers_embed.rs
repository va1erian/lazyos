//! The desktop pictures a desktop image ships in [`fhs::share::WALLPAPERS`]:
//! two abstract ones and two LazyOS ones, rendered by `tools/wallpaper/gen.py`
//! into `assets/wallpapers`. Settings lists the directory and LazyShell draws
//! the picture `sys/ui/wallpaper` names.

use crate::os_image::Sink;

/// `(file name, bytes)` of every picture, in the order Settings lists them.
pub const PICTURES: [(&str, &[u8]); 4] = [
    (
        "Aurora.jpg",
        include_bytes!("../assets/wallpapers/Aurora.jpg"),
    ),
    (
        "Dunes.jpg",
        include_bytes!("../assets/wallpapers/Dunes.jpg"),
    ),
    (
        "LazyOS-Green.jpg",
        include_bytes!("../assets/wallpapers/LazyOS-Green.jpg"),
    ),
    (
        "LazyOS-Night.jpg",
        include_bytes!("../assets/wallpapers/LazyOS-Night.jpg"),
    ),
];

/// Add the pictures to the OS file list.
pub fn embed(sink: &mut dyn Sink) {
    println!("cargo:rerun-if-changed=build_support/wallpapers_embed.rs");
    println!("cargo:rerun-if-changed=assets/wallpapers");
    for (name, bytes) in PICTURES {
        sink.add_bytes(
            &format!("{}/{name}", fhs::share::WALLPAPERS),
            bytes.to_vec(),
        );
    }
}
