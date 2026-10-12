use super::*;

#[test]
fn the_project_file_is_the_first_lrp_by_name() {
    let dir = std::env::temp_dir().join(format!("lazyrad-os-dev-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    assert_eq!(project_file(&dir), None);
    std::fs::write(dir.join("b.lrp"), "").unwrap();
    std::fs::write(dir.join("a.lrp"), "").unwrap();
    std::fs::write(dir.join("a.rhai"), "").unwrap();
    assert_eq!(project_file(&dir), Some(dir.join("a.lrp")));
    let _ = std::fs::remove_dir_all(&dir);
}
