//! `LAZYOS_SRB2KART=1` (docs/srb2kart-port-plan.md): the 1 GiB default OS
//! volume (1.5 GiB beside the WebKit browser) and where the package, the data
//! and the credits land.

use crate::os_disk::{
    default_size, parse_size, DEFAULT_OS_SIZE, RUSTC_EXTRA, SRB2KART_EXTRA, WEBKIT_EXTRA,
};
use crate::os_image::{OsFiles, Sink, Source};
use crate::srb2kart_embed::{data_destination, data_files};

#[test]
fn srb2kart_raises_the_default_os_size_to_one_gib() {
    assert_eq!(DEFAULT_OS_SIZE + SRB2KART_EXTRA, 1 << 30);
    assert_eq!(default_size(true, false, false), parse_size("1G").unwrap());
    assert_eq!(default_size(false, false, false), DEFAULT_OS_SIZE);
    assert!(default_size(true, false, false) > default_size(false, false, false));
}

#[test]
fn rustc_adds_its_gib_to_whatever_else_is_selected() {
    assert_eq!(
        default_size(false, false, true),
        parse_size("1536M").unwrap()
    );
    assert_eq!(
        default_size(true, true, true),
        default_size(true, true, false) + RUSTC_EXTRA
    );
}

#[test]
fn webkit_raises_the_default_os_size_to_one_gib() {
    // Three programs of about 107 MiB each, their package and the image do not fit the 512 MiB
    // default with room to write.
    assert_eq!(DEFAULT_OS_SIZE + WEBKIT_EXTRA, parse_size("1G").unwrap());
    assert_eq!(default_size(false, true, false), parse_size("1G").unwrap());
    let three_programs_and_an_image: u64 = 3 * (107 << 20) + (128 << 20);
    assert!(three_programs_and_an_image < default_size(false, true, false));
    assert!(default_size(false, true, false) > default_size(false, false, false));
    // The two add up when both are selected: SRB2Kart's data (460 MiB) beside
    // the browser's three programs filled a 1 GiB volume and the update died
    // halfway.
    assert_eq!(
        default_size(true, true, false),
        parse_size("1536M").unwrap(),
        "512 MiB base plus 512 MiB for each"
    );
    assert!(default_size(true, true, false) > default_size(true, false, false));
    assert!(default_size(true, true, false) > default_size(false, true, false));
}

#[test]
fn data_lands_under_system_share_srb2kart() {
    assert_eq!(
        data_destination("music.kart"),
        "/system/share/srb2kart/music.kart"
    );
    assert_eq!(
        data_destination("CREDITS.txt"),
        format!("{}/CREDITS.txt", fhs::share::SRB2KART_DATA)
    );
    assert!(fhs::share::SRB2KART_LZP.starts_with(fhs::share::SAMPLES));
}

#[test]
fn every_data_file_is_found_in_name_order() {
    let dir = std::env::temp_dir().join(format!("srb2kart-embed-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    for name in ["srb2.srb", "gfx.kart", "sub/extra.kart"] {
        std::fs::write(dir.join(name), name.as_bytes()).unwrap();
    }
    let found = data_files(&dir);
    let names: Vec<&str> = found.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, ["gfx.kart", "srb2.srb", "sub/extra.kart"]);
    let mut files = OsFiles::default();
    for (relative, path) in found {
        files.add_file(&data_destination(&relative), path);
    }
    let all = files.files();
    assert!(all
        .iter()
        .any(|file| file.path == "/system/share/srb2kart/sub/extra.kart"));
    assert!(all
        .iter()
        .all(|file| matches!(file.source, Source::Path(_))));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_missing_data_directory_has_no_files() {
    assert!(data_files(std::path::Path::new("/nonexistent/srb2kart/data")).is_empty());
}
