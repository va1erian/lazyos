//! The `webdbrowse` scenario end to end against the service half and the fake web: every step
//! passes, in order, with the details the QEMU harness reads; the blocked picture is fetched only
//! once the blocker was bypassed, and the page of the view without a network fetches nothing.

mod support;

use xui_webd_client::browse::{base_url, Browse, Marker, DEFAULT_BASE};
use xui_webd_client::transport::ScopedTransport;

use support::full::web;
use support::web::BASE;
use support::TestTransport;

const SIZE: (u32, u32) = (320, 200);
const TICK_MS: u64 = 16;

/// Run `browse` until it says `DONE` (or a long time passed); every marker, and `check` called
/// after each tick with the markers so far.
fn run<T: ScopedTransport>(
    browse: &mut Browse<T>,
    mut check: impl FnMut(&[Marker]),
) -> Vec<Marker> {
    let mut markers = Vec::new();
    let mut now = 1;
    while !browse.done() && now < 1_000_000 {
        let fresh = browse.tick(SIZE, now);
        if !fresh.is_empty() {
            markers.extend(fresh);
            check(&markers);
        }
        now += TICK_MS;
    }
    markers
}

fn detail<'a>(markers: &'a [Marker], step: &str) -> &'a str {
    &markers
        .iter()
        .find(|m| m.step == step)
        .unwrap_or_else(|| panic!("no {step} in {markers:#?}"))
        .detail
}

#[test]
fn every_step_passes_in_order_against_the_service() {
    let web = web();
    let world = web.world.clone();
    let mut browse = Browse::new(move || TestTransport::full(&world, 9), BASE);
    let fake = web.web.clone();
    let blocked = format!("{BASE}/blocked.png");
    let markers = run(&mut browse, |markers| {
        let last = markers.last().unwrap();
        let fetched = fake.requests().contains(&blocked);
        // The blocked picture is not fetched before the reload that bypasses the blocker.
        if last.step == "BLOCKED" {
            assert!(!fetched, "blocked.png was fetched while the list applied");
        }
        if last.step == "UNBLOCKED" {
            assert!(fetched, "the bypassing reload fetched blocked.png");
        }
    });
    let failed: Vec<String> = markers
        .iter()
        .filter(|m| !m.pass)
        .map(Marker::text)
        .collect();
    assert!(failed.is_empty(), "{failed:#?}");
    let steps: Vec<&str> = markers.iter().map(|m| m.step).collect();
    assert_eq!(
        steps,
        [
            "VIEW",
            "BLOCKLIST",
            "BLOCKED",
            "UNBLOCKED",
            "NAV",
            "BACK",
            "FORWARD",
            "COOKIE_A",
            "COOKIE_B",
            "COOKIE_A2",
            "DOWNLOAD",
            "DIALOG",
            "POPUP",
            "HELD",
            "SCOPE",
            "DELTA",
            "LATENCY",
            "DONE"
        ]
    );
    assert!(detail(&markers, "VIEW").starts_with("id="));
    assert_eq!(detail(&markers, "BLOCKLIST"), "list=test status=ready");
    assert_eq!(
        detail(&markers, "NAV"),
        format!("index={BASE}/ page2={BASE}/page2")
    );
    assert_eq!(detail(&markers, "COOKIE_A"), "title=cookie=webd=browser");
    assert_eq!(
        detail(&markers, "COOKIE_B"),
        "first=cookie= second=cookie=webd=own"
    );
    assert_eq!(detail(&markers, "COOKIE_A2"), "title=cookie=webd=browser");
    assert_eq!(detail(&markers, "DOWNLOAD"), "name=file.bin bytes=4096");
    assert_eq!(
        detail(&markers, "DIALOG"),
        "dialogs=2 kinds=alert,confirm title=confirm=true"
    );
    assert_eq!(
        detail(&markers, "POPUP"),
        format!("url={BASE}/popped gesture=false")
    );
    assert_eq!(
        detail(&markers, "HELD"),
        "kind=other gesture=false url=mailto:test@example.com"
    );
    assert_eq!(
        detail(&markers, "SCOPE"),
        "web=eacces load=eacces file=eacces html=finished"
    );
    assert_eq!(detail(&markers, "DELTA"), "dy=-120 rects=1");
    let latency = detail(&markers, "LATENCY");
    assert!(latency.starts_with("load_ms="), "{latency}");
    assert!(latency.contains("n=20 lost=0") && latency.contains("n=10 lost=0"));
    assert_eq!(detail(&markers, "DONE"), "steps=17");
    // The page given to the view without a network never reached the server.
    let requests = web.web.requests();
    assert!(
        !requests.iter().any(|r| r.ends_with("/nonet.png")),
        "{requests:?}"
    );
    // B, C and D were ended; A stays on show.
    assert_eq!(web.world.borrow().service.view_count(), 1);
    let picture = browse.picture().unwrap();
    assert_eq!((picture.width(), picture.height()), SIZE);
    // The marker lines have the shape the harness reads.
    assert_eq!(
        markers[0].text(),
        format!("VIEW:PASS:{}", markers[0].detail)
    );
}

#[test]
fn a_webd_that_never_comes_ends_the_run_with_a_failure() {
    let web = web();
    web.world.borrow_mut().down = true;
    let world = web.world.clone();
    let mut browse = Browse::new(move || TestTransport::full(&world, 9), BASE);
    let markers = run(&mut browse, |_| {});
    let texts: Vec<String> = markers.iter().map(Marker::text).collect();
    assert_eq!(texts.len(), 2, "{texts:?}");
    assert!(texts[0].starts_with("VIEW:FAIL:"));
    assert_eq!(texts[1], "DONE:FAIL:steps=1 failed=VIEW");
}

#[test]
fn a_view_lost_mid_run_fails_the_steps_that_need_it_and_the_rest_run() {
    let web = web();
    let world = web.world.clone();
    let mut browse = Browse::new(move || TestTransport::full(&world, 9), BASE);
    let queue = web.world.borrow().queue.clone();
    let mut killed = false;
    let markers = run(&mut browse, |markers| {
        if !killed && markers.last().unwrap().step == "COOKIE_A" {
            killed = true;
            let id: u64 = markers[0].detail["id=".len()..]
                .split(' ')
                .next()
                .unwrap()
                .parse()
                .unwrap();
            xui_webd::engine::EngineEvents::view_lost(&*queue, id, xui_webd::engine::LOST_CRASHED);
        }
    });
    let verdict = |step: &str| markers.iter().find(|m| m.step == step).map(|m| m.pass);
    assert_eq!(verdict("COOKIE_B"), Some(true), "B does not need A");
    assert_eq!(verdict("SCOPE"), Some(true));
    assert_eq!(verdict("DOWNLOAD"), Some(false));
    assert!(detail(&markers, "DOWNLOAD").starts_with("view A is lost"));
    assert!(!markers.last().unwrap().pass);
}

#[test]
fn the_base_url_comes_from_the_first_http_argument() {
    let args = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(base_url(args(&["--client"])), DEFAULT_BASE);
    assert_eq!(
        base_url(args(&["--client", "http://10.0.2.2:9000/"])),
        "http://10.0.2.2:9000"
    );
    assert_eq!(base_url(args(&["https://x", "ftp://y"])), DEFAULT_BASE);
}
