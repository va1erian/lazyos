//! `pkgstore::tree` over an in-memory tree: extraction, documentation
//! publishing, removal, confinement and repair after a stop at every step.

mod common;

use common::{MemTree, Sample, SYSTEM_NAME};
use pkgstore::layout::{self, DATA_MODE, EXEC_MODE};
use pkgstore::tree::{self, TreeError};

const DOCS: &str = "/docs/apps/org.lazy.counter";

/// The filesystem half of `pkgd`'s install: extract, stage the docs, (the
/// record and activation happen here in `pkgd`), publish the docs, delete the
/// superseded version.
fn install(
    fs: &mut MemTree,
    sample: &Sample,
    previous: Option<&Sample>,
) -> Result<(), TreeError<common::Fault>> {
    let path = layout::install_path(&sample.install_dir()).unwrap();
    tree::remove_tree(fs, &path)?;
    tree::extract(fs, sample, &path)?;
    let staged = tree::stage_docs(fs, sample, SYSTEM_NAME)?;
    tree::commit_docs(fs, SYSTEM_NAME, staged)?;
    if let Some(old) = previous {
        tree::remove_tree(fs, &layout::install_path(&old.install_dir()).unwrap())?;
    }
    Ok(())
}

fn remove(fs: &mut MemTree, sample: &Sample) -> Result<(), TreeError<common::Fault>> {
    tree::remove_tree(fs, &layout::install_path(&sample.install_dir()).unwrap())?;
    tree::withdraw_docs(fs, SYSTEM_NAME)?;
    tree::remove_if_empty(fs, &layout::app_dir(SYSTEM_NAME).unwrap());
    Ok(())
}

fn readme(fs: &MemTree) -> Option<String> {
    fs.files
        .get(&format!("{DOCS}/README.md"))
        .map(|(data, _)| String::from_utf8_lossy(&data[..30]).into_owned())
}

#[test]
fn install_upgrade_and_remove_leave_nothing_behind() {
    let mut fs = MemTree::new();
    let v1 = Sample::v(1);
    install(&mut fs, &v1, None).unwrap();
    let root = layout::install_path(&v1.install_dir()).unwrap();
    assert_eq!(fs.files[&format!("{root}/bin/counter.elf")].1, EXEC_MODE);
    assert_eq!(fs.files[&format!("{root}/icons/app-16.png")].1, DATA_MODE);
    assert_eq!(fs.files[&format!("{DOCS}/README.md")].1, DATA_MODE);
    assert!(fs.files.contains_key(&format!("{DOCS}/guide/usage.md")));
    assert!(readme(&fs).unwrap().contains("v1"));

    let v2 = Sample::v(2);
    install(&mut fs, &v2, Some(&v1)).unwrap();
    assert!(readme(&fs).unwrap().contains("v2"));
    assert!(fs.stat_path(&root).is_none(), "the old version stayed");
    assert_eq!(
        fs.below("/docs/apps")
            .iter()
            .filter(|p| p.contains('~'))
            .count(),
        0
    );

    remove(&mut fs, &v2).unwrap();
    assert!(fs.below("/apps").is_empty(), "{:?}", fs.below("/apps"));
    assert!(
        fs.below("/docs/apps").is_empty(),
        "{:?}",
        fs.below("/docs/apps")
    );
    assert!(
        fs.files.contains_key("/docs/os/README.md"),
        "the OS docs were touched"
    );
}

#[test]
fn a_version_without_docs_withdraws_them() {
    let mut fs = MemTree::new();
    install(&mut fs, &Sample::v(1), None).unwrap();
    let bare = Sample {
        version: 2,
        with_docs: false,
    };
    install(&mut fs, &bare, Some(&Sample::v(1))).unwrap();
    assert!(fs.below("/docs/apps").is_empty());
    assert!(!fs.dirs.contains_key(DOCS));
}

#[test]
fn removals_are_confined_to_apps_and_app_docs() {
    let mut fs = MemTree::new();
    for path in [
        "/",
        "/apps",
        "/docs",
        "/docs/apps",
        "/docs/os",
        "/docs/os/README.md",
        "/logs",
        "/system",
        "/apps/../docs",
        "/docs/apps/../os",
        "/home/user",
        "/appsx/a",
    ] {
        assert!(!tree::deletable(path), "{path}");
        assert!(
            matches!(tree::remove_tree(&mut fs, path), Err(TreeError::Bad(_))),
            "{path}"
        );
    }
    assert!(fs.files.contains_key("/docs/os/README.md"));
    assert!(tree::deletable("/apps/org.lazy.counter"));
    assert!(tree::deletable("/docs/apps/org.lazy.counter~new"));
}

#[test]
fn hostile_entries_never_reach_the_filesystem() {
    struct Hostile;
    impl pkgstore::tree::Source for Hostile {
        fn entries(&self) -> Vec<(&str, bool)> {
            vec![("docs/../../os/README.md", false)]
        }
        fn read(&self, _: &str) -> Result<Vec<u8>, String> {
            Ok(b"pwned".to_vec())
        }
    }
    let mut fs = MemTree::new();
    assert!(tree::stage_docs(&mut fs, &Hostile, SYSTEM_NAME).is_err());
    assert!(tree::extract(&mut fs, &Hostile, "/apps/org.lazy.counter/1.0.0-00000000").is_err());
    assert_eq!(fs.files["/docs/os/README.md"].0, b"os");
}

/// A stop after every mutating call of an upgrade, then the startup repair:
/// the docs are exactly v1 or exactly v2 (never a mixture, never absent), and
/// no `~new`/`~old` copy survives.
#[test]
fn an_upgrade_stopped_anywhere_repairs_to_one_version() {
    let mut saw_old = false;
    let mut saw_new = false;
    for fuel in 0..200 {
        let mut fs = MemTree::new();
        install(&mut fs, &Sample::v(1), None).unwrap();
        fs.fuel = Some(fuel);
        let finished = install(&mut fs, &Sample::v(2), Some(&Sample::v(1))).is_ok();
        fs.fuel = None;
        tree::repair_docs(&mut fs).unwrap();
        let text = readme(&fs).expect("the docs vanished");
        let usage = fs
            .files
            .get(&format!("{DOCS}/guide/usage.md"))
            .expect("half the docs vanished");
        let usage = String::from_utf8_lossy(&usage.0[..30]).into_owned();
        let v2 = text.contains("v2");
        assert_eq!(v2, usage.contains("v2"), "fuel {fuel}: mixed versions");
        saw_old |= !v2;
        saw_new |= v2;
        assert!(
            fs.below("/docs/apps").iter().all(|p| !p.contains('~')),
            "fuel {fuel}"
        );
        if finished {
            assert!(v2, "a finished upgrade left the old docs");
            break;
        }
    }
    assert!(saw_old && saw_new);
}

trait StatPath {
    fn stat_path(&self, path: &str) -> Option<()>;
}

impl StatPath for MemTree {
    fn stat_path(&self, path: &str) -> Option<()> {
        (self.dirs.contains_key(path) || self.files.contains_key(path)).then_some(())
    }
}
