//! What `pkgd` does at startup with the core packages, on a real ext2 volume
//! (`libs/ext2fs`, the driver the kernel mounts `/` with): the
//! `pkgstore::provision` decisions carried out with the same `pkgstore::tree`
//! calls `pkgd` makes, its reused package and extraction buffers, and its
//! hash-chained audit log. The `confd` rows are a map here.
//!
//! [`Heap`] measures what matters for `pkgd`'s memory: the user heap never
//! returns a block over 64 KiB, so every such allocation is growth that only a
//! restart gives back. Filesystem calls are excluded (in `pkgd` they happen in
//! the kernel).

#![allow(dead_code)]

pub mod lzp;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::BTreeMap;

use ext2fs::{AttrChange, Ext2, Ext2Error, FileKind, Owner};
use lazyos_crypto::hex;
use messenger_generated::os_lazy_pkgd_v1::{encode_pkg_event, PkgEvent};
use pkgstore::audit::Chain;
use pkgstore::layout;
use pkgstore::provision::{self, Action, Current, Shipped, Tally};
use pkgstore::tree::{self, Node, TreeFs};

/// The largest block the user heap recycles.
const RECYCLED: usize = 64 * 1024;

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static PAUSED: Cell<bool> = const { Cell::new(false) };
    static GROWTH: Cell<usize> = const { Cell::new(0) };
}

/// The test binary's allocator: `System`, plus the never-returned growth of
/// the code under measurement.
pub struct Heap;

unsafe impl GlobalAlloc for Heap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if layout.size() > RECYCLED {
            let _ = COUNTING.try_with(|on| {
                if on.get() && !PAUSED.with(Cell::get) {
                    GROWTH.with(|g| g.set(g.get() + layout.size()));
                }
            });
        }
        System.alloc(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout)
    }
}

/// Run `f` with the growth counter on; returns its result and the growth.
pub fn measure<T>(f: impl FnOnce() -> T) -> (T, usize) {
    GROWTH.with(|g| g.set(0));
    COUNTING.with(|on| on.set(true));
    let result = f();
    COUNTING.with(|on| on.set(false));
    (result, GROWTH.with(Cell::get))
}

fn unmeasured<T>(f: impl FnOnce() -> T) -> T {
    let was = PAUSED.with(|p| p.replace(true));
    let result = f();
    PAUSED.with(|p| p.set(was));
    result
}

/// [`TreeFs`] over the library, as root; its calls are not measured.
pub struct Ext2Tree<'a>(pub &'a Ext2);

impl TreeFs for Ext2Tree<'_> {
    type Error = Ext2Error;

    fn stat(&mut self, path: &str) -> Result<Option<Node>, Ext2Error> {
        unmeasured(|| match self.0.lookup(path) {
            Ok(meta) if meta.kind == FileKind::Dir => Ok(Some(Node::Dir)),
            Ok(meta) => Ok(Some(Node::File(meta.size))),
            Err(Ext2Error::NotFound) => Ok(None),
            Err(error) => Err(error),
        })
    }

    fn mkdir(&mut self, path: &str) -> Result<(), Ext2Error> {
        unmeasured(|| self.0.mkdir(path, 0o755, Owner::ROOT).map(|_| ()))
    }

    fn write(&mut self, path: &str, data: &[u8]) -> Result<(), Ext2Error> {
        unmeasured(|| {
            match self.0.create(path, 0o644, Owner::ROOT) {
                Ok(_) => {}
                Err(Ext2Error::Exists) => self.0.truncate(path, 0)?,
                Err(error) => return Err(error),
            }
            match self.0.write(path, 0, data)? {
                written if written == data.len() => Ok(()),
                _ => Err(Ext2Error::NoSpace),
            }
        })
    }

    fn chmod(&mut self, path: &str, mode: u16) -> Result<(), Ext2Error> {
        let change = AttrChange {
            mode: Some(mode),
            ..AttrChange::default()
        };
        unmeasured(|| self.0.setattr(path, &change).map(|_| ()))
    }

    fn list(&mut self, path: &str) -> Result<Vec<String>, Ext2Error> {
        unmeasured(|| {
            Ok(self
                .0
                .readdir(path)?
                .into_iter()
                .map(|entry| entry.name)
                .filter(|name| name != "." && name != "..")
                .collect())
        })
    }

    fn remove(&mut self, path: &str) -> Result<(), Ext2Error> {
        unmeasured(|| match self.0.lookup(path)?.kind {
            FileKind::Dir => self.0.rmdir(path),
            FileKind::File => self.0.unlink(path),
        })
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<(), Ext2Error> {
        unmeasured(|| self.0.rename(from, to))
    }
}

/// An image's `/system/packages`: the archives and their index.
pub struct Image {
    pub packages: Vec<(Shipped, Vec<u8>)>,
}

impl Image {
    /// An image from `(system_name, version, archive)`; the index digest is
    /// the archive's real SHA-256 unless `lie` names a package whose index
    /// line is wrong (a damaged image).
    pub fn new(packages: Vec<(&str, &str, Vec<u8>)>) -> Image {
        Image {
            packages: packages
                .into_iter()
                .map(|(name, version, bytes)| {
                    let shipped = Shipped {
                        system_name: name.into(),
                        version: version.into(),
                        digest: hex::encode(&lazyos_crypto::sha256::sha256(&bytes)),
                    };
                    (shipped, bytes)
                })
                .collect(),
        }
    }

    /// The index file the build writes, read back as `pkgd` reads it.
    pub fn shipped(&self) -> Vec<Shipped> {
        let shipped: Vec<Shipped> = self.packages.iter().map(|(s, _)| s.clone()).collect();
        provision::parse_index(&provision::format_index(&shipped)).unwrap()
    }

    fn bytes(&self, system_name: &str) -> &[u8] {
        &self
            .packages
            .iter()
            .find(|(s, _)| s.system_name == system_name)
            .unwrap()
            .1
    }

    pub fn largest(&self) -> usize {
        self.packages.iter().map(|(_, b)| b.len()).max().unwrap_or(0)
    }
}

/// One `sys/apps/<system_name>` row.
#[derive(Clone, Debug)]
pub struct Row {
    pub version: String,
    pub digest: String,
    pub install_dir: String,
    pub core: bool,
}

/// `pkgd`'s state across passes (one pass per boot).
pub struct Pkgd {
    pub rows: BTreeMap<String, Row>,
    pub stamp: Option<String>,
    pub chain: Chain,
    /// The package file buffer and the extraction buffer, kept for the life
    /// of the service as `pkgd` keeps them.
    buffer: Vec<u8>,
    scratch: Vec<u8>,
}

impl Pkgd {
    pub fn new() -> Pkgd {
        Pkgd {
            rows: BTreeMap::new(),
            stamp: None,
            chain: Chain::default(),
            buffer: Vec::new(),
            scratch: Vec::new(),
        }
    }

    fn current(&self) -> Vec<Current> {
        self.rows
            .iter()
            .map(|(name, row)| Current {
                system_name: name.clone(),
                version: row.version.clone(),
                digest: row.digest.clone(),
                core: row.core,
            })
            .collect()
    }

    /// One provisioning pass, `None` when the stamp short-circuits it.
    pub fn provision(&mut self, fs: &Ext2, image: &Image) -> Option<Tally> {
        let shipped = image.shipped();
        if provision::up_to_date(self.stamp.as_deref(), &shipped, &self.current()) {
            return None;
        }
        // Sized once for the largest package, so reading each one reuses it.
        self.buffer.reserve(image.largest());
        let mut tally = Tally::default();
        let mut actions = provision::plan(&shipped, &self.current());
        provision::largest_first(&mut actions, |name| image.bytes(name).len());
        for action in actions {
            match action {
                Action::Install(name) | Action::Upgrade(name) => {
                    let upgrade = self.rows.contains_key(&name);
                    let index = shipped.iter().find(|s| s.system_name == name).unwrap();
                    match self.install(fs, image.bytes(&name), index) {
                        Ok(()) if upgrade => tally.upgraded += 1,
                        Ok(()) => tally.installed += 1,
                        Err(_) => tally.failed += 1,
                    }
                }
                Action::Keep { .. } => tally.kept += 1,
                Action::MarkCore(name) => self.rows.get_mut(&name).unwrap().core = true,
                Action::Demote(name) => self.rows.get_mut(&name).unwrap().core = false,
            }
        }
        self.stamp = Some(provision::stamp(&shipped));
        Some(tally)
    }

    /// `pkgd`'s install of one shipped package: read it into the kept buffer,
    /// open and check it, build the new directory and docs, switch the row,
    /// retire the old directory; audited as `op=provision`.
    fn install(&mut self, fs: &Ext2, bytes: &[u8], index: &Shipped) -> Result<(), String> {
        self.buffer.clear();
        self.buffer.extend_from_slice(bytes);
        let buffer = std::mem::take(&mut self.buffer);
        let outcome = self.install_bytes(fs, &buffer, index);
        self.buffer = buffer;
        let event = PkgEvent {
            op: "provision".into(),
            system_name: index.system_name.clone(),
            version: index.version.clone(),
            digest: index.digest.clone(),
            ok: outcome.is_ok(),
            detail: outcome.clone().err().unwrap_or_default(),
            ..PkgEvent::default()
        };
        unmeasured(|| append_log(fs, &mut self.chain, &event));
        outcome
    }

    fn install_bytes(&mut self, fs: &Ext2, bytes: &[u8], index: &Shipped) -> Result<(), String> {
        let package = lazypkg::Package::open(bytes).map_err(|e| format!("{e}"))?;
        let digest = hex::encode(&package.digest());
        if digest != index.digest || package.manifest().app.system_name != index.system_name {
            return Err("the package does not match the image's index".into());
        }
        let name = index.system_name.as_str();
        let install_dir = package.install_dir();
        let path = layout::install_path(&install_dir).unwrap();
        let mut tree_fs = Ext2Tree(fs);
        let discard = |tree_fs: &mut Ext2Tree| {
            let _ = tree::remove_tree(tree_fs, &path);
            if let Ok(staging) = pkgstore::docs::staging_dir(name) {
                let _ = tree::remove_tree(tree_fs, &staging);
            }
            tree::remove_if_empty(tree_fs, &layout::app_dir(name).unwrap());
        };
        tree::remove_tree(&mut tree_fs, &path).map_err(|e| format!("{e:?}"))?;
        let staged = tree::extract_with(&mut tree_fs, &package, &path, &mut self.scratch)
            .and_then(|_| tree::stage_docs(&mut tree_fs, &package, name));
        let staged = match staged {
            Ok(staged) => staged,
            Err(error) => {
                discard(&mut tree_fs);
                return Err(format!("{error:?}"));
            }
        };
        let previous = self.rows.insert(
            name.to_string(),
            Row {
                version: index.version.clone(),
                digest,
                install_dir: install_dir.clone(),
                core: true,
            },
        );
        if let Some(old) = previous.filter(|old| old.install_dir != install_dir) {
            tree::remove_tree(&mut tree_fs, &layout::install_path(&old.install_dir).unwrap())
                .map_err(|e| format!("{e:?}"))?;
        }
        tree::commit_docs(&mut tree_fs, name, staged).map_err(|e| format!("{e:?}"))
    }
}

/// `pkgd`'s audit append: one chained line at the end of `pkg.log`.
pub fn append_log(fs: &Ext2, chain: &mut Chain, event: &PkgEvent) {
    let line = chain.append(&encode_pkg_event(event).unwrap());
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
