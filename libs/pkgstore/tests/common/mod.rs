//! Test support shared by the `tree` integration binaries: a stand-in for the
//! sample package (the Counter demo, `tools/pkg/samples/counter`) and an
//! in-memory [`TreeFs`].

#![allow(dead_code)]

use std::collections::BTreeMap;

use pkgstore::tree::{Node, Source, TreeFs};

/// The sample package's shape at `version`: a manifest, the program, the three
/// required icons and one page of documentation. Each version's bytes differ,
/// as an upgrade's do.
pub struct Sample {
    pub version: u32,
    pub with_docs: bool,
}

impl Sample {
    pub fn v(version: u32) -> Sample {
        Sample {
            version,
            with_docs: true,
        }
    }

    /// The install directory `lazypkg` would derive (`<sn>/<version>-<digest8>`).
    pub fn install_dir(&self) -> String {
        format!(
            "{}/1.0.{}-{:08x}",
            SYSTEM_NAME,
            self.version,
            0x0c0u32 + self.version
        )
    }
}

pub const SYSTEM_NAME: &str = "org.lazy.counter";

impl Source for Sample {
    fn entries(&self) -> Vec<(&str, bool)> {
        let mut entries = vec![
            ("manifest.toml", false),
            ("bin/", true),
            ("bin/counter.elf", false),
            ("icons/app-16.png", false),
            ("icons/app-32.png", false),
            ("icons/app-128.png", false),
        ];
        if self.with_docs {
            entries.push(("docs/", true));
            entries.push(("docs/README.md", false));
            entries.push(("docs/guide/usage.md", false));
        }
        entries
    }

    fn read(&self, name: &str) -> Result<Vec<u8>, String> {
        let size = match name {
            "bin/counter.elf" => 6000,
            "docs/README.md" => 300,
            _ => 120,
        };
        let mut data = format!("{name} v{}\n", self.version).into_bytes();
        data.resize(size + self.version as usize % 7, b'x');
        Ok(data)
    }
}

/// An in-memory tree: directories and files by absolute path, with an
/// optional fault after `fuel` mutating calls.
#[derive(Clone, Default)]
pub struct MemTree {
    pub dirs: BTreeMap<String, ()>,
    pub files: BTreeMap<String, (Vec<u8>, u16)>,
    pub fuel: Option<u32>,
}

/// The injected failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fault;

impl MemTree {
    pub fn new() -> MemTree {
        let mut tree = MemTree::default();
        for dir in ["/", "/apps", "/docs", "/docs/apps", "/docs/os", "/logs"] {
            tree.dirs.insert(dir.to_string(), ());
        }
        tree.files
            .insert("/docs/os/README.md".to_string(), (b"os".to_vec(), 0o644));
        tree
    }

    fn spend(&mut self) -> Result<(), Fault> {
        match &mut self.fuel {
            Some(0) => Err(Fault),
            Some(left) => {
                *left -= 1;
                Ok(())
            }
            None => Ok(()),
        }
    }

    fn parent(path: &str) -> String {
        match path.rsplit_once('/') {
            Some(("", _)) => "/".to_string(),
            Some((parent, _)) => parent.to_string(),
            None => "/".to_string(),
        }
    }

    fn children(&self, path: &str) -> Vec<String> {
        let prefix = if path == "/" {
            "/".to_string()
        } else {
            format!("{path}/")
        };
        let direct = |p: &String| {
            p.starts_with(&prefix) && !p[prefix.len()..].contains('/') && p.len() > prefix.len()
        };
        self.dirs
            .keys()
            .chain(self.files.keys())
            .filter(|p| direct(p))
            .map(|p| p[prefix.len()..].to_string())
            .collect()
    }

    /// Every path below `root`.
    pub fn below(&self, root: &str) -> Vec<String> {
        let prefix = format!("{root}/");
        self.dirs
            .keys()
            .chain(self.files.keys())
            .filter(|p| p.starts_with(&prefix))
            .cloned()
            .collect()
    }
}

impl TreeFs for MemTree {
    type Error = Fault;

    fn stat(&mut self, path: &str) -> Result<Option<Node>, Fault> {
        if self.dirs.contains_key(path) {
            return Ok(Some(Node::Dir));
        }
        Ok(self
            .files
            .get(path)
            .map(|(data, _)| Node::File(data.len() as u64)))
    }

    fn mkdir(&mut self, path: &str) -> Result<(), Fault> {
        self.spend()?;
        assert!(
            self.dirs.contains_key(&Self::parent(path)),
            "mkdir {path} without its parent"
        );
        assert!(
            !self.files.contains_key(path) && !self.dirs.contains_key(path),
            "mkdir over {path}"
        );
        self.dirs.insert(path.to_string(), ());
        Ok(())
    }

    fn write(&mut self, path: &str, data: &[u8]) -> Result<(), Fault> {
        self.spend()?;
        assert!(
            self.dirs.contains_key(&Self::parent(path)),
            "write {path} without its parent"
        );
        self.files.insert(path.to_string(), (data.to_vec(), 0o644));
        Ok(())
    }

    fn append(&mut self, path: &str, data: &[u8]) -> Result<(), Fault> {
        self.spend()?;
        let file = self.files.get_mut(path).expect("append to a missing file");
        file.0.extend_from_slice(data);
        Ok(())
    }

    fn chmod(&mut self, path: &str, mode: u16) -> Result<(), Fault> {
        self.spend()?;
        self.files.get_mut(path).expect("chmod of a missing file").1 = mode;
        Ok(())
    }

    fn list(&mut self, path: &str) -> Result<Vec<String>, Fault> {
        assert!(self.dirs.contains_key(path), "list of {path}");
        Ok(self.children(path))
    }

    fn remove(&mut self, path: &str) -> Result<(), Fault> {
        self.spend()?;
        if self.files.remove(path).is_some() {
            return Ok(());
        }
        assert!(self.children(path).is_empty(), "rmdir of non-empty {path}");
        assert!(self.dirs.remove(path).is_some(), "remove of missing {path}");
        Ok(())
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<(), Fault> {
        self.spend()?;
        assert!(self.stat(to).unwrap().is_none(), "rename over {to}");
        let moved: Vec<String> = std::iter::once(from.to_string())
            .chain(self.below(from))
            .collect();
        for old in moved {
            let new = format!("{to}{}", &old[from.len()..]);
            if self.dirs.remove(&old).is_some() {
                self.dirs.insert(new, ());
            } else if let Some(file) = self.files.remove(&old) {
                self.files.insert(new, file);
            }
        }
        Ok(())
    }
}
