//! The hash-chained audit log `/data/log/pkg.log`.
//!
//! One line per event:
//!
//! ```text
//! <seq> <prev_hash_hex> <event_hex> <hash_hex>
//! ```
//!
//! `seq` counts from 1; `event_hex` is the hex of the generated `PkgEvent`
//! bytes (`idl/pkgd.midl`), the same record published on
//! `system/events/pkg/<op>`; `hash_hex` is SHA-256 over the raw `prev_hash`
//! bytes followed by the raw event bytes, and the next line's `prev_hash` is
//! that hash. The first line's `prev_hash` is 32 zero bytes. Removing,
//! reordering or editing any line breaks the chain from there on, which
//! [`verify`] reports with the line number. The chain is tamper-*evident*, not
//! tamper-proof: someone who can rewrite the whole file can rebuild it, which is
//! why every event is also published for `logd` to keep elsewhere.

use alloc::string::String;
use alloc::vec::Vec;

use lazyos_crypto::hex;
use lazyos_crypto::sha256::sha256_parts;
use messenger_generated::os_lazy_pkgd_v1::decode_pkg_event;

/// Length of a SHA-256 digest.
pub const HASH_LEN: usize = 32;
/// The `prev_hash` of the first record.
pub const GENESIS: [u8; HASH_LEN] = [0; HASH_LEN];
/// Longest event record the log accepts, so a corrupt line cannot make the
/// verifier allocate without bound. A real event is a few hundred bytes.
pub const MAX_EVENT_LEN: usize = 8 * 1024;

/// `sha256(prev || event)`: the hash a record carries.
pub fn chain_hash(prev: &[u8; HASH_LEN], event: &[u8]) -> [u8; HASH_LEN] {
    sha256_parts(&[prev, event])
}

/// Where a chain stands: how many records, and the hash the next one must
/// carry as its `prev_hash`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chain {
    pub count: u64,
    pub last: [u8; HASH_LEN],
}

impl Default for Chain {
    /// An empty log.
    fn default() -> Chain {
        Chain {
            count: 0,
            last: GENESIS,
        }
    }
}

impl Chain {
    /// The next log line (newline included) for `event`, advancing the chain.
    pub fn append(&mut self, event: &[u8]) -> String {
        let seq = self.count + 1;
        let hash = chain_hash(&self.last, event);
        let mut line = String::new();
        line.push_str(&alloc::format!("{seq} "));
        line.push_str(&hex::encode(&self.last));
        line.push(' ');
        line.push_str(&hex::encode(event));
        line.push(' ');
        line.push_str(&hex::encode(&hash));
        line.push('\n');
        self.count = seq;
        self.last = hash;
        line
    }
}

/// Why the log is not a valid chain: the 1-based line and the reason.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifyError {
    pub line: usize,
    pub reason: &'static str,
}

impl core::fmt::Display for VerifyError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "line {}: {}", self.line, self.reason)
    }
}

fn nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

/// Lowercase hex to bytes; `None` for odd length or a non-hex (or uppercase)
/// character, so one event has exactly one spelling.
fn unhex(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    bytes
        .chunks(2)
        .map(|pair| Some(nibble(pair[0])? << 4 | nibble(pair[1])?))
        .collect()
}

fn unhex_hash(text: &str) -> Option<[u8; HASH_LEN]> {
    unhex(text)?.try_into().ok()
}

/// Verify a whole log. Returns the chain state (so the writer can continue
/// it), or the first line that does not belong.
///
/// A line must have exactly four fields, the next `seq`, a `prev_hash` equal to
/// the previous hash, an event that decodes as a `PkgEvent`, and a hash that
/// matches. A last line without its newline is a torn write and is refused: an
/// append is one `write`, so a missing newline means it did not complete.
pub fn verify(text: &str) -> Result<Chain, VerifyError> {
    verify_from(Chain::default(), text)
}

/// [`verify`] for the next part of a log, continuing `chain` (the state the
/// part before it verified to), so a large log can be checked in pieces of
/// whole lines without holding it all. Line numbers count from the piece.
pub fn verify_from(chain: Chain, text: &str) -> Result<Chain, VerifyError> {
    let mut chain = chain;
    let mut rest = text;
    let mut line_no = 0usize;
    while !rest.is_empty() {
        line_no += 1;
        let fail = |reason| VerifyError {
            line: line_no,
            reason,
        };
        let Some((line, tail)) = rest.split_once('\n') else {
            return Err(fail("the last record is not newline-terminated"));
        };
        rest = tail;
        let mut fields = line.split(' ');
        let (Some(seq), Some(prev), Some(event), Some(hash), None) = (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        ) else {
            return Err(fail("a record needs four space-separated fields"));
        };
        if seq.parse::<u64>().ok() != Some(chain.count + 1) || seq.starts_with('0') {
            return Err(fail("the sequence number is not the next one"));
        }
        let prev = unhex_hash(prev).ok_or_else(|| fail("prev_hash is not 64 hex digits"))?;
        if prev != chain.last {
            return Err(fail("prev_hash does not match the previous record"));
        }
        if event.len() > MAX_EVENT_LEN * 2 {
            return Err(fail("the event is too long"));
        }
        let event = unhex(event).ok_or_else(|| fail("the event is not hex"))?;
        let hash = unhex_hash(hash).ok_or_else(|| fail("the hash is not 64 hex digits"))?;
        if chain_hash(&prev, &event) != hash {
            return Err(fail("the hash does not match the record"));
        }
        if decode_pkg_event(&event).is_err() {
            return Err(fail("the event is not a PkgEvent"));
        }
        chain = Chain {
            count: chain.count + 1,
            last: hash,
        };
    }
    Ok(chain)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;
    use alloc::string::ToString;
    use messenger_generated::os_lazy_pkgd_v1::{encode_pkg_event, PkgEvent};

    fn event(op: &str, ok: bool, detail: &str) -> Vec<u8> {
        encode_pkg_event(&PkgEvent {
            op: op.to_string(),
            system_name: "org.lazy.demo".to_string(),
            version: "1.0.0".to_string(),
            install_dir: "org.lazy.demo/1.0.0-abcdef12".to_string(),
            digest: "ab".repeat(32),
            actor_uid: 1000,
            ok,
            detail: detail.to_string(),
        })
        .expect("encodes")
    }

    fn log(events: &[Vec<u8>]) -> String {
        let mut chain = Chain::default();
        events.iter().map(|event| chain.append(event)).collect()
    }

    #[test]
    fn an_empty_log_is_valid() {
        assert_eq!(verify("").unwrap(), Chain::default());
    }

    #[test]
    fn a_chain_verifies_and_reports_where_it_ended() {
        let events = [
            event("install", true, ""),
            event("denied", false, "not the owner"),
            event("remove", true, ""),
        ];
        let text = log(&events);
        let chain = verify(&text).unwrap();
        assert_eq!(chain.count, 3);
        assert_eq!(text.lines().count(), 3);
        // The writer can pick up exactly where the verifier stopped.
        let mut resumed = chain;
        let line = resumed.append(&event("install", true, ""));
        let mut whole = text.clone();
        whole.push_str(&line);
        assert_eq!(verify(&whole).unwrap(), resumed);
    }

    #[test]
    fn the_line_format_is_seq_prev_event_hash() {
        let text = log(&[event("install", true, "")]);
        let fields: Vec<&str> = text.trim_end().split(' ').collect();
        assert_eq!(fields.len(), 4);
        assert_eq!(fields[0], "1");
        assert_eq!(fields[1], "0".repeat(64));
        assert_eq!(fields[2], hex::encode(&event("install", true, "")));
        let expected = chain_hash(&GENESIS, &event("install", true, ""));
        assert_eq!(fields[3], hex::encode(&expected));
    }

    #[test]
    fn flipping_any_byte_is_detected() {
        let text = log(&[
            event("install", true, ""),
            event("remove", true, ""),
            event("install", false, "no data volume"),
        ]);
        let original = text.as_bytes().to_vec();
        for index in 0..original.len() {
            let mut tampered = original.clone();
            // Flip within the alphabet each field allows, so the failure is the
            // chain's doing and not just "this is not hex".
            tampered[index] = match tampered[index] {
                b'\n' => b' ',
                b' ' => b'\n',
                b'0' => b'1',
                _ => b'0',
            };
            let tampered = String::from_utf8(tampered).unwrap();
            assert!(
                verify(&tampered).is_err(),
                "a change at byte {index} went unnoticed"
            );
        }
    }

    #[test]
    fn a_flipped_event_byte_names_the_record() {
        let text = log(&[event("install", true, ""), event("remove", true, "")]);
        let mut lines: Vec<String> = text.lines().map(String::from).collect();
        let mut fields: Vec<String> = lines[1].split(' ').map(String::from).collect();
        // Change one hex digit of the second record's event.
        let flipped = if fields[2].starts_with('0') { "1" } else { "0" };
        fields[2].replace_range(0..1, flipped);
        lines[1] = fields.join(" ");
        let tampered = format!("{}\n{}\n", lines[0], lines[1]);
        let error = verify(&tampered).unwrap_err();
        assert_eq!(error.line, 2);
    }

    #[test]
    fn a_log_verifies_in_pieces_of_whole_lines() {
        let mut chain = Chain::default();
        let mut text = String::new();
        for n in 0..6 {
            text.push_str(&chain.append(&event("install", n % 2 == 0, "")));
        }
        let whole = verify(&text).unwrap();
        let cut = text.match_indices('\n').nth(2).unwrap().0 + 1;
        let first = verify_from(Chain::default(), &text[..cut]).unwrap();
        assert_eq!(first.count, 3);
        assert_eq!(verify_from(first, &text[cut..]).unwrap(), whole);
        // The second piece alone does not start a chain.
        assert!(verify_from(Chain::default(), &text[cut..]).is_err());
    }

    #[test]
    fn removed_reordered_and_truncated_records_are_detected() {
        let text = log(&[
            event("install", true, ""),
            event("remove", true, ""),
            event("denied", false, "x"),
        ]);
        let lines: Vec<&str> = text.lines().collect();
        // Dropped from the middle.
        let dropped = format!("{}\n{}\n", lines[0], lines[2]);
        assert_eq!(verify(&dropped).unwrap_err().line, 2);
        // Swapped.
        let swapped = format!("{}\n{}\n{}\n", lines[1], lines[0], lines[2]);
        assert_eq!(verify(&swapped).unwrap_err().line, 1);
        // The head removed.
        let headless = format!("{}\n{}\n", lines[1], lines[2]);
        assert!(verify(&headless).is_err());
        // A torn final line.
        let torn = &text[..text.len() - 5];
        assert_eq!(verify(torn).unwrap_err().line, 3);
        // Removing only the tail is a valid (shorter) chain: the audit is
        // tamper-evident against edits, and the published events cover loss.
        let shorter = format!("{}\n{}\n", lines[0], lines[1]);
        assert_eq!(verify(&shorter).unwrap().count, 2);
    }

    #[test]
    fn malformed_lines_are_errors_not_panics() {
        for text in [
            "garbage\n",
            "1 2 3\n",
            "1 a b c d\n",
            "\n",
            "1  \n",
            &format!("1 {} zz {}\n", "0".repeat(64), "0".repeat(64)),
            &format!("1 {} ab {}\n", "0".repeat(64), "G".repeat(64)),
            &format!("2 {} ab {}\n", "0".repeat(64), "0".repeat(64)),
            &format!("01 {} ab {}\n", "0".repeat(64), "0".repeat(64)),
        ] {
            assert!(verify(text).is_err(), "{text:?}");
        }
    }

    #[test]
    fn a_hash_valid_record_that_is_not_an_event_is_refused() {
        let mut chain = Chain::default();
        let line = chain.append(&[0xff, 0xff, 0xff]);
        assert_eq!(
            verify(&line).unwrap_err().reason,
            "the event is not a PkgEvent"
        );
    }

    #[test]
    fn an_oversized_event_is_refused_before_decoding() {
        let huge = "ab".repeat(MAX_EVENT_LEN + 1);
        let line = format!("1 {} {huge} {}\n", "0".repeat(64), "0".repeat(64));
        assert_eq!(verify(&line).unwrap_err().reason, "the event is too long");
    }

    #[test]
    fn uppercase_hex_is_not_a_second_spelling() {
        let text = log(&[event("install", true, "")]);
        let upper = text.to_uppercase();
        assert!(verify(&upper).is_err());
    }
}
