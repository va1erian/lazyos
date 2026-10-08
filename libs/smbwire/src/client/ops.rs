//! Tree and file operations on a logged-on [`Client`]: what `smb` and (in
//! F3) `smbfuse` call.
//!
//! Paths are slash-separated and relative to the share root
//! (`crate::name::to_smb` checks them). Every handle an operation opens is
//! closed before it returns, on failure too.

use alloc::vec::Vec;

use super::{Client, Transport};
use crate::header::command as cmd;
use crate::msg::{
    self, access, disposition, CreateRequest, CreateResponse, DirEntry, FileId, FileInfo, FsSize,
};
use crate::status::{END_OF_FILE, NO_MORE_FILES};
use crate::{name, Error};

/// Entries one listing may return before it is cut off.
pub const MAX_LISTING: usize = 100_000;

/// A connected share.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Share {
    pub tree_id: u32,
    pub flags: u32,
    pub maximal_access: u32,
}

/// How [`Client::open`] opens a path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Open {
    /// An existing file or directory, attributes only.
    Stat,
    /// An existing file, for reading.
    Read,
    /// A file for writing, created or truncated.
    Replace,
    /// An existing directory, for listing.
    Directory,
    /// A new directory.
    MakeDirectory,
    /// An existing file or directory, to delete or rename it.
    Delete,
}

impl Open {
    fn request(self, path: &str) -> Result<CreateRequest, Error> {
        use disposition::*;
        let (access, disposition, options) = match self {
            Open::Stat => (access::READ_ATTRIBUTES, OPEN, 0),
            Open::Read => (
                access::READ_DATA | access::READ_ATTRIBUTES,
                OPEN,
                msg::OPTION_NON_DIRECTORY_FILE,
            ),
            Open::Replace => (
                access::READ_DATA
                    | access::WRITE_DATA
                    | access::APPEND_DATA
                    | access::READ_ATTRIBUTES,
                OVERWRITE_IF,
                msg::OPTION_NON_DIRECTORY_FILE,
            ),
            Open::Directory => (
                access::LIST_DIRECTORY | access::READ_ATTRIBUTES,
                OPEN,
                msg::OPTION_DIRECTORY_FILE,
            ),
            Open::MakeDirectory => (access::READ_ATTRIBUTES, CREATE, msg::OPTION_DIRECTORY_FILE),
            Open::Delete => (access::DELETE | access::READ_ATTRIBUTES, OPEN, 0),
        };
        Ok(CreateRequest {
            access: access | access::SYNCHRONIZE,
            attributes: if self == Open::MakeDirectory {
                msg::ATTR_DIRECTORY
            } else {
                0
            },
            share: msg::SHARE_READ | msg::SHARE_WRITE | msg::SHARE_DELETE,
            disposition,
            options,
            name: name::to_smb(path)?,
        })
    }
}

impl<T: Transport> Client<T> {
    /// Connect `\\server\share`; later file operations use it. Only a disk
    /// share is accepted, and not one that requires encryption.
    pub fn tree_connect(&mut self, server: &str, share: &str) -> Result<Share, Error> {
        let body = msg::tree_connect_request(&name::unc(server, share)?)?;
        let (header, message) = self.exchange(cmd::TREE_CONNECT, body)?;
        if header.status != crate::status::SUCCESS {
            return Err(Error::Status {
                command: cmd::TREE_CONNECT,
                status: header.status,
            });
        }
        let r = msg::parse_tree_connect(&message)?;
        if r.share_flags & msg::SHAREFLAG_ENCRYPT_DATA != 0 {
            return Err(Error::Refused("the share requires encryption (SMB3)"));
        }
        if r.share_type != msg::SHARE_TYPE_DISK {
            return Err(Error::Refused("not a disk share"));
        }
        self.tree_id = header.tree_id;
        Ok(Share {
            tree_id: header.tree_id,
            flags: r.share_flags,
            maximal_access: r.maximal_access,
        })
    }

    /// Open `path`.
    pub fn open(&mut self, path: &str, how: Open) -> Result<CreateResponse, Error> {
        let body = msg::create_request(&how.request(path)?)?;
        let message = self.call(cmd::CREATE, body, &[])?;
        msg::parse_create(&message)
    }

    pub fn close(&mut self, file: &FileId) -> Result<(), Error> {
        self.call(cmd::CLOSE, msg::close_request(file), &[])
            .map(drop)
    }

    /// Run `body` on `path` opened `how`, closing the handle afterwards
    /// whatever `body` returned.
    fn with<R>(
        &mut self,
        path: &str,
        how: Open,
        body: impl FnOnce(&mut Self, &CreateResponse) -> Result<R, Error>,
    ) -> Result<R, Error> {
        let opened = self.open(path, how)?;
        let result = body(self, &opened);
        let closed = self.close(&opened.file_id);
        let value = result?;
        closed?;
        Ok(value)
    }

    /// Up to `len` bytes at `offset` (capped at [`Client::max_read`]); empty
    /// at the end of the file.
    pub fn read(&mut self, file: &FileId, offset: u64, len: u32) -> Result<Vec<u8>, Error> {
        let len = len.min(self.max_read);
        let (header, message) = self.exchange(cmd::READ, msg::read_request(file, offset, len))?;
        match header.status {
            crate::status::SUCCESS => Ok(msg::parse_read(&message, len)?.to_vec()),
            END_OF_FILE => Ok(Vec::new()),
            status => Err(Error::Status {
                command: cmd::READ,
                status,
            }),
        }
    }

    /// Write `data` (at most [`Client::max_write`] bytes) at `offset`; the
    /// count the server wrote.
    pub fn write(&mut self, file: &FileId, offset: u64, data: &[u8]) -> Result<u32, Error> {
        if data.len() > self.max_write as usize {
            return Err(Error::Malformed("WRITE larger than the server accepts"));
        }
        let message = self.call(cmd::WRITE, msg::write_request(file, offset, data)?, &[])?;
        let count = msg::parse_write(&message)?;
        if count as usize > data.len() {
            return Err(Error::Malformed("WRITE count larger than the data"));
        }
        Ok(count)
    }

    pub fn flush(&mut self, file: &FileId) -> Result<(), Error> {
        self.call(cmd::FLUSH, msg::flush_request(file), &[])
            .map(drop)
    }

    /// The entries of the directory `path`, without `.` and `..`.
    pub fn list(&mut self, path: &str) -> Result<Vec<DirEntry>, Error> {
        let output = self.max_transact;
        self.with(path, Open::Directory, |c, dir| {
            let mut entries = Vec::new();
            let mut seen = 0usize;
            let pattern = crate::crypto::utf16le("*");
            let mut flags = msg::QUERY_RESTART_SCANS;
            loop {
                let body = msg::query_directory_request(
                    &dir.file_id,
                    msg::CLASS_ID_BOTH_DIRECTORY,
                    flags,
                    &pattern,
                    output,
                )?;
                flags = 0;
                let (header, message) = c.exchange(cmd::QUERY_DIRECTORY, body)?;
                match header.status {
                    NO_MORE_FILES => return Ok(entries),
                    crate::status::SUCCESS => {}
                    status => {
                        return Err(Error::Status {
                            command: cmd::QUERY_DIRECTORY,
                            status,
                        })
                    }
                }
                let buffer = msg::parse_output(&message, output)?;
                if buffer.is_empty() {
                    return Ok(entries);
                }
                let (found, skipped) = msg::parse_directory(buffer)?;
                // Skipped entries count too: a server answering every query
                // with only `.`, `..` or unlistable names must still end.
                seen += found.len() + skipped;
                entries.extend(found);
                if seen > MAX_LISTING {
                    return Err(Error::Malformed("directory listing too long"));
                }
            }
        })
    }

    /// The attributes of `path`.
    pub fn stat(&mut self, path: &str) -> Result<FileInfo, Error> {
        self.with(path, Open::Stat, |_, opened| Ok(opened.info))
    }

    /// Create the directory `path`.
    pub fn mkdir(&mut self, path: &str) -> Result<(), Error> {
        self.with(path, Open::MakeDirectory, |_, _| Ok(()))
    }

    /// Delete the file or (empty) directory `path`.
    pub fn delete(&mut self, path: &str) -> Result<(), Error> {
        self.with(path, Open::Delete, |c, opened| {
            let body = msg::set_info_request(
                &opened.file_id,
                msg::INFO_FILE,
                msg::CLASS_DISPOSITION,
                &msg::disposition_info(),
            )?;
            c.call(cmd::SET_INFO, body, &[]).map(drop)
        })
    }

    /// Rename `from` to `to` (both from the share root), replacing an
    /// existing `to` only when `replace` says so.
    pub fn rename(&mut self, from: &str, to: &str, replace: bool) -> Result<(), Error> {
        let target = name::to_smb(to)?;
        if target.is_empty() {
            return Err(Error::BadName);
        }
        self.with(from, Open::Delete, |c, opened| {
            let info = msg::rename_info(replace, &target);
            let body =
                msg::set_info_request(&opened.file_id, msg::INFO_FILE, msg::CLASS_RENAME, &info)?;
            c.call(cmd::SET_INFO, body, &[]).map(drop)
        })
    }

    /// Set the size of an open file.
    pub fn set_size(&mut self, file: &FileId, size: u64) -> Result<(), Error> {
        let body = msg::set_info_request(
            file,
            msg::INFO_FILE,
            msg::CLASS_END_OF_FILE,
            &msg::end_of_file_info(size),
        )?;
        self.call(cmd::SET_INFO, body, &[]).map(drop)
    }

    /// The size and free space of the share.
    pub fn statfs(&mut self) -> Result<FsSize, Error> {
        self.with("", Open::Stat, |c, root| {
            let body = msg::query_info_request(
                &root.file_id,
                msg::INFO_FILESYSTEM,
                msg::CLASS_FS_FULL_SIZE,
                64,
            );
            let message = c.call(cmd::QUERY_INFO, body, &[])?;
            msg::parse_fs_full_size(msg::parse_output(&message, 64)?)
        })
    }

    /// Disconnect the tree and log off; best effort, for a clean exit.
    pub fn logoff(&mut self) {
        if self.tree_id != 0 {
            let _ = self.exchange(cmd::TREE_DISCONNECT, msg::empty_request().to_vec());
            self.tree_id = 0;
        }
        if self.session_id != 0 {
            let _ = self.exchange(cmd::LOGOFF, msg::empty_request().to_vec());
        }
    }
}
