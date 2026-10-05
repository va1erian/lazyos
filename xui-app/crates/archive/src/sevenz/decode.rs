//! Decoding one 7z folder: a single Copy, LZMA, LZMA2 or Deflate coder.

use std::io::{self, BufReader, Read};
use std::thread;

use flate2::read::DeflateDecoder;

use crate::codec::{lzma_error, BoxRead};
use crate::error::{Error, Result};
use crate::pipe::{pipe, PipeWriter};

use super::header::Folder;

const COPY: &[u8] = &[0x00];
const LZMA: &[u8] = &[0x03, 0x01, 0x01];
const LZMA2: &[u8] = &[0x21];
const DEFLATE: &[u8] = &[0x04, 0x01, 0x08];
const AES: &[u8] = &[0x06, 0xf1, 0x07, 0x01];

/// The method column for a folder's files.
pub fn coder_name(folder: &Folder) -> String {
    let names: Vec<&str> = folder
        .coders
        .iter()
        .map(|coder| match coder.id.as_slice() {
            COPY => "Copy",
            LZMA => "LZMA",
            LZMA2 => "LZMA2",
            DEFLATE => "Deflate",
            AES => "AES",
            [0x03, 0x03, ..] => "BCJ",
            [0x04, 0x02, 0x02] => "BZip2",
            [0x03] => "Delta",
            _ => "?",
        })
        .collect();
    names.join("+")
}

/// Whether a folder is encrypted.
pub fn is_encrypted(folder: &Folder) -> bool {
    folder.coders.iter().any(|coder| coder.id == AES)
}

/// The unpacked stream of `folder` read from its packed bytes `packed`.
pub fn folder_reader(folder: &Folder, packed: BoxRead) -> Result<BoxRead> {
    if is_encrypted(folder) {
        return Err(Error::unsupported("encrypted 7z data"));
    }
    let [coder] = folder.coders.as_slice() else {
        return Err(Error::unsupported(format!(
            "7z {} coder chains",
            coder_name(folder)
        )));
    };
    if folder.packed_streams != 1 {
        return Err(Error::unsupported("7z coders with several inputs"));
    }
    let size = folder.unpack_size();
    match coder.id.as_slice() {
        COPY => Ok(packed),
        DEFLATE => Ok(Box::new(DeflateDecoder::new(BufReader::new(packed)))),
        LZMA => {
            if coder.props.len() != 5 {
                return Err(Error::corrupt("7z: bad LZMA properties"));
            }
            let props = coder.props.clone();
            Ok(spawn(move |writer| {
                // lzma-rs reads the 5 property bytes in front of the stream;
                // the size comes from the folder, not a header.
                let mut input = BufReader::new(io::Cursor::new(props).chain(packed));
                let options = lzma_rs::decompress::Options {
                    unpacked_size: lzma_rs::decompress::UnpackedSize::UseProvided(Some(size)),
                    memlimit: None,
                    allow_incomplete: false,
                };
                lzma_rs::lzma_decompress_with_options(&mut input, writer, &options)
                    .map_err(lzma_error)
            }))
        }
        LZMA2 => Ok(spawn(move |writer| {
            let mut input = BufReader::new(packed);
            lzma_rs::lzma2_decompress(&mut input, writer).map_err(lzma_error)
        })),
        _ => Err(Error::unsupported(format!(
            "7z {} data",
            coder_name(folder)
        ))),
    }
}

/// Run a push decoder on a helper thread and read its output.
fn spawn(run: impl FnOnce(&mut PipeWriter) -> io::Result<()> + Send + 'static) -> BoxRead {
    let (mut writer, reader) = pipe();
    let started = thread::Builder::new()
        .name("7z-decode".into())
        .spawn(move || match run(&mut writer) {
            Ok(()) => writer.finish(),
            Err(error) => writer.fail(error),
        });
    match started {
        Ok(_) => Box::new(reader),
        Err(error) => Box::new(crate::archive::Failing(Some(error))),
    }
}
