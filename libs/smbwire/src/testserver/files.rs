//! The fake server's file commands over its in-memory tree.

use std::string::String;
use std::vec;
use std::vec::Vec;

use super::Server;
use crate::crypto::utf16le;
use crate::header::Header;
use crate::msg::{self, disposition};
use crate::ntlm::from_utf16le;
use crate::status::*;
use crate::{le16, le32, le64};

fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(p, _)| p)
}

impl Server {
    fn info(&self, path: &str) -> Option<msg::FileInfo> {
        if self.dirs.contains(path) {
            return Some(msg::FileInfo {
                attributes: msg::ATTR_DIRECTORY,
                last_write: 0x01D9_0000_0000_0000,
                ..Default::default()
            });
        }
        self.files.get(path).map(|data| msg::FileInfo {
            end_of_file: data.len() as u64,
            allocation: data.len() as u64,
            attributes: msg::ATTR_ARCHIVE,
            last_write: 0x01D9_0000_0000_0000,
            ..Default::default()
        })
    }

    fn handle_path(&self, body: &[u8]) -> Option<(u64, String)> {
        let id = le64(body, 0)?;
        self.handles.get(&id).map(|(path, _)| (id, path.clone()))
    }

    pub(super) fn create(&mut self, h: &Header, message: &[u8], body: &[u8]) {
        let offset = le16(body, 44).unwrap() as usize;
        let len = le16(body, 46).unwrap() as usize;
        let name = if len == 0 {
            String::new()
        } else {
            from_utf16le(&message[offset..offset + len]).unwrap()
        };
        assert!(!name.contains('/'), "the client sends backslashes");
        let path = name.replace('\\', "/");
        let disp = le32(body, 36).unwrap();
        let options = le32(body, 40).unwrap();
        let exists = self.info(&path).is_some();
        if !self.dirs.contains(parent(&path)) {
            return self.reply(h, OBJECT_PATH_NOT_FOUND, &[]);
        }
        match disp {
            disposition::OPEN if !exists => return self.reply(h, OBJECT_NAME_NOT_FOUND, &[]),
            disposition::CREATE if exists => return self.reply(h, OBJECT_NAME_COLLISION, &[]),
            disposition::CREATE if options & msg::OPTION_DIRECTORY_FILE != 0 => {
                self.dirs.insert(path.clone());
            }
            disposition::CREATE => {
                self.files.insert(path.clone(), Vec::new());
            }
            disposition::OVERWRITE_IF => {
                if self.dirs.contains(&path) {
                    return self.reply(h, FILE_IS_A_DIRECTORY, &[]);
                }
                self.files.insert(path.clone(), Vec::new());
            }
            _ => {}
        }
        let info = self.info(&path).unwrap();
        if options & msg::OPTION_DIRECTORY_FILE != 0 && !info.is_dir() {
            return self.reply(h, NOT_A_DIRECTORY, &[]);
        }
        if options & msg::OPTION_NON_DIRECTORY_FILE != 0 && info.is_dir() {
            return self.reply(h, FILE_IS_A_DIRECTORY, &[]);
        }
        let id = self.next_handle;
        self.next_handle += 1;
        self.handles.insert(id, (path, false));
        let mut out = vec![0u8; 88];
        out[0] = 89;
        out[4] = 1;
        out[8 + 16..8 + 24].copy_from_slice(&info.last_write.to_le_bytes());
        out[48..56].copy_from_slice(&info.end_of_file.to_le_bytes());
        out[56..60].copy_from_slice(&info.attributes.to_le_bytes());
        out[64..72].copy_from_slice(&id.to_le_bytes());
        self.reply(h, SUCCESS, &out);
    }

    pub(super) fn read(&mut self, h: &Header, body: &[u8]) {
        let len = le32(body, 4).unwrap() as usize;
        let offset = le64(body, 8).unwrap() as usize;
        let (_, path) = self.handle_path(&body[16..]).unwrap();
        let data = &self.files[&path];
        if offset >= data.len() {
            return self.reply(h, END_OF_FILE, &[]);
        }
        let chunk = data[offset..(offset + len).min(data.len())].to_vec();
        let mut out = vec![17u8, 0, 80, 0];
        out.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
        out.extend_from_slice(&[0; 8]);
        out.extend_from_slice(&chunk);
        self.reply(h, SUCCESS, &out);
    }

    pub(super) fn write(&mut self, h: &Header, message: &[u8], body: &[u8]) {
        let at = le16(body, 2).unwrap() as usize;
        let len = le32(body, 4).unwrap() as usize;
        let offset = le64(body, 8).unwrap() as usize;
        let (_, path) = self.handle_path(&body[16..]).unwrap();
        let data = self.files.get_mut(&path).unwrap();
        if data.len() < offset + len {
            data.resize(offset + len, 0);
        }
        data[offset..offset + len].copy_from_slice(&message[at..at + len]);
        let mut out = vec![17u8, 0, 0, 0];
        out.extend_from_slice(&(len as u32).to_le_bytes());
        out.extend_from_slice(&[0; 9]);
        self.reply(h, SUCCESS, &out);
    }

    pub(super) fn query_directory(&mut self, h: &Header, body: &[u8]) {
        let restart = body[3] & msg::QUERY_RESTART_SCANS != 0;
        let (_, dir) = self.handle_path(&body[8..]).unwrap();
        if !restart && !self.how.endless_listing {
            return self.reply(h, NO_MORE_FILES, &[]);
        }
        let prefix = if dir.is_empty() {
            String::new()
        } else {
            format!("{dir}/")
        };
        let mut names: Vec<String> = vec![String::from("."), String::from("..")];
        for path in self.files.keys().chain(self.dirs.iter()) {
            if let Some(rest) = path.strip_prefix(&prefix) {
                if !rest.is_empty() && !rest.contains('/') && path != &dir {
                    names.push(String::from(rest));
                }
            }
        }
        if self.how.endless_listing {
            names.truncate(2);
        }
        let mut buffer = Vec::new();
        for (index, name) in names.iter().enumerate() {
            let path = if name.starts_with('.') {
                dir.clone()
            } else {
                format!("{prefix}{name}")
            };
            let info = self.info(&path).unwrap();
            let encoded = utf16le(name);
            let mut entry = vec![0u8; 104];
            entry[40..48].copy_from_slice(&info.end_of_file.to_le_bytes());
            entry[56..60].copy_from_slice(&info.attributes.to_le_bytes());
            entry[60..64].copy_from_slice(&(encoded.len() as u32).to_le_bytes());
            entry.extend_from_slice(&encoded);
            while !entry.len().is_multiple_of(8) {
                entry.push(0);
            }
            if index + 1 < names.len() {
                let next = entry.len() as u32;
                entry[0..4].copy_from_slice(&next.to_le_bytes());
            }
            buffer.extend_from_slice(&entry);
        }
        let mut out = vec![9u8, 0, 72, 0];
        out.extend_from_slice(&(buffer.len() as u32).to_le_bytes());
        out.extend_from_slice(&buffer);
        self.reply(h, SUCCESS, &out);
    }

    pub(super) fn query_info(&mut self, h: &Header, body: &[u8]) {
        assert_eq!(
            (body[2], body[3]),
            (msg::INFO_FILESYSTEM, msg::CLASS_FS_FULL_SIZE)
        );
        let mut info = Vec::new();
        info.extend_from_slice(&1000u64.to_le_bytes());
        info.extend_from_slice(&250u64.to_le_bytes());
        info.extend_from_slice(&250u64.to_le_bytes());
        info.extend_from_slice(&8u32.to_le_bytes());
        info.extend_from_slice(&512u32.to_le_bytes());
        let mut out = vec![9u8, 0, 72, 0];
        out.extend_from_slice(&(info.len() as u32).to_le_bytes());
        out.extend_from_slice(&info);
        self.reply(h, SUCCESS, &out);
    }

    pub(super) fn set_info(&mut self, h: &Header, message: &[u8], body: &[u8]) {
        let class = body[3];
        let len = le32(body, 4).unwrap() as usize;
        let at = le16(body, 8).unwrap() as usize;
        let data = &message[at..at + len];
        let (id, path) = self.handle_path(&body[16..]).unwrap();
        match class {
            msg::CLASS_DISPOSITION => {
                let prefix = format!("{path}/");
                if self.dirs.contains(&path)
                    && self
                        .files
                        .keys()
                        .chain(self.dirs.iter())
                        .any(|p| p.starts_with(&prefix))
                {
                    return self.reply(h, DIRECTORY_NOT_EMPTY, &[]);
                }
                self.handles.get_mut(&id).unwrap().1 = true;
            }
            msg::CLASS_RENAME => {
                let replace = data[0] != 0;
                let name_len = le32(data, 16).unwrap() as usize;
                let to = from_utf16le(&data[20..20 + name_len])
                    .unwrap()
                    .replace('\\', "/");
                if self.info(&to).is_some() && !replace {
                    return self.reply(h, OBJECT_NAME_COLLISION, &[]);
                }
                if let Some(bytes) = self.files.remove(&path) {
                    self.files.insert(to.clone(), bytes);
                } else {
                    // A directory takes everything below it along.
                    let below = format!("{path}/");
                    let moved = |p: &String| format!("{to}{}", &p[path.len()..]);
                    let files: Vec<String> = self
                        .files
                        .keys()
                        .filter(|p| p.starts_with(&below))
                        .cloned()
                        .collect();
                    for old in files {
                        let bytes = self.files.remove(&old).unwrap();
                        self.files.insert(moved(&old), bytes);
                    }
                    let dirs: Vec<String> = self
                        .dirs
                        .iter()
                        .filter(|p| **p == path || p.starts_with(&below))
                        .cloned()
                        .collect();
                    for old in dirs {
                        self.dirs.remove(&old);
                        self.dirs.insert(moved(&old));
                    }
                }
                self.handles.get_mut(&id).unwrap().0 = to;
            }
            msg::CLASS_END_OF_FILE => {
                let size = le64(data, 0).unwrap() as usize;
                self.files.get_mut(&path).unwrap().resize(size, 0);
            }
            _ => return self.reply(h, NOT_SUPPORTED, &[]),
        }
        self.reply(h, SUCCESS, &[2, 0]);
    }
}
