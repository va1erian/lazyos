//! The formats, what each can do, and how a file's format is recognised.
//!
//! Detection trusts content over names: the magic bytes pick the container or
//! compression, and a compressed stream counts as a tarball only when its
//! first decompressed block is a valid tar header ([`crate::tar::is_header`]).
//! Names decide only what a *new* archive becomes ([`Format::for_name`]).

use crate::codec::Codec;

/// An archive format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Format {
    /// PKZIP (`.zip`), stored and deflate members, zip64.
    Zip,
    /// A tarball (`.tar`): ustar, pax and GNU long names.
    Tar,
    /// A gzip-compressed tarball (`.tar.gz`, `.tgz`).
    TarGz,
    /// An xz-compressed tarball (`.tar.xz`, `.txz`), read-only.
    TarXz,
    /// A Zstandard-compressed tarball (`.tar.zst`, `.tzst`).
    TarZst,
    /// One gzip-compressed file (`.gz`).
    Gz,
    /// One xz-compressed file (`.xz`), read-only.
    Xz,
    /// One Zstandard-compressed file (`.zst`).
    Zst,
    /// 7-Zip (`.7z`): LZMA, LZMA2 and copy coders, read-only.
    SevenZ,
}

/// How hard a writer compresses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Level {
    /// No compression (zip members are stored).
    Store,
    /// Fastest compression.
    Fast,
    /// The usual trade-off.
    #[default]
    Normal,
    /// Smallest output.
    Max,
}

impl Level {
    /// Every level, in the order a picker lists them.
    pub const ALL: [Level; 4] = [Level::Store, Level::Fast, Level::Normal, Level::Max];

    /// The picker's label.
    pub fn label(self) -> &'static str {
        match self {
            Level::Store => "Store",
            Level::Fast => "Fast",
            Level::Normal => "Normal",
            Level::Max => "Maximum",
        }
    }

    /// The deflate level (0–9).
    pub fn deflate(self) -> u32 {
        match self {
            Level::Store => 0,
            Level::Fast => 1,
            Level::Normal => 6,
            Level::Max => 9,
        }
    }
}

/// Every format, in the order a picker lists the writable ones first.
pub const ALL: [Format; 9] = [
    Format::Zip,
    Format::TarGz,
    Format::TarZst,
    Format::Tar,
    Format::Gz,
    Format::Zst,
    Format::TarXz,
    Format::Xz,
    Format::SevenZ,
];

impl Format {
    /// The display name.
    pub fn name(self) -> &'static str {
        match self {
            Format::Zip => "ZIP",
            Format::Tar => "TAR",
            Format::TarGz => "TAR.GZ",
            Format::TarXz => "TAR.XZ",
            Format::TarZst => "TAR.ZST",
            Format::Gz => "GZIP",
            Format::Xz => "XZ",
            Format::Zst => "ZSTD",
            Format::SevenZ => "7Z",
        }
    }

    /// The usual file name extensions, the preferred one first.
    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            Format::Zip => &["zip"],
            Format::Tar => &["tar"],
            Format::TarGz => &["tar.gz", "tgz"],
            Format::TarXz => &["tar.xz", "txz"],
            Format::TarZst => &["tar.zst", "tzst"],
            Format::Gz => &["gz"],
            Format::Xz => &["xz"],
            Format::Zst => &["zst"],
            Format::SevenZ => &["7z"],
        }
    }

    /// Whether this library can create and modify archives of this format.
    pub fn writable(self) -> bool {
        !matches!(self, Format::TarXz | Format::Xz | Format::SevenZ)
    }

    /// Whether the format holds exactly one file (no names, no folders).
    pub fn single_file(self) -> bool {
        matches!(self, Format::Gz | Format::Xz | Format::Zst)
    }

    /// Whether the format is a tarball, compressed or not.
    pub fn is_tar(self) -> bool {
        matches!(
            self,
            Format::Tar | Format::TarGz | Format::TarXz | Format::TarZst
        )
    }

    /// The stream compression around a tarball or a single file.
    pub fn codec(self) -> Codec {
        match self {
            Format::TarGz | Format::Gz => Codec::Gzip,
            Format::TarXz | Format::Xz => Codec::Xz,
            Format::TarZst | Format::Zst => Codec::Zstd,
            Format::Zip | Format::Tar | Format::SevenZ => Codec::None,
        }
    }

    /// The format a new archive named `name` should have, by its extension
    /// (the longest match wins, so `x.tar.gz` is a tarball, not a gzip file).
    pub fn for_name(name: &str) -> Option<Format> {
        let lower = name.to_ascii_lowercase();
        ALL.iter()
            .flat_map(|format| format.extensions().iter().map(move |ext| (*format, *ext)))
            .filter(|(_, ext)| {
                lower.len() > ext.len() + 1
                    && lower.ends_with(ext)
                    && lower.as_bytes()[lower.len() - ext.len() - 1] == b'.'
            })
            .max_by_key(|(_, ext)| ext.len())
            .map(|(format, _)| format)
    }

    /// `name` without this format's extension (`a.tar.gz` -> `a`), for the
    /// folder an archive extracts into and a single file's inner name.
    pub fn strip_extension(self, name: &str) -> String {
        let lower = name.to_ascii_lowercase();
        for ext in self.extensions() {
            let suffix = format!(".{ext}");
            if lower.ends_with(&suffix) && name.len() > suffix.len() {
                return name[..name.len() - suffix.len()].to_owned();
            }
        }
        // `x.tgz` read as gzip still loses its `.tgz`; anything else keeps
        // its name with a marker so it never collides with the archive.
        match name.rsplit_once('.') {
            Some((stem, _)) if !stem.is_empty() => stem.to_owned(),
            _ => format!("{name}.out"),
        }
    }
}

/// What the first bytes of a file say about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Magic {
    /// A zip local header, empty-archive EOCD or spanned marker.
    Zip,
    /// `1f 8b`.
    Gzip,
    /// `fd 37 7a 58 5a 00`.
    Xz,
    /// `28 b5 2f fd`.
    Zstd,
    /// `37 7a bc af 27 1c`.
    SevenZ,
    /// A valid tar header block.
    Tar,
}

/// Recognise `head` (at least the first 512 bytes, when the file has them).
pub fn sniff(head: &[u8]) -> Option<Magic> {
    if head.starts_with(b"PK\x03\x04")
        || head.starts_with(b"PK\x05\x06")
        || head.starts_with(b"PK\x07\x08")
    {
        Some(Magic::Zip)
    } else if head.starts_with(&[0x1f, 0x8b]) {
        Some(Magic::Gzip)
    } else if head.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0]) {
        Some(Magic::Xz)
    } else if head.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
        Some(Magic::Zstd)
    } else if head.starts_with(&[b'7', b'z', 0xbc, 0xaf, 0x27, 0x1c]) {
        Some(Magic::SevenZ)
    } else if head.len() >= 512 && crate::tar::is_header(&head[..512]) {
        Some(Magic::Tar)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_longest_extension_wins() {
        assert_eq!(Format::for_name("a.tar.gz"), Some(Format::TarGz));
        assert_eq!(Format::for_name("a.GZ"), Some(Format::Gz));
        assert_eq!(Format::for_name("a.tgz"), Some(Format::TarGz));
        assert_eq!(Format::for_name("x.zip"), Some(Format::Zip));
        assert_eq!(Format::for_name("x.7z"), Some(Format::SevenZ));
        assert_eq!(Format::for_name("zip"), None);
        assert_eq!(Format::for_name("notes.txt"), None);
    }

    #[test]
    fn stripping_drops_the_whole_extension() {
        assert_eq!(Format::TarGz.strip_extension("pkg.tar.gz"), "pkg");
        assert_eq!(Format::TarGz.strip_extension("pkg.tgz"), "pkg");
        assert_eq!(Format::Gz.strip_extension("notes.txt.gz"), "notes.txt");
        assert_eq!(Format::Zip.strip_extension("README"), "README.out");
    }

    #[test]
    fn magic_bytes_are_recognised() {
        assert_eq!(sniff(b"PK\x03\x04rest"), Some(Magic::Zip));
        assert_eq!(sniff(&[0x1f, 0x8b, 8]), Some(Magic::Gzip));
        assert_eq!(sniff(&[0x28, 0xb5, 0x2f, 0xfd]), Some(Magic::Zstd));
        assert_eq!(sniff(b"hello"), None);
    }

    #[test]
    fn read_only_formats_are_not_writable() {
        assert!(Format::Zip.writable() && Format::TarZst.writable());
        assert!(!Format::SevenZ.writable() && !Format::TarXz.writable());
    }
}
