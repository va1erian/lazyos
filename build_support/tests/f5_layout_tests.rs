//! The F5 layout (issue #509): the desktop apps ship as core packages in
//! `/system/packages` with an index, and an F4-built image updated in place
//! loses the app programs F3 put in `/system/bin` and the `xapps.lst` list.

use std::path::{Path, PathBuf};

use ext2fs::memio::MemIo;
use ext2fs::{Ext2, Geometry};

use crate::core_packages::{self, parse_autostart, short_of};
use crate::os_image::{write_volume, OsFiles, Sink, Source};
use crate::os_layout::{dirs, parse_passwd};

const STAMP: i64 = 1_700_000_000;
const PASSWD: &str = "root:0:0:toor:/root:sh\nalice:1000:1000:lazy:/home/alice:sh\n";

/// A scratch `target/pkg/core` with two built packages and their `core.lst`.
fn core_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lazyos-core-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("autostart")).unwrap();
    let mut list = String::from("# short system_name version file digest autostart_file autostart_digest\n");
    for short in ["editor", "sysmon"] {
        let plain = format!("os.lazy.{short}-0.1.0.lzp");
        std::fs::write(dir.join(&plain), format!("plain {short}")).unwrap();
        std::fs::write(dir.join("autostart").join(&plain), format!("auto {short}")).unwrap();
        list.push_str(&format!(
            "{short} os.lazy.{short} 0.1.0 {plain} {} autostart/{plain} {}\n",
            "a".repeat(64),
            "b".repeat(64)
        ));
    }
    std::fs::write(dir.join("core.lst"), list).unwrap();
    dir
}

fn source_bytes(source: &Source) -> Vec<u8> {
    match source {
        Source::Bytes(bytes) => bytes.clone(),
        Source::Path(path) => std::fs::read(path).unwrap(),
    }
}

#[test]
fn the_autostart_list_names_core_apps_by_stem_short_id_or_system_name() {
    assert_eq!(parse_autostart(None), ["terminal"]);
    assert_eq!(parse_autostart(Some("none")), Vec::<String>::new());
    assert_eq!(
        parse_autostart(Some("term, sysmon,os.lazy.paint,,")),
        ["terminal", "sysmon", "paint"]
    );
    assert_eq!(short_of("term"), "terminal");
    assert!(core_packages::is_core_stem("editor"));
    assert!(!core_packages::is_core_stem("term"), "the Terminal is a program");
    assert!(!core_packages::is_core_stem("installer"));
    assert!(!core_packages::is_core_stem("devices"));
}

#[test]
fn packages_land_in_system_packages_with_their_index() {
    let dir = core_dir("embed");
    let built = core_packages::built(&dir);
    assert_eq!(built.len(), 2);
    let mut sink = OsFiles::default();
    let wanted: Vec<_> = built.iter().collect();
    core_packages::embed(&mut sink, &wanted, &["sysmon".to_string()]);
    let files = sink.files();
    let find = |path: &str| files.iter().find(|f| f.path == path).unwrap_or_else(|| panic!("{path}"));
    let editor = find("/system/packages/os.lazy.editor.lzp");
    assert_eq!(source_bytes(&editor.source), b"plain editor");
    assert_eq!(editor.mode, 0o644);
    // The one LAZYOS_XUI_AUTOSTART names ships in its autostart variant.
    let sysmon = find("/system/packages/os.lazy.sysmon.lzp");
    assert_eq!(source_bytes(&sysmon.source), b"auto sysmon");
    let index = String::from_utf8(source_bytes(&find(fhs::system::PACKAGES_INDEX).source)).unwrap();
    assert!(index.contains(&format!("os.lazy.editor 0.1.0 {}\n", "a".repeat(64))), "{index}");
    assert!(
        index.contains(&format!("os.lazy.sysmon 0.1.0 {} autostart\n", "b".repeat(64))),
        "{index}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn nothing_built_embeds_nothing() {
    let dir = std::env::temp_dir().join(format!("lazyos-core-none-{}", std::process::id()));
    assert!(core_packages::built(Path::new(&dir)).is_empty());
    let mut sink = OsFiles::default();
    core_packages::embed(&mut sink, &[], &[]);
    assert!(sink.files().is_empty(), "no index without packages");
}

#[test]
fn an_f4_image_updated_by_the_f5_build_drops_the_app_programs() {
    let io = MemIo::new(8 << 20);
    let geometry = Geometry {
        block_size: 4096,
        blocks_count: (8 << 20) / 4096,
        bytes_per_inode: 16 * 1024,
    };
    ext2fs::format(&io, geometry, "lazyos-root", [5; 16], STAMP).unwrap();
    let volume = Ext2::open(Box::new(io.clone()), || STAMP).unwrap();
    let layout = dirs(&parse_passwd(PASSWD));
    let mut f4 = OsFiles::default();
    f4.add_bytes(fhs::etc::PASSWD, PASSWD.as_bytes().to_vec());
    f4.add_bytes(fhs::bin::EDITOR, b"editor elf".to_vec());
    f4.add_bytes(fhs::bin::TERMINAL, b"terminal elf".to_vec());
    f4.add_bytes(fhs::system::XAPPS_LST, b"/system/bin/editor\n".to_vec());
    let old = write_volume(&volume, None, &layout, &f4.files(), STAMP).unwrap();

    let dir = core_dir("update");
    let built = core_packages::built(&dir);
    let mut f5 = OsFiles::default();
    f5.add_bytes(fhs::etc::PASSWD, PASSWD.as_bytes().to_vec());
    f5.add_bytes(fhs::bin::TERMINAL, b"terminal elf".to_vec());
    core_packages::embed(&mut f5, &built.iter().collect::<Vec<_>>(), &[]);
    let new = write_volume(&volume, Some(&old), &layout, &f5.files(), STAMP).unwrap();

    assert!(volume.lookup(fhs::bin::EDITOR).is_err(), "the Editor is a package now");
    assert!(volume.lookup(fhs::system::XAPPS_LST).is_err());
    assert_eq!(volume.read_file(fhs::bin::TERMINAL).unwrap(), b"terminal elf");
    assert_eq!(
        volume.read_file("/system/packages/os.lazy.editor.lzp").unwrap(),
        b"plain editor"
    );
    assert!(new.entries.contains_key(fhs::system::PACKAGES_INDEX));
    assert!(!new.entries.contains_key(fhs::bin::EDITOR));
    drop(volume);
    let problems = ext2fs::check::fsck(&io.snapshot());
    assert!(problems.is_empty(), "{problems:#?}");
    let _ = std::fs::remove_dir_all(&dir);
}
