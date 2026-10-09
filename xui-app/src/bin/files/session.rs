//! Files' [`Session`]: copy and paste through `clipboardd`, and the
//! selection published on `session/<id>/selection`.
//!
//! Copy offers the selected paths as one eager `text/uri-list` offer (the
//! encoding drag and drop already uses, `xui_app::platform::urilist`); Paste
//! asks `clipboardd` for the newest `text/uri-list` offer in the caller's
//! session (token 0) and copies every path into the folder with the copy a
//! drop runs (`copy_into`: folders recursively, links as links, a taken name
//! gets `name (2)`). The selection goes out through the `midlc`-generated
//! `publish_session_selection` (`idl/files.midl`) on the central broker,
//! skipping a repeat of the last one.
//!
//! Serial: `FILES:COPY:PASS:<n>` or `FILES:COPY:FAIL:<errno>`,
//! `FILES:PASTE:PASS:<copied>:<failed>` (`FILES:PASTE:EMPTY` when the
//! clipboard holds no files, `FILES:PASTE:FAIL:<errno>` when it refused), and
//! `FILES:SELECTION:PASS:<n>` per published selection of `n` paths
//! (`FILES:SELECTION:FAIL:<reason>` when the broker refused), and
//! `FILES:DIR:<path>` whenever the folder a window announces differs from
//! the last one announced (a navigation, or another window's folder).

use std::cell::RefCell;
use std::io;
use std::path::{Path, PathBuf};

use messenger_generated::os_lazy_files_v1 as files_wire;
use xui_app::platform::topics::{Broker, PublishError};
use xui_app::platform::{clipboard, urilist};
use xui_explorer::platform::{Pasted, Session};
use xui_explorer::std_platform::{copy_into, CopyReport};

/// `-ENOENT`: no service, or no offer carrying the MIME type.
const ENOENT: i64 = -2;

/// The LazyOS session seam.
pub struct LazySession {
    /// This task's login session (kernel-stamped), the topic's `{session}`;
    /// `None` when unknown, and then no selection is published.
    session: Option<u64>,
    /// The last selection published, so an unchanged one is not sent again.
    last: RefCell<Option<(PathBuf, Vec<PathBuf>)>>,
    /// The folder of the last announcement, for `FILES:DIR`.
    dir: RefCell<Option<PathBuf>>,
}

impl LazySession {
    pub fn new() -> LazySession {
        LazySession {
            session: xui_app::sys::cred_get(None).ok().map(|cred| cred.session),
            last: RefCell::new(None),
            dir: RefCell::new(None),
        }
    }
}

impl Session for LazySession {
    fn copy(&self, paths: &[PathBuf]) -> io::Result<()> {
        let bytes = urilist::encode(paths).into_bytes();
        match clipboard::offer(urilist::MIME, &bytes) {
            Ok(_) => {
                println!("FILES:COPY:PASS:{}", paths.len());
                Ok(())
            }
            Err(code) => {
                println!("FILES:COPY:FAIL:{code}");
                Err(errno_error(code))
            }
        }
    }

    fn paste_into(&self, dir: &Path) -> io::Result<Pasted> {
        let bytes = match clipboard::paste(0, urilist::MIME) {
            Ok(bytes) => bytes,
            Err(ENOENT) => {
                println!("FILES:PASTE:EMPTY");
                return Ok(Pasted::default());
            }
            Err(code) => {
                println!("FILES:PASTE:FAIL:{code}");
                return Err(errno_error(code));
            }
        };
        let sources = urilist::decode(&bytes);
        if sources.is_empty() {
            println!("FILES:PASTE:EMPTY");
            return Ok(Pasted::default());
        }
        let CopyReport { copied, failed } = copy_into(&sources, dir);
        for (path, error) in &failed {
            println!("FILES:PASTE:SKIP:{}:{error}", path.display());
        }
        println!("FILES:PASTE:PASS:{copied}:{}", failed.len());
        Ok(Pasted { copied, failed })
    }

    fn selection_changed(&self, dir: &Path, paths: &[PathBuf]) {
        if self.dir.borrow().as_deref() != Some(dir) {
            println!("FILES:DIR:{}", dir.display());
            *self.dir.borrow_mut() = Some(dir.to_path_buf());
        }
        let Some(session) = self.session else {
            return;
        };
        let current = (dir.to_path_buf(), paths.to_vec());
        if self.last.borrow().as_ref() == Some(&current) {
            return;
        }
        let selection = files_wire::Selection {
            folder: dir.to_string_lossy().into_owned(),
            paths: paths
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect(),
        };
        let published = files_wire::publish_session_selection(
            &mut Broker::new(),
            &session.to_string(),
            &selection,
        );
        match published {
            Ok(_) => {
                println!("FILES:SELECTION:PASS:{}", paths.len());
                *self.last.borrow_mut() = Some(current);
            }
            // No broker (a console image): nothing to tell, nothing failed.
            Err(PublishError::Errno(ENOENT)) => {}
            Err(error) => println!("FILES:SELECTION:FAIL:{error:?}"),
        }
    }
}

/// A negative errno as an `io::Error` the status bar can show.
fn errno_error(code: i64) -> io::Error {
    let text = match code {
        -13 => "the clipboard refused it",
        -7 => "too many files for the clipboard",
        ENOENT => "the clipboard service is not running",
        _ => "the clipboard failed",
    };
    io::Error::other(format!("{text} ({code})"))
}
