//! The 7z files-info block: names, empty streams and files, times and
//! attributes, one record per archive member.

use crate::error::{Error, Result};

use super::header::{short, Cursor, K_END};

const K_EMPTY_STREAM: u64 = 0x0e;
const K_EMPTY_FILE: u64 = 0x0f;
const K_NAME: u64 = 0x11;
const K_MTIME: u64 = 0x14;
const K_WIN_ATTRIBUTES: u64 = 0x15;

/// One file record.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileRecord {
    pub name: String,
    pub has_stream: bool,
    pub is_dir: bool,
    pub mtime: Option<u64>,
    pub attributes: Option<u32>,
}

pub fn files_info(c: &mut Cursor<'_>) -> Result<Vec<FileRecord>> {
    // A file record costs at least a two-byte name terminator.
    let count = c.count(16)?;
    let mut files = vec![
        FileRecord {
            has_stream: true,
            ..FileRecord::default()
        };
        count
    ];
    let mut empty_stream: Vec<usize> = Vec::new();
    loop {
        let property = c.number()?;
        if property == K_END {
            break;
        }
        let size = usize::try_from(c.number()?).map_err(|_| short())?;
        let body = c.take(size)?;
        let mut p = Cursor::new(body);
        match property {
            K_EMPTY_STREAM => {
                let bits = p.bits(count)?;
                empty_stream = bits
                    .iter()
                    .enumerate()
                    .filter(|(_, &b)| b)
                    .map(|(i, _)| i)
                    .collect();
                for &i in &empty_stream {
                    files[i].has_stream = false;
                    files[i].is_dir = true;
                }
            }
            K_EMPTY_FILE => {
                let bits = p.bits(empty_stream.len())?;
                for (&i, empty_file) in empty_stream.iter().zip(bits) {
                    files[i].is_dir = !empty_file;
                }
            }
            K_NAME => {
                if p.byte()? != 0 {
                    return Err(Error::unsupported("7z: external names"));
                }
                let mut units = p
                    .take(p.left())?
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|pair| u16::from_le_bytes(*pair));
                for file in files.iter_mut() {
                    let name: Vec<u16> = units.by_ref().take_while(|&unit| unit != 0).collect();
                    file.name = String::from_utf16_lossy(&name);
                }
            }
            K_MTIME => {
                let defined = p.defined(count)?;
                if p.byte()? != 0 {
                    return Err(Error::unsupported("7z: external times"));
                }
                for (file, defined) in files.iter_mut().zip(defined) {
                    if defined {
                        file.mtime = Some(p.u64()?);
                    }
                }
            }
            K_WIN_ATTRIBUTES => {
                let defined = p.defined(count)?;
                if p.byte()? != 0 {
                    return Err(Error::unsupported("7z: external attributes"));
                }
                for (file, defined) in files.iter_mut().zip(defined) {
                    if defined {
                        let attributes = p.u32()?;
                        file.attributes = Some(attributes);
                        if attributes & 0x10 != 0 {
                            file.is_dir = true;
                        }
                    }
                }
            }
            // Anti items, other times, comments, padding: not needed.
            _ => {}
        }
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_huge_count_fails_without_allocating() {
        // 2^56 files in a header of a dozen bytes.
        let mut bytes = vec![0xfe, 0, 0, 0, 0, 0, 0, 1];
        bytes.extend_from_slice(&[0; 4]);
        assert!(files_info(&mut Cursor::new(&bytes)).is_err());
    }
}
