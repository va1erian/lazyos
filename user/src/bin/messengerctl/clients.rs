//! Key, clipboard and MIME/open-with command output.

use alloc::format;
use user::messenger::{clipboard, keyd, mime};
use user::sys;

use super::commands::report;

/// `keys`: list the `keyd` service's key ids and use counters (issue #102).
/// Key material is never part of the protocol, so this command cannot and
/// does not print any.
pub(crate) fn print_keys() {
    let client = match keyd::Client::connect() {
        Ok(client) => client,
        Err(error) => return report(error.message()),
    };
    match client.keys() {
        Ok(keys) if keys.is_empty() => sys::write_str("keyd: no keys held\n"),
        Ok(keys) => {
            sys::write_str(&format!("keyd: {} key(s)\n", keys.len()));
            for key in &keys {
                sys::write_str(&format!(
                    "  #{} {:<8} uses {:<3} last-use tick {}\n",
                    key.id, key.kind, key.uses, key.last_use
                ));
            }
        }
        Err(error) => report(error.message()),
    }
}

/// `clipboard`: the current offer's MIME types and owner from `clipboardd`
/// (issue #115). Metadata only: the `Current` method has no content field, so
/// this command cannot and does not print any payload bytes.
pub(crate) fn print_clipboard() {
    let client = match clipboard::Client::connect() {
        Ok(client) => client,
        Err(error) => return report(error.message()),
    };
    match client.current() {
        Ok(None) => sys::write_str("clipboard: no offer\n"),
        Ok(Some(offer)) => {
            sys::write_str(&format!(
                "clipboard: token {}  owner {}  session {}\n",
                offer.token, offer.owner, offer.session
            ));
            sys::write_str(&format!(
                "  {}  {} mime(s)  tick {}\n",
                if offer.lazy { "lazy" } else { "eager" },
                offer.mimes.len(),
                offer.tick
            ));
            for mime in &offer.mimes {
                sys::write_str(&format!("    {mime}\n"));
            }
        }
        Err(error) => report(error.message()),
    }
}

/// `mime <path>`: the type `mimed`'s database guesses for a path (issue
/// #116). Falls back to `application/octet-stream` when `mimed` is absent.
pub(crate) fn print_mime(path: &str) {
    if path.is_empty() {
        return report("usage: mime <path>");
    }
    sys::write_str(&format!("{}: {}\n", path, mime::guess(path)));
}

/// `open <path> [verb]`: resolve the open-with app for the path and print the
/// launch event `mimed` published (issue #116). The verb defaults to `open`.
pub(crate) fn open_path(rest: &str) {
    let mut parts = rest.split_whitespace();
    let Some(path) = parts.next() else {
        return report("usage: open <path> [verb]");
    };
    let verb = parts.next().unwrap_or(mime::DEFAULT_VERB);
    match mime::open(path, verb) {
        Ok(result) => {
            sys::write_str(&format!(
                "open {} -> {} ({}, topic {})\n",
                path, result.app, result.mime, result.topic
            ));
            if result.launched {
                sys::write_str("  (launched through os.lazy.init)\n");
            }
            if !result.published {
                sys::write_str("  (launch event not published; supervisor unreachable)\n");
            }
        }
        Err(error) => report(error.message()),
    }
}
