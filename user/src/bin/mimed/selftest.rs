//! `mimed`'s boot self-test: database guesses, a registry round trip, and the
//! open walk. Every check prints one machine-parseable marker so a headless
//! boot proves the path.
//!
//! Split out of `mimed.rs`, which is past the file-size budget.

use alloc::format;
use user::messenger::mime;
use user::sys;

use super::apps::{choose, AppRegistry};
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

    // The seeded file-type defaults (issue #116): plain text opens in the
    // Editor, PNGs in Paint, Markdown in the Docs renderer. The registry is
    // seeded the same in every image, so the mapping is asserted directly.
    let plain = apps.lookup("text/plain", "open");
    let png = apps.lookup("image/png", "open");
    let markdown = apps.lookup("text/markdown", "open");
    if plain == Some("editor") && png == Some("paint") && markdown == Some("docs") {
        sys::write_str("MIME:DEFAULT:PASS text/plain=editor image/png=paint text/markdown=docs\n");
    } else {
        sys::write_str(&format!(
            "MIME:DEFAULT:FAIL text/plain={} image/png={} text/markdown={}\n",
            plain.unwrap_or("<none>"),
            png.unwrap_or("<none>"),
            markdown.unwrap_or("<none>"),
        ));
    }

    // Markdown keeps the Editor as its `edit` verb and Docs as its `view` verb,
    // and names the Editor as the fallback for `open` when Docs is not shipped.
    let verbs_ok = apps.lookup("text/markdown", "edit") == Some("editor")
        && apps.lookup("text/markdown", "view") == Some("docs");
    if verbs_ok {
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

    // The fallback policy is pure: an unshipped primary resolves to the named
    // fallback, a shipped one stays, and a pair without a fallback keeps its
    // primary. Exercised directly so a boot proves it without a live `init`.
    let fallback = apps
        .resolve("text/markdown", "open")
        .and_then(|(_, fallback)| fallback);
    let fallback_ok = choose("docs", fallback, false) == "editor"
        && choose("docs", fallback, true) == "docs"
        && choose("editor", None, false) == "editor";
    if fallback_ok {
        sys::write_str("MIME:FALLBACK:PASS text/markdown=docs->editor\n");
    } else {
        sys::write_str("MIME:FALLBACK:FAIL markdown open fallback\n");
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
