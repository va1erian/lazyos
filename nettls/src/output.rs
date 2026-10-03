//! Where the body goes, and the file names wget and `curl -O` derive from
//! the URL.
//!
//! A derived name is the URL path's last segment, still percent-encoded, so
//! a server-chosen `%2F` can never become a `/` and no name can climb out
//! of the current directory. `.`, `..` and empty segments become
//! `index.html` (wget) or an error (curl, which has no default name).

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use url::Url;

use crate::opts::Output;
use crate::report::Failure;

/// wget's name for `url`: the last path segment, or `index.html`.
pub fn wget_name(url: &Url) -> String {
    last_segment(url).unwrap_or_else(|| "index.html".to_string())
}

/// curl `-O`'s name for `url`; curl refuses a URL that ends in `/`.
pub fn remote_name(url: &Url) -> Result<String, Failure> {
    last_segment(url).ok_or_else(|| {
        Failure::Write("remote file name has no length (the URL ends in '/'; use -o FILE)".into())
    })
}

fn last_segment(url: &Url) -> Option<String> {
    let segment = url.path_segments()?.next_back()?;
    match segment {
        "" | "." | ".." => None,
        // `url` percent-encodes controls and spaces; this also refuses
        // anything else that is not printable ASCII.
        s if s
            .bytes()
            .all(|b| (0x21..=0x7e).contains(&b) && b != b'/' && b != b'\\') =>
        {
            Some(s.to_string())
        }
        _ => None,
    }
}

/// wget's rule for an existing file: `name`, else `name.1`, `name.2`, ...
/// in `dir`. Returns the first name that does not exist.
pub fn unclobbered(dir: &Path, name: &str) -> Result<PathBuf, Failure> {
    let first = dir.join(name);
    if !exists(&first) {
        return Ok(first);
    }
    for n in 1..=9999 {
        let candidate = dir.join(format!("{name}.{n}"));
        if !exists(&candidate) {
            return Ok(candidate);
        }
    }
    Err(Failure::Write(format!("{name}: too many existing copies")))
}

fn exists(path: &Path) -> bool {
    // A dangling symlink counts as taken: never write through it.
    path.symlink_metadata().is_ok()
}

/// The destination for the body, opened only once the response is known to
/// be wanted (so a failed request leaves no empty file behind).
pub enum Sink {
    Stdout(io::Stdout),
    File(File, PathBuf),
}

impl Sink {
    pub fn writer(&mut self) -> &mut dyn Write {
        match self {
            Sink::Stdout(out) => out,
            Sink::File(file, _) => file,
        }
    }

    /// The file written to, if any.
    pub fn path(&self) -> Option<&Path> {
        match self {
            Sink::Stdout(_) => None,
            Sink::File(_, path) => Some(path),
        }
    }
}

/// Open the destination `output` names for the original URL `url`.
pub fn open(output: &Output, url: &Url) -> Result<Sink, Failure> {
    let write_err = |path: &Path, e: io::Error| Failure::Write(format!("{}: {e}", path.display()));
    match output {
        Output::Stdout => Ok(Sink::Stdout(io::stdout())),
        Output::File(path) => {
            let file = File::create(path).map_err(|e| write_err(path, e))?;
            Ok(Sink::File(file, path.clone()))
        }
        Output::RemoteName => {
            let path = PathBuf::from(remote_name(url)?);
            let file = File::create(&path).map_err(|e| write_err(&path, e))?;
            Ok(Sink::File(file, path))
        }
        Output::WgetName => {
            let path = unclobbered(Path::new(""), &wget_name(url))?;
            // `create_new` closes the gap between the check and the open.
            let file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|e| write_err(&path, e))?;
            Ok(Sink::File(file, path))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn wget_names() {
        assert_eq!(wget_name(&url("https://h/")), "index.html");
        assert_eq!(wget_name(&url("https://h")), "index.html");
        assert_eq!(wget_name(&url("https://h/dir/")), "index.html");
        assert_eq!(
            wget_name(&url("https://h/a/file.tar.gz?x=1#f")),
            "file.tar.gz"
        );
        assert_eq!(wget_name(&url("https://h/a%2F..%2Fb")), "a%2F..%2Fb");
        assert_eq!(wget_name(&url("https://h/sp ace")), "sp%20ace");
        assert_eq!(wget_name(&url("https://h/a/..")), "index.html");
    }

    #[test]
    fn curl_remote_names() {
        assert_eq!(remote_name(&url("https://h/x.bin")).unwrap(), "x.bin");
        assert!(remote_name(&url("https://h/")).is_err());
    }

    #[test]
    fn existing_files_are_not_clobbered() {
        let dir = std::env::temp_dir().join(format!("nettls-out-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(unclobbered(&dir, "f").unwrap(), dir.join("f"));
        std::fs::write(dir.join("f"), b"x").unwrap();
        assert_eq!(unclobbered(&dir, "f").unwrap(), dir.join("f.1"));
        std::fs::write(dir.join("f.1"), b"x").unwrap();
        assert_eq!(unclobbered(&dir, "f").unwrap(), dir.join("f.2"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
