//! A parsed module as a script value (`Song`), and where songs come from.
//!
//! A song is loaded from a file the script may read (the player's file
//! sandbox decides, exactly as for `file_read_text`), or decoded from base64
//! text: a project carries its own songs in a script module, because a
//! packaged LazyRAD app ships only its forms and scripts.

use std::rc::Rc;

use lazyrad_runtime::{Access, FsPolicy};
use modplay::{ModError, Module, Note, CHANNELS, ROWS_PER_PATTERN};
use rhai::{Array, Dynamic, Map};

/// A parsed, validated module, shared by every deck that plays it.
#[derive(Clone)]
pub struct Song(pub Rc<Module>);

impl Song {
    pub fn parse(bytes: &[u8]) -> Result<Song, String> {
        Module::parse(bytes)
            .map(|module| Song(Rc::new(module)))
            .map_err(|error| format!("not a ProTracker module: {}", describe(error)))
    }

    pub fn title(&self) -> String {
        text(&self.0.title)
    }

    /// The pattern played at `order`, or `-1` past the end.
    pub fn pattern_at(&self, order: i64) -> i64 {
        usize::try_from(order)
            .ok()
            .and_then(|order| self.0.orders.get(order))
            .map_or(-1, |&p| i64::from(p))
    }

    /// The four cells of `row` in `order`, as maps; empty cells past the end.
    pub fn row(&self, order: i64, row: i64) -> Array {
        (0..CHANNELS)
            .map(|channel| Dynamic::from_map(self.cell(order, row, channel as i64)))
            .collect()
    }

    /// One cell: `#{ period, sample, effect, param }`.
    pub fn cell(&self, order: i64, row: i64, channel: i64) -> Map {
        let index = |value: i64, limit: usize| usize::try_from(value).ok().filter(|v| *v < limit);
        let note = match (
            index(order, self.0.orders.len()),
            index(row, ROWS_PER_PATTERN),
            index(channel, CHANNELS),
        ) {
            (Some(order), Some(row), Some(channel)) => self.0.note(order, row, channel),
            _ => Note::default(),
        };
        let mut map = Map::new();
        map.insert("period".into(), i64::from(note.period).into());
        map.insert("sample".into(), i64::from(note.sample).into());
        map.insert("effect".into(), i64::from(note.effect).into());
        map.insert("param".into(), i64::from(note.param).into());
        map
    }

    /// Every sample slot: `#{ number, name, length, volume, finetune, looped }`.
    pub fn samples(&self) -> Array {
        self.0
            .samples
            .iter()
            .enumerate()
            .map(|(index, sample)| {
                let mut map = Map::new();
                map.insert("number".into(), (index as i64 + 1).into());
                map.insert("name".into(), text(&sample.name).into());
                map.insert("length".into(), (sample.data.len() as i64).into());
                map.insert("volume".into(), i64::from(sample.volume).into());
                map.insert("finetune".into(), i64::from(sample.finetune).into());
                map.insert("looped".into(), sample.loop_range.is_some().into());
                Dynamic::from_map(map)
            })
            .collect()
    }
}

/// A NUL-padded name as text: bytes up to the first NUL, non-ASCII and
/// control bytes shown as `?` (names are from the file, so untrusted), and
/// trailing spaces trimmed.
pub fn text(bytes: &[u8]) -> String {
    bytes
        .iter()
        .take_while(|&&b| b != 0)
        .map(|&b| {
            if (0x20..0x7F).contains(&b) {
                char::from(b)
            } else {
                '?'
            }
        })
        .collect::<String>()
        .trim_end()
        .to_owned()
}

fn describe(error: ModError) -> &'static str {
    match error {
        ModError::TooShort => "the file is too short",
        ModError::BadSignature => "no M.K./4CHN signature",
        ModError::UnsupportedChannels => "only four-channel modules are supported",
        ModError::BadSongLength => "the song length is invalid",
        ModError::BadOrder => "the order list names an impossible pattern",
        ModError::TruncatedPatterns => "the pattern data is truncated",
    }
}

/// Read and parse `path` under the script's file policy.
pub fn load(policy: &FsPolicy, path: &str) -> Result<Song, String> {
    let real = policy
        .resolve(path, Access::Read)
        .map_err(|error| error.to_string())?;
    let bytes = std::fs::read(&real).map_err(|error| format!("`{path}`: {error}"))?;
    Song::parse(&bytes).map_err(|error| format!("`{path}`: {error}"))
}

/// Decode standard base64 (`A-Z a-z 0-9 + /`, `=` padding); whitespace is
/// ignored so a long string can be split over lines.
pub fn decode_base64(text: &str) -> Result<Vec<u8>, String> {
    fn value(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some(u32::from(c - b'A')),
            b'a'..=b'z' => Some(u32::from(c - b'a') + 26),
            b'0'..=b'9' => Some(u32::from(c - b'0') + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let digits: Vec<u8> = text.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    let body = digits
        .strip_suffix(b"==")
        .or_else(|| digits.strip_suffix(b"="))
        .unwrap_or(&digits);
    if !digits.len().is_multiple_of(4) {
        return Err("base64: the length is not a multiple of 4".to_owned());
    }
    let mut out = Vec::with_capacity(body.len() * 3 / 4);
    for chunk in body.chunks(4) {
        let mut word = 0u32;
        for &c in chunk {
            let v = value(c).ok_or_else(|| format!("base64: unexpected `{}`", char::from(c)))?;
            word = word << 6 | v;
        }
        word <<= 6 * (4 - chunk.len() as u32);
        let bytes = word.to_be_bytes();
        out.extend_from_slice(&bytes[1..chunk.len()]);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracker::tests::demo_module;

    #[test]
    fn base64_round_trips_known_vectors() {
        for (encoded, plain) in [
            ("", ""),
            ("Zg==", "f"),
            ("Zm8=", "fo"),
            ("Zm9v", "foo"),
            ("Zm9vYg==", "foob"),
            ("Zm9v\n YmFy", "foobar"),
        ] {
            assert_eq!(
                decode_base64(encoded).unwrap(),
                plain.as_bytes(),
                "{encoded}"
            );
        }
        assert!(decode_base64("Zm9").is_err());
        assert!(decode_base64("Zm9*").is_err());
        assert!(decode_base64("Z===").is_err());
    }

    #[test]
    fn names_are_cleaned_for_display() {
        assert_eq!(text(b"lead  \0garbage"), "lead");
        assert_eq!(text(b"a\x07b\xffc"), "a?b?c");
        assert_eq!(text(b"\0"), "");
    }

    #[test]
    fn cells_rows_and_samples_read_the_module() {
        let song = Song(demo_module());
        assert!(!song.title().is_empty());
        assert!(song.pattern_at(0) >= 0);
        assert_eq!(song.pattern_at(-1), -1);
        assert_eq!(song.pattern_at(999), -1);
        assert_eq!(song.row(0, 0).len(), 4);
        let outside = song.cell(0, 64, 0);
        assert_eq!(outside["period"].as_int().unwrap(), 0);
        let samples = song.samples();
        assert_eq!(samples.len(), 31);
        let first = samples[0].clone().cast::<Map>();
        assert_eq!(first["number"].as_int().unwrap(), 1);
        assert!(first["length"].as_int().unwrap() > 0);
    }

    #[test]
    fn garbage_is_refused_with_a_reason() {
        let error = Song::parse(b"hello").err().unwrap();
        assert!(error.contains("too short"), "{error}");
    }

    #[test]
    fn loading_goes_through_the_file_policy() {
        let dir = std::env::temp_dir().join(format!("lazyrad-os-song-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let policy = FsPolicy::Sandboxed(lazyrad_runtime::Sandbox::new(dir.clone()));
        assert!(load(&policy, "../outside.mod").is_err());
        std::fs::write(dir.join("bad.mod"), b"nope").unwrap();
        let error = load(&policy, "bad.mod").err().unwrap();
        assert!(error.contains("bad.mod"), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
