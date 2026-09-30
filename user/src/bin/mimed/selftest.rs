//! `mimed`'s boot self-test: database guesses, a registry round trip, and the
//! open walk. Every check prints one machine-parseable marker so a headless
//! boot proves the path.
//!
//! Split out of `mimed.rs`, which is past the file-size budget.

use alloc::format;
use user::messenger::mime;
use user::sys;

use super::apps::AppRegistry;
use super::db::MimeDb;
use super::handlers::open_path;

/// The boot self-test: database guesses, a registry round trip, and the open
/// walk. Every check prints one machine-parseable marker.
pub(crate) fn selftest(db: &MimeDb, apps: &mut AppRegistry) {
    for (path, expected) in [
        ("NOTES.TXT", "text/plain"),
        ("README.MD", "text/markdown"),
        ("MAIN.RS", "text/x-rust"),
        ("APP.ELF", "application/x-elf"),
        ("LOGO.PNG", "image/png"),
        ("DATA.BIN", mime::FALLBACK_MIME),
    ] {
        let got = db.guess(path);
        if got == expected {
            sys::write_str(&format!("MIME:GUESS:PASS {path} {got}\n"));
        } else {
            sys::write_str(&format!(
                "MIME:GUESS:FAIL {path} got={got} want={expected}\n"
            ));
        }
    }
    // The override file is shipped in the services image; a custom image
    // without it still passes, with an informational line instead.
    match &db.source {
        Some(source) => {
            let path = "SAMPLE.LZT";
            let expected = "text/x-lazy-test";
            let got = db.guess(path);
            if got == expected {
                sys::write_str(&format!("MIME:GUESS:PASS {path} {got} ({source})\n"));
            } else {
                sys::write_str(&format!(
                    "MIME:GUESS:FAIL {path} got={got} want={expected} ({source})\n"
                ));
            }
        }
        None => sys::write_str("MIME:GUESS:INFO no override file; built-ins only\n"),
    }

    apps.register("text/x-lazy-test", "lazytest", "open");
    apps.register("text/x-lazy-test", "lazytest", "edit");
    let looked_up = apps.lookup("text/x-lazy-test", "open") == Some("lazytest");
    let verbs = apps.verbs("text/x-lazy-test");
    let verbs_ok =
        verbs.iter().any(|verb| verb == "open") && verbs.iter().any(|verb| verb == "edit");
    if looked_up && verbs_ok {
        sys::write_str("MIME:REGISTER:PASS\n");
    } else {
        sys::write_str("MIME:REGISTER:FAIL lookup or verbs mismatch\n");
    }

    // Markdown opens in the Editor and can be viewed (rendered) in Docs.
    if apps.lookup("text/markdown", "open") == Some("editor")
        && apps.lookup("text/markdown", "edit") == Some("editor")
        && apps.lookup("text/markdown", "view") == Some("docs")
    {
        sys::write_str(
            "MIME:VIEW:PASS
",
        );
    } else {
        sys::write_str(
            "MIME:VIEW:FAIL markdown verbs
",
        );
    }

    let mut bus = None;
    for (path, expected) in [
        ("NOTES.TXT", "editor"),
        ("LOGO.PNG", "paint"),
        ("SAMPLE.LZT", "lazytest"),
    ] {
        // Session 0: the service's own (system) session. No open-with app is
        // installed yet in this image, so the launch attempt exercises the
        // gated fallback and the open walk still passes on the publish record.
        match open_path(db, apps, &mut bus, path, mime::DEFAULT_VERB, Some(0)) {
            Ok(result) if result.app == expected => {
                sys::write_str(&format!("MIME:OPEN:PASS {path} {}\n", result.app));
                if !result.published {
                    sys::write_str(&format!(
                        "MIME:OPEN:INFO {path} resolved; launch event not published\n"
                    ));
                }
            }
            Ok(result) => sys::write_str(&format!(
                "MIME:OPEN:FAIL {path} got={} want={expected}\n",
                result.app
            )),
            Err(error) => sys::write_str(&format!("MIME:OPEN:FAIL {path} {}\n", error.message())),
        }
    }
}
