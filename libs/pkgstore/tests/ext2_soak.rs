//! Soak: 1 000 install / upgrade / remove cycles of the sample package on a
//! real ext2 volume made and checked by `libs/ext2fs` (the driver the kernel
//! mounts `/` with). Afterwards `/apps` and `/docs/apps` are empty, the
//! hash-chained `/logs/pkg.log` verifies, the inode count is back where it
//! started and the independent fsck-style checker finds nothing (no orphan or
//! leaked inode or block). The kernel suite runs the same cycles through the
//! VFS adapter (`ext2_suite::pkg_tree`).

mod common;

use common::{Sample, SYSTEM_NAME};
use ext2fs::check::fsck;
use ext2fs::memio::MemIo;
use ext2fs::{AttrChange, Ext2, Ext2Error, FileKind, Geometry, Owner};
use messenger_generated::os_lazy_pkgd_v1::{encode_pkg_event, PkgEvent};
use pkgstore::audit::{verify, Chain};
use pkgstore::layout;
use pkgstore::tree::{self, Node, TreeError, TreeFs};

const CYCLES: u32 = 1000;

fn clock() -> i64 {
    1_700_000_000
}

/// [`TreeFs`] over the library, as root, the way the kernel adapter drives it.
struct Ext2Tree<'a>(&'a Ext2);

impl TreeFs for Ext2Tree<'_> {
    type Error = Ext2Error;

    fn stat(&mut self, path: &str) -> Result<Option<Node>, Ext2Error> {
        match self.0.lookup(path) {
            Ok(meta) if meta.kind == FileKind::Dir => Ok(Some(Node::Dir)),
            Ok(meta) => Ok(Some(Node::File(meta.size))),
            Err(Ext2Error::NotFound) => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn mkdir(&mut self, path: &str) -> Result<(), Ext2Error> {
        self.0.mkdir(path, 0o755, Owner::ROOT).map(|_| ())
    }

    fn write(&mut self, path: &str, data: &[u8]) -> Result<(), Ext2Error> {
        match self.0.create(path, 0o644, Owner::ROOT) {
            Ok(_) => {}
            Err(Ext2Error::Exists) => self.0.truncate(path, 0)?,
            Err(error) => return Err(error),
        }
        match self.0.write(path, 0, data)? {
            written if written == data.len() => Ok(()),
            _ => Err(Ext2Error::NoSpace),
        }
    }

    fn chmod(&mut self, path: &str, mode: u16) -> Result<(), Ext2Error> {
        let change = AttrChange {
            mode: Some(mode),
            ..AttrChange::default()
        };
        self.0.setattr(path, &change).map(|_| ())
    }

    fn list(&mut self, path: &str) -> Result<Vec<String>, Ext2Error> {
        Ok(self
            .0
            .readdir(path)?
            .into_iter()
            .map(|entry| entry.name)
            .filter(|name| name != "." && name != "..")
            .collect())
    }

    fn remove(&mut self, path: &str) -> Result<(), Ext2Error> {
        match self.0.lookup(path)?.kind {
            FileKind::Dir => self.0.rmdir(path),
            FileKind::File => self.0.unlink(path),
        }
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<(), Ext2Error> {
        self.0.rename(from, to)
    }
}

/// `pkgd`'s audit append: one chained line at the end of `pkg.log`.
fn audit(fs: &Ext2, chain: &mut Chain, op: &str, sample: &Sample) {
    let event = PkgEvent {
        op: op.into(),
        system_name: SYSTEM_NAME.into(),
        version: format!("1.0.{}", sample.version),
        install_dir: sample.install_dir(),
        digest: format!("{:064x}", sample.version),
        actor_uid: 1000,
        ok: true,
        detail: String::new(),
    };
    let line = chain.append(&encode_pkg_event(&event).unwrap());
    let end = match fs.lookup(layout::LOG_FILE) {
        Ok(meta) => meta.size,
        Err(Ext2Error::NotFound) => {
            fs.create(layout::LOG_FILE, 0o644, Owner::ROOT).unwrap();
            0
        }
        Err(error) => panic!("{error:?}"),
    };
    assert_eq!(
        fs.write(layout::LOG_FILE, end, line.as_bytes()).unwrap(),
        line.len()
    );
}

fn install(
    fs: &mut Ext2Tree,
    sample: &Sample,
    previous: Option<&Sample>,
) -> Result<(), TreeError<Ext2Error>> {
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

fn remove(fs: &mut Ext2Tree, sample: &Sample) -> Result<(), TreeError<Ext2Error>> {
    tree::remove_tree(fs, &layout::install_path(&sample.install_dir()).unwrap())?;
    tree::withdraw_docs(fs, SYSTEM_NAME)?;
    tree::remove_if_empty(fs, &layout::app_dir(SYSTEM_NAME).unwrap());
    Ok(())
}

#[test]
fn a_thousand_install_upgrade_remove_cycles_leave_a_clean_volume() {
    let io = MemIo::new(32 << 20);
    let geometry = Geometry {
        block_size: 4096,
        blocks_count: (32 << 20) / 4096,
        bytes_per_inode: 16 * 1024,
    };
    ext2fs::format(&io, geometry, "lazyos-root", [9; 16], clock()).unwrap();
    let mut chain = Chain::default();
    let mut baseline = None;
    for cycle in (0..CYCLES).step_by(100) {
        // A remount every 100 cycles, as reboots between sessions would.
        let fs = Ext2::open(Box::new(io.clone()), clock).unwrap();
        if cycle == 0 {
            for dir in ["/apps", "/docs", "/docs/apps", "/logs"] {
                fs.mkdir_p(dir, 0o755, 0, 0).unwrap();
            }
        }
        for step in 0..100 {
            let n = (cycle + step) * 2;
            let (old, new) = (Sample::v(n + 1), Sample::v(n + 2));
            let mut tree_fs = Ext2Tree(&fs);
            install(&mut tree_fs, &old, None).unwrap();
            audit(&fs, &mut chain, "install", &old);
            install(&mut tree_fs, &new, Some(&old)).unwrap();
            audit(&fs, &mut chain, "install", &new);
            remove(&mut tree_fs, &new).unwrap();
            audit(&fs, &mut chain, "remove", &new);
            if baseline.is_none() {
                baseline = Some(fs.free_inodes().unwrap());
            }
            assert_eq!(
                Some(fs.free_inodes().unwrap()),
                baseline,
                "cycle {}: inodes leaked",
                cycle + step
            );
        }
        fs.flush().unwrap();
    }
    let fs = Ext2::open(Box::new(io.clone()), clock).unwrap();
    let mut tree_fs = Ext2Tree(&fs);
    assert!(
        tree_fs.list("/apps").unwrap().is_empty(),
        "/apps is not empty"
    );
    assert!(
        tree_fs.list("/docs/apps").unwrap().is_empty(),
        "/docs/apps is not empty"
    );
    let size = fs.lookup(layout::LOG_FILE).unwrap().size as usize;
    let mut log = vec![0u8; size];
    fs.read(layout::LOG_FILE, 0, &mut log).unwrap();
    let verified = verify(std::str::from_utf8(&log).unwrap()).expect("pkg.log verifies");
    assert_eq!(verified.count, u64::from(CYCLES) * 3);
    drop(fs);
    let problems = fsck(&io.snapshot());
    assert!(problems.is_empty(), "{problems:#?}");
}
