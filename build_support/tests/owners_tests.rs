//! Which producer placed each file (`build_support/os_owners.rs`).

use crate::os_image::{OsFiles, Sink};

fn add(files: &mut OsFiles, path: &str, text: &str) {
    files.add_bytes(path, text.as_bytes().to_vec());
}

#[test]
fn unscoped_files_belong_to_the_build_script() {
    let mut files = OsFiles::default();
    add(&mut files, "/system/bin/init", "a");
    assert_eq!(files.owners().owner("/system/bin/init"), Some("build.rs"));
}

#[test]
fn a_scope_names_the_owner_and_ends_with_the_call() {
    let mut files = OsFiles::default();
    files.produced_by("docs_embed", |f| add(f, "/system/docs/a.md", "a"));
    add(&mut files, "/system/bin/init", "b");
    assert_eq!(
        files.owners().owner("/system/docs/a.md"),
        Some("docs_embed")
    );
    assert_eq!(files.owners().owner("/system/bin/init"), Some("build.rs"));
    assert!(files.owners().collisions().is_empty());
}

#[test]
fn a_second_producer_for_a_path_is_a_collision_and_wins_the_bytes() {
    let mut files = OsFiles::default();
    files.produced_by("drivers", |f| add(f, "/system/bin/netd", "old"));
    files.produced_by("linuxapps_embed", |f| add(f, "/system/bin/netd", "new"));
    let collisions = files.owners().collisions();
    assert_eq!(collisions.len(), 1);
    assert_eq!(collisions[0].path, "/system/bin/netd");
    assert_eq!(collisions[0].producers, vec!["drivers", "linuxapps_embed"]);
    assert_eq!(
        files.owners().owner("/system/bin/netd"),
        Some("linuxapps_embed")
    );
    assert_eq!(files.files().len(), 1);
}

#[test]
fn one_producer_replacing_its_own_path_is_not_a_collision() {
    let mut files = OsFiles::default();
    files.produced_by("xui_embed", |f| {
        add(f, "/system/packages/os.lazy.paint.lzp", "plain");
        add(f, "/system/packages/os.lazy.paint.lzp", "autostart");
    });
    assert!(files.owners().collisions().is_empty());
}

#[test]
fn the_table_lists_every_path_with_its_owner_in_path_order() {
    let mut files = OsFiles::default();
    files.produced_by("b_embed", |f| add(f, "/system/share/b", "b"));
    files.produced_by("a_embed", |f| add(f, "/system/share/a", "a"));
    assert_eq!(
        files.owners().table(),
        "/system/share/a\ta_embed\n/system/share/b\tb_embed\n"
    );
}

#[test]
fn the_table_file_is_written_and_strictness_decides_whether_a_collision_fails() {
    let mut files = OsFiles::default();
    files.produced_by("one", |f| add(f, "/system/x", "1"));
    files.produced_by("two", |f| add(f, "/system/x", "2"));
    let root = std::env::temp_dir().join(format!("lazyos-owners-{}", std::process::id()));
    std::fs::create_dir_all(root.join("target")).unwrap();
    // Not `write_owners`: CI sets `LAZYOS_OWNER_STRICT`, which that reads.
    assert!(files.write_owners_with(&root, false).is_ok());
    let table = std::fs::read_to_string(root.join("target/os-owners.tsv")).unwrap();
    assert_eq!(
        table,
        "/system/x	two
"
    );
    assert!(files
        .write_owners_with(&root, true)
        .unwrap_err()
        .contains("1 path"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_nested_scope_restores_the_outer_producer() {
    let mut files = OsFiles::default();
    files.produced_by("outer", |f| {
        f.produced_by("inner", |g| add(g, "/system/a", "a"));
        add(f, "/system/b", "b");
    });
    assert_eq!(files.owners().owner("/system/a"), Some("inner"));
    assert_eq!(files.owners().owner("/system/b"), Some("outer"));
}
