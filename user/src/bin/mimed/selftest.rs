//! `mimed`'s boot self-test: database guesses, a registry round trip, and the
//! open walk. Every check prints one machine-parseable marker so a headless
//! boot proves the path.
//!
//! Split out of `mimed.rs`, which is past the file-size budget.

use alloc::string::ToString;
use user::messenger::mime;
use user::sys;
use user::trace;

use super::apps::{choose, AppRegistry};
use super::db::MimeDb;
use super::handlers::open_path;
use super::prefs::Prefs;

/// The boot self-test: database guesses, a registry round trip, and the open
/// walk. Every check prints one machine-parseable marker.
pub(crate) fn selftest(db: &MimeDb, apps: &mut AppRegistry) {
    for (path, expected) in [
        ("NOTES.TXT", "text/plain"),
        (fhs::docs::README, "text/markdown"),
        ("MAIN.RS", "text/x-rust"),
        ("APP.ELF", "application/x-elf"),
        ("LOGO.PNG", "image/png"),
        ("HOLIDAY.JPEG", "image/jpeg"),
        ("ANIM.GIF", "image/gif"),
        ("ICON.BMP", "image/bmp"),
        ("DRAWING.ORA", "image/openraster"),
        ("LETTER.LZW", "application/x-lazywriter"),
        ("BUNDLE.ZIP", "application/zip"),
        ("SRC.TAR.GZ", "application/gzip"),
        ("PHOTOS.7Z", "application/x-7z-compressed"),
        ("MANUAL.PDF", "application/pdf"),
        ("DATA.BIN", mime::FALLBACK_MIME),
        ("https://example.com/a.zip", "x-scheme-handler/https"),
        ("HTTP://example.com/", "x-scheme-handler/http"),
        ("mailto:me@example.com", "x-scheme-handler/mailto"),
        ("file:///docs/index.html", "text/html"),
    ] {
        let got = db.guess(path);
        if got == expected {
            trace::line!(trace::Channel::Mime, "GUESS:PASS {path} {got}");
        } else {
            trace::line!(
                trace::Channel::Mime,
                "GUESS:FAIL {path} got={got} want={expected}"
            );
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
                trace::line!(trace::Channel::Mime, "GUESS:PASS {path} {got} ({source})");
            } else {
                trace::line!(
                    trace::Channel::Mime,
                    "GUESS:FAIL {path} got={got} want={expected} ({source})"
                );
            }
        }
        None => trace::line!(
            trace::Channel::Mime,
            "GUESS:INFO no override file; built-ins only"
        ),
    }

    apps.register("text/x-lazy-test", "lazytest", "open");
    apps.register("text/x-lazy-test", "lazytest", "edit");
    let looked_up = apps.lookup("text/x-lazy-test", "open") == Some("lazytest");
    let verbs = apps.verbs("text/x-lazy-test");
    let verbs_ok =
        verbs.iter().any(|verb| verb == "open") && verbs.iter().any(|verb| verb == "edit");
    if looked_up && verbs_ok {
        trace::line!(trace::Channel::Mime, "REGISTER:PASS");
    } else {
        trace::line!(
            trace::Channel::Mime,
            "REGISTER:FAIL lookup or verbs mismatch"
        );
    }

    // The registry is bounded: a full one refuses a new pair, still takes a
    // held pair's replacement, and frees room when a pair is withdrawn.
    let mut small = AppRegistry::with_limit(3);
    let filled = (0..3).all(|n| small.register(&alloc::format!("text/x-cap-{n}"), "a", "open"));
    let refused = !small.register("text/x-cap-3", "a", "open");
    let replaced = small.register("text/x-cap-0", "b", "open");
    small.unregister("text/x-cap-1", "a", "open");
    let room = small.register("text/x-cap-3", "a", "open");
    if filled && refused && replaced && room {
        trace::line!(trace::Channel::Mime, "REGISTER:LIMIT:PASS");
    } else {
        trace::line!(
            trace::Channel::Mime,
            "REGISTER:LIMIT:FAIL filled={filled} refused={refused} replaced={replaced} room={room}"
        );
    }

    // Withdrawing a registration (the package manager's removal path): the
    // handler it replaced takes over again, and a registration someone else
    // made later is never undone.
    apps.register("text/x-lazy-unreg", "first", "open");
    apps.register("text/x-lazy-unreg", "second", "open");
    let swapped = apps.lookup("text/x-lazy-unreg", "open") == Some("second");
    apps.unregister("text/x-lazy-unreg", "second", "open");
    let restored = apps.lookup("text/x-lazy-unreg", "open") == Some("first");
    apps.unregister("text/x-lazy-unreg", "ghost", "open");
    let untouched = apps.lookup("text/x-lazy-unreg", "open") == Some("first");
    apps.unregister("text/x-lazy-unreg", "first", "open");
    let dropped = apps.lookup("text/x-lazy-unreg", "open").is_none();
    if swapped && restored && untouched && dropped {
        trace::line!(trace::Channel::Mime, "UNREGISTER:PASS");
    } else {
        trace::line!(trace::Channel::Mime, "UNREGISTER:FAIL swapped={swapped} restored={restored} untouched={untouched} dropped={dropped}");
    }

    // The seeded file-type defaults (issue #116): plain text opens in the
    // Editor, PNGs in Paint, Markdown in the Docs renderer. The registry is
    // seeded the same in every image, so the mapping is asserted directly.
    let plain = apps.lookup("text/plain", "open");
    let png = apps.lookup("image/png", "open");
    let markdown = apps.lookup("text/markdown", "open");
    if plain == Some("os.lazy.editor")
        && png == Some("os.lazy.paint")
        && markdown == Some("os.lazy.docs")
    {
        trace::line!(trace::Channel::Mime, "DEFAULT:PASS text/plain=os.lazy.editor image/png=os.lazy.paint text/markdown=os.lazy.docs");
    } else {
        trace::line!(
            trace::Channel::Mime,
            "DEFAULT:FAIL text/plain={} image/png={} text/markdown={}",
            plain.unwrap_or("<none>"),
            png.unwrap_or("<none>"),
            markdown.unwrap_or("<none>")
        );
    }

    // LazyWriter documents (issue #533) open and edit in LazyWriter; plain
    // text and Markdown above stay with the Editor and Docs.
    let writer_open = apps.lookup("application/x-lazywriter", "open");
    let writer_edit = apps.lookup("application/x-lazywriter", "edit");
    if writer_open == Some("os.lazy.writer") && writer_edit == Some("os.lazy.writer") {
        trace::line!(
            trace::Channel::Mime,
            "DEFAULT:PASS application/x-lazywriter=os.lazy.writer"
        );
    } else {
        trace::line!(
            trace::Channel::Mime,
            "DEFAULT:FAIL application/x-lazywriter open={} edit={}",
            writer_open.unwrap_or("<none>"),
            writer_edit.unwrap_or("<none>")
        );
    }

    // Archives open in the Archiver (docs/archiver-plan.md).
    let archive_open = apps.lookup("application/zip", "open");
    if archive_open == Some("os.lazy.archiver") {
        sys::write_str(
            "MIME:DEFAULT:PASS application/zip=os.lazy.archiver
",
        );
    } else {
        trace::line!(
            trace::Channel::Mime,
            "DEFAULT:FAIL application/zip open={}",
            archive_open.unwrap_or("<none>")
        );
    }

    // PDF documents open in the PDF Viewer (docs/pdf-reader-plan.md).
    let pdf_open = apps.lookup("application/pdf", "open");
    if pdf_open == Some("os.lazy.pdf") {
        trace::line!(
            trace::Channel::Mime,
            "DEFAULT:PASS application/pdf=os.lazy.pdf"
        );
    } else {
        trace::line!(
            trace::Channel::Mime,
            "DEFAULT:FAIL application/pdf open={}",
            pdf_open.unwrap_or("<none>")
        );
    }

    // Markdown keeps the Editor as its `edit` verb and Docs as its `view` verb,
    // and names the Editor as the fallback for `open` when Docs is not shipped.
    let verbs_ok = apps.lookup("text/markdown", "edit") == Some("os.lazy.editor")
        && apps.lookup("text/markdown", "view") == Some("os.lazy.docs");
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
    let fallback_ok = choose("os.lazy.docs", fallback, false) == "os.lazy.editor"
        && choose("os.lazy.docs", fallback, true) == "os.lazy.docs"
        && choose("os.lazy.editor", None, false) == "os.lazy.editor";
    if fallback_ok {
        trace::line!(
            trace::Channel::Mime,
            "FALLBACK:PASS text/markdown=os.lazy.docs->os.lazy.editor"
        );
    } else {
        trace::line!(trace::Channel::Mime, "FALLBACK:FAIL markdown open fallback");
    }

    // JPEG and BMP: the Picture Viewer opens, Paint edits, and Paint is the
    // `open` fallback when the viewer is not shipped.
    let pictures_ok = ["image/jpeg", "image/bmp"].iter().all(|mime| {
        let fallback = apps
            .resolve(mime, "open")
            .and_then(|(_, fallback)| fallback);
        apps.lookup(mime, "open") == Some("os.lazy.pictures")
            && apps.lookup(mime, "edit") == Some("os.lazy.paint")
            && choose("os.lazy.pictures", fallback, false) == "os.lazy.paint"
            && choose("os.lazy.pictures", fallback, true) == "os.lazy.pictures"
    });
    if pictures_ok {
        sys::write_str(
            "MIME:PICTURES:PASS jpeg,bmp open=os.lazy.pictures edit=os.lazy.paint
",
        );
    } else {
        sys::write_str(
            "MIME:PICTURES:FAIL jpeg/bmp open and edit
",
        );
    }

    prefers(apps);
    owned(apps);

    let mut bus = None;
    let mut prefs = Prefs::new();
    for (path, expected) in [
        ("NOTES.TXT", "os.lazy.editor"),
        ("LOGO.PNG", "os.lazy.paint"),
        ("SAMPLE.LZT", "lazytest"),
    ] {
        // Session 0: the service's own (system) session. No open-with app is
        // installed yet in this image, so the launch attempt exercises the
        // gated fallback and the open walk still passes on the publish record.
        match open_path(
            db,
            apps,
            &mut bus,
            &mut prefs,
            path,
            mime::DEFAULT_VERB,
            Some(0),
            None,
        ) {
            Ok(result) if result.app == expected => {
                trace::line!(trace::Channel::Mime, "OPEN:PASS {path} {}", result.app);
                if !result.published {
                    trace::line!(
                        trace::Channel::Mime,
                        "OPEN:INFO {path} resolved; launch event not published"
                    );
                }
            }
            Ok(result) => trace::line!(
                trace::Channel::Mime,
                "OPEN:FAIL {path} got={} want={expected}",
                result.app
            ),
            Err(error) => {
                trace::line!(trace::Channel::Mime, "OPEN:FAIL {path} {}", error.message())
            }
        }
    }
}

/// A user's choice between two apps that handle one type (docs/lazywebplus.md): the choice wins
/// when the app handles the type, an app that does not handle it is ignored, and the key is a
/// `confd` path in the user's own subtree. Pure, so a boot proves it without `confd`.
fn prefers(apps: &mut AppRegistry) {
    const TYPE: &str = "x-scheme-handler/lazytest";
    apps.register(TYPE, "os.lazy.first", "open");
    apps.register(TYPE, "os.lazy.second", "open");
    let resolve = |apps: &AppRegistry, preferred| {
        apps.resolve_preferred(TYPE, "open", preferred, None)
            .map(|(app, _)| app.to_string())
    };
    let by_install_order = resolve(apps, None).as_deref() == Some("os.lazy.second");
    let chosen = resolve(apps, Some("os.lazy.first")).as_deref() == Some("os.lazy.first");
    let registered_is_chosen =
        resolve(apps, Some("os.lazy.second")).as_deref() == Some("os.lazy.second");
    let stranger = resolve(apps, Some("os.lazy.stranger")).as_deref() == Some("os.lazy.second");
    let empty = resolve(apps, Some("  ")).as_deref() == Some("os.lazy.second");
    let untyped = apps
        .resolve_preferred("x-scheme-handler/none", "open", Some("os.lazy.first"), None)
        .is_none();
    apps.unregister(TYPE, "os.lazy.second", "open");
    apps.unregister(TYPE, "os.lazy.first", "open");
    if by_install_order && chosen && registered_is_chosen && stranger && empty && untyped {
        trace::line!(
            trace::Channel::Mime,
            "PREFER:PASS a user's default app wins, a stranger does not"
        );
    } else {
        trace::line!(
            trace::Channel::Mime,
            "PREFER:FAIL order={by_install_order} chosen={chosen} registered={registered_is_chosen} stranger={stranger} empty={empty} untyped={untyped}"
        );
    }
}

/// A registration one user's app made is seen by that user alone (audit H10):
/// another user's `Open` resolves to the machine's app (or nothing), and
/// withdrawing the user's app gives the type back. Pure, so a boot proves it.
fn owned(apps: &mut AppRegistry) {
    const TYPE: &str = "x-scheme-handler/lazyowner";
    apps.register(TYPE, "os.lazy.machine", "open");
    apps.register_owned(TYPE, "evil.app", "open", 1000);
    let seen = |apps: &AppRegistry, uid| {
        apps.resolve_preferred(TYPE, "open", None, uid)
            .map(|(app, _)| app.to_string())
    };
    let owner = seen(apps, Some(1000)).as_deref() == Some("evil.app");
    let other = seen(apps, Some(1001)).as_deref() == Some("os.lazy.machine");
    let contested =
        apps.contested(TYPE, "open", Some(1000)) && !apps.contested(TYPE, "open", Some(1001));
    // A user's preference for an app they cannot be sent to is ignored.
    let hidden = apps
        .resolve_preferred(TYPE, "open", Some("evil.app"), Some(1001))
        .map(|(app, _)| app.to_string())
        .as_deref()
        == Some("os.lazy.machine");
    apps.unregister(TYPE, "evil.app", "open");
    let restored = seen(apps, Some(1000)).as_deref() == Some("os.lazy.machine");
    apps.unregister(TYPE, "os.lazy.machine", "open");
    let only_theirs = {
        apps.register_owned(TYPE, "evil.app", "open", 1000);
        let none_for_others = seen(apps, Some(1001)).is_none();
        apps.unregister(TYPE, "evil.app", "open");
        none_for_others
    };
    if owner && other && contested && hidden && restored && only_theirs {
        trace::line!(
            trace::Channel::Mime,
            "OWNER:PASS a user's registration is seen by that user alone"
        );
    } else {
        trace::line!(
            trace::Channel::Mime,
            "OWNER:FAIL owner={owner} other={other} contested={contested} hidden={hidden} restored={restored} only_theirs={only_theirs}"
        );
    }
}
