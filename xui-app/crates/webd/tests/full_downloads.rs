//! Downloads of a browser view: where a file may be written, the `.part` protocol, and what the
//! client hears. The Downloads folder is a real directory in the temp folder.

#![cfg(not(feature = "host"))]

mod support;

use std::path::Path;

use messenger_generated::os_lazy_webd_view_v1 as wire;
use support::full::{browser, data_only, web, FEv, Fx, Via};
use support::origin;
use xui_webd::fabric::errno::*;
use xui_webd::recording::Call as EngineCall;

fn downloading() -> wire::ViewConfig {
    wire::ViewConfig {
        downloads: true,
        ..browser()
    }
}

/// The `.part` path the service handed out must be a file directly inside the Downloads folder.
fn assert_inside(fx: &Fx, part: &str, name: &str) {
    let part = Path::new(part);
    assert_eq!(part.parent(), Some(fx.dir.0.as_path()), "{part:?}");
    assert_eq!(
        part.file_name().and_then(|n| n.to_str()),
        Some(format!("{name}.part").as_str())
    );
}

#[test]
fn a_download_gets_a_part_file_inside_the_folder_under_a_name_the_server_cannot_steer() {
    // Why: the server (the page's origin) suggests the file name; it must not choose the folder,
    // hide the file, spoof an extension with invisible characters or take a name that exists.
    let mut fx = Fx::new("dl-names");
    let app = origin(7);
    let (view, events) = fx.view(app, Via::Browser, downloading(), 4, 4);
    let cases = [
        ("../../etc/passwd", "passwd"),
        ("C:\\Windows\\evil.exe", "evil.exe"),
        (".hidden", "hidden"),
        ("a\u{7}b\nc.txt", "a_b_c.txt"),
        ("invoice\u{202E}fdp.exe", "invoice_fdp.exe"),
        ("", "download"),
        // The same fallback twice: the second is numbered.
        ("..", "download (1)"),
        ("a:b*c?.zip", "a_b_c_.zip"),
    ];
    for (n, (suggested, safe)) in cases.iter().enumerate() {
        let id = n as u64 + 1;
        let part = fx
            .queue
            .download_start(view, id, "http://example.com/f", suggested, 10)
            .unwrap_or_else(|| panic!("{suggested:?} refused"));
        assert_inside(&fx, &part, safe);
        fx.pump(0);
        let started = wire::DownloadStartedArgs {
            view,
            download: id,
            url: "http://example.com/f".into(),
            name: (*safe).into(),
            total: 10,
        };
        assert_eq!(
            fx.news(events),
            vec![FEv::DownloadStarted(started)],
            "{suggested:?}"
        );
    }
    // A name already reserved gets a number, not the same file.
    let first = fx
        .queue
        .download_start(view, 20, "http://x/", "same.zip", -1)
        .unwrap();
    let second = fx
        .queue
        .download_start(view, 21, "http://x/", "same.zip", -1)
        .unwrap();
    assert_inside(&fx, &first, "same.zip");
    assert_inside(&fx, &second, "same (1).zip");
}

#[test]
fn a_finished_download_becomes_its_file_and_a_failed_one_leaves_nothing() {
    // Why: the engine writes the `.part` file; the service renames it when every byte arrived and
    // removes it otherwise, and the client is told which happened.
    let mut fx = Fx::new("dl-finish");
    let app = origin(7);
    let (view, events) = fx.view(app, Via::Browser, downloading(), 4, 4);
    let part = fx
        .queue
        .download_start(view, 1, "http://x/a", "a.txt", 4)
        .unwrap();
    std::fs::write(&part, b"data").unwrap();
    fx.queue.download_progress(view, 1, 2, 4);
    fx.queue.download_progress(view, 1, 4, 4);
    fx.queue.download_done(view, 1);
    let dir = fx.dir.0.clone();
    assert_eq!(std::fs::read(dir.join("a.txt")).unwrap(), b"data");
    assert!(!dir.join("a.txt.part").exists());

    let part = fx
        .queue
        .download_start(view, 2, "http://x/b", "b.txt", -1)
        .unwrap();
    std::fs::write(&part, b"half").unwrap();
    fx.queue.download_fail(view, 2, "connection reset");
    assert!(!dir.join("b.txt.part").exists() && !dir.join("b.txt").exists());
    // Done with no file written (the engine lied or the file was removed) is a failure, not a success.
    fx.queue
        .download_start(view, 3, "http://x/c", "c.txt", -1)
        .unwrap();
    fx.queue.download_done(view, 3);
    assert!(!dir.join("c.txt").exists());

    fx.pump(0);
    let sent = fx.news(events);
    // The progress reports were merged into the newest one.
    let progress: Vec<_> = sent
        .iter()
        .filter(|e| matches!(e, FEv::DownloadProgress(_)))
        .collect();
    assert_eq!(
        progress,
        vec![&FEv::DownloadProgress(wire::DownloadProgressArgs {
            view,
            download: 1,
            received: 4,
            total: 4
        })]
    );
    assert!(sent.contains(&FEv::DownloadDone(wire::DownloadDoneArgs {
        view,
        download: 1,
        name: "a.txt".into()
    })));
    assert!(
        sent.contains(&FEv::DownloadFailed(wire::DownloadFailedArgs {
            view,
            download: 2,
            reason: "connection reset".into()
        }))
    );
    assert!(sent
        .iter()
        .any(|e| matches!(e, FEv::DownloadFailed(f) if f.download == 3)));
    assert!(!sent
        .iter()
        .any(|e| matches!(e, FEv::DownloadDone(d) if d.download == 3)));
    // A finished file is never replaced by a later download of the same name.
    let part = fx
        .queue
        .download_start(view, 4, "http://x/a", "a.txt", 1)
        .unwrap();
    assert_inside(&fx, &part, "a (1).txt");
}

#[test]
fn only_a_view_made_with_downloads_gets_a_destination() {
    // Why: the engine asks for a destination on the engine's thread for every response it cannot
    // show; a view that did not ask to download must get none, whatever the page serves.
    let mut fx = Fx::new("dl-refused");
    let app = origin(7);
    let (plain, events) = fx.view(app, Via::Browser, browser(), 4, 4);
    let (offline, _) = fx.view(app, Via::View, data_only(), 4, 4);
    let (net, _) = fx.view(app, Via::Net, web(), 4, 4);
    for view in [plain, offline, net, 9999] {
        assert_eq!(
            fx.queue.download_start(view, 1, "http://x/", "f", 1),
            None,
            "view {view}"
        );
    }
    fx.pump(0);
    assert!(fx.news(events).is_empty());
    assert!(
        !fx.dir.0.exists(),
        "nothing was created for a refused download"
    );
    // The calls are refused too.
    fx.engine.take();
    assert_eq!(fx.download(app, plain, "http://example.com/f"), Err(EACCES));
    assert_eq!(fx.download_cancel(app, plain, 1), Err(EACCES));
    assert!(fx.engine.take().is_empty());
}

#[test]
fn download_and_cancel_reach_the_engine_and_save_page_as_follows_the_policy() {
    // Why: `Download` is Save Page As: the address is judged like a `Load`, and a view cannot use
    // it to reach what its policy would not load.
    let mut fx = Fx::new("dl-calls");
    let app = origin(7);
    let config = wire::ViewConfig {
        scheme_policy: wire::SCHEME_POLICY_ORIGINS,
        origins: vec!["https://example.com".into()],
        ..downloading()
    };
    let (view, _) = fx.view(app, Via::Browser, config, 4, 4);
    fx.engine.take();
    fx.download(app, view, "https://example.com/file.zip")
        .unwrap();
    for url in [
        "http://example.com/file.zip",
        "file:///etc/passwd",
        "mailto:a@b",
        "https://other.test/",
    ] {
        assert_eq!(fx.download(app, view, url), Err(EACCES), "{url}");
    }
    assert_eq!(
        fx.download(
            app,
            view,
            &format!("https://example.com/{}", "a".repeat(9000))
        ),
        Err(EINVAL)
    );
    fx.download_cancel(app, view, 5).unwrap();
    assert_eq!(
        fx.engine.take(),
        vec![
            EngineCall::Download(view, "https://example.com/file.zip".into()),
            EngineCall::DownloadCancel(view, 5),
        ]
    );
    assert_eq!(
        fx.download(origin(8), view, "https://example.com/f"),
        Err(ENOENT)
    );
    assert_eq!(fx.download_cancel(origin(8), view, 5), Err(ENOENT));
}

#[test]
fn a_client_has_at_most_32_downloads_under_way_and_a_finished_one_frees_its_place() {
    // Why: a page needs no gesture to serve an attachment; a loop of them must not fill the folder's
    // bookkeeping or the disk, and (review of 2026-10-22, finding 5) must neither use up the places
    // of the other clients of the one saver nor lock the session out for good once it has seen 256.
    let mut fx = Fx::new("dl-cap");
    let (app, other) = (origin(7), origin(8));
    let (view, _) = fx.view(app, Via::Browser, downloading(), 4, 4);
    let (neighbour, _) = fx.view(other, Via::Browser, downloading(), 4, 4);
    let start = |fx: &Fx, view, id: u64| {
        fx.queue
            .download_start(view, id, "http://x/", &format!("f{id}"), 1)
    };
    let per_client = lazydownloads::MAX_DOWNLOADS_PER_OWNER as u64;
    for id in 1..=per_client {
        assert!(start(&fx, view, id).is_some(), "{id}");
    }
    assert_eq!(start(&fx, view, 9999), None, "the client's places are used");
    assert!(
        start(&fx, neighbour, 1).is_some(),
        "another client's downloads are not affected"
    );
    // Downloads end and others start, far more than the old session-wide 256: the oldest under way
    // fails each round (the first `per_client` are 1.., the rest 1000..).
    for round in 0..lazydownloads::MAX_DOWNLOADS as u64 * 2 {
        let ending = if round < per_client {
            round + 1
        } else {
            1000 + round - per_client
        };
        fx.queue.download_fail(view, ending, "connection reset");
        fx.pump(round);
        assert!(start(&fx, view, 1000 + round).is_some(), "round {round}");
    }
    assert!(
        start(&fx, neighbour, 2).is_some(),
        "the neighbour still has its places"
    );
}

#[test]
fn a_download_of_a_view_that_ended_is_refused() {
    // Why: the view's rules go with it; a late callback must not find a destination.
    let mut fx = Fx::new("dl-gone");
    let app = origin(7);
    let (view, events) = fx.view(app, Via::Browser, downloading(), 4, 4);
    fx.destroy_view(app, view).unwrap();
    assert_eq!(fx.queue.download_start(view, 1, "http://x/", "f", 1), None);
    fx.queue.download_done(view, 1);
    fx.pump(0);
    assert!(fx.news(events).is_empty());
}
