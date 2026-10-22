//! Typed text in the address bar: a search, HTTPS first, and the question when a host does not
//! answer over HTTPS. Driven offscreen against a recording engine.

mod support;

use support::*;
use xui_core::backend::Event as UiEvent;
use xui_webkit::engine::Call;
use xui_webkit::events::Event;
use xui_webkit::mailbox::PageState;
use xui_webplus::app::Msg;
use xui_webplus::downgrade;

fn type_address(stage: &xui_canvas::snapshot::Stage<'_, Msg>, text: &str) {
    stage.emit(Msg::FocusAddress);
    for c in text.chars() {
        stage.inject(UiEvent::Char(c));
    }
    stage.emit(Msg::Go);
}

fn loads(calls: &[Call]) -> Vec<String> {
    calls
        .iter()
        .filter_map(|c| match c {
            Call::Load(url) => Some(url.clone()),
            _ => None,
        })
        .collect()
}

fn failed(url: &str, why: &str) -> PageState {
    PageState {
        url: url.into(),
        error: format!("Could not load {url}: {why}"),
        ..PageState::default()
    }
}

#[test]
fn words_are_searched_and_hosts_open_over_https() {
    let (_, calls) = drive("search-typed", Some("about:blank"), |stage, _| {
        type_address(stage, "rust lifetimes");
        type_address(stage, "example.com/a");
        type_address(stage, "?example.com");
    });
    let loads = loads(&calls);
    assert_eq!(
        loads[1..],
        [
            "https://duckduckgo.com/?q=rust%20lifetimes",
            "https://example.com/a",
            "https://duckduckgo.com/?q=example.com",
        ]
    );
}

#[test]
fn a_host_that_does_not_answer_over_https_asks_before_http_and_the_yes_is_remembered() {
    let (image, calls) = drive("search-downgrade", Some("about:blank"), |stage, rig| {
        type_address(stage, "Plain.Example:8080");
        rig.state(
            stage,
            failed("https://Plain.Example:8080/", "Couldn't connect to server"),
        );
        // The question is the browser's own page, built for this host.
        let page = rig.internal.url_of(downgrade::NAME);
        rig.state(stage, loaded(&page, "This site did not answer", 1));
        // The page's button, as the policy hands it to the window; a click for another host does nothing.
        let command = |host: &str| Event::Command {
            url: downgrade::allow_command(host),
        };
        rig.events(stage, vec![command("other.example")]);
        rig.events(stage, vec![command("plain.example:8080")]);
        // Remembered: typing the host again goes straight to HTTP.
        type_address(stage, "plain.example:8080");
    });
    save(&image, "lazywebplus-https-ask.png");
    let loads = loads(&calls);
    let question = loads
        .iter()
        .position(|u| u.starts_with("data:") && u.contains("did not answer"))
        .unwrap_or_else(|| panic!("the question was loaded: {loads:?}"));
    assert_eq!(loads[1], "https://Plain.Example:8080/");
    assert_eq!(question, 2, "{loads:?}");
    assert_eq!(
        loads[3..],
        ["http://Plain.Example:8080/", "http://plain.example:8080/"],
        "{loads:?}"
    );
}

#[test]
fn a_certificate_error_never_asks_to_drop_the_encryption() {
    let (_, calls) = drive("search-cert", Some("about:blank"), |stage, rig| {
        type_address(stage, "bad.example");
        rig.state(
            stage,
            failed(
                "https://bad.example/",
                "SSL peer certificate or SSH remote key was not OK",
            ),
        );
    });
    let loads = loads(&calls);
    assert_eq!(loads, ["about:blank", "https://bad.example/"], "{loads:?}");
}

#[test]
fn a_single_label_host_with_a_path_gets_its_offer_and_the_yes_loads_the_typed_address() {
    let (_, calls) = drive("search-label", Some("about:blank"), |stage, rig| {
        type_address(stage, "intranet/wiki");
        rig.state(
            stage,
            failed("https://intranet/wiki", "Couldn't connect to server"),
        );
        let page = rig.internal.url_of(downgrade::NAME);
        rig.state(stage, loaded(&page, "This site did not answer", 1));
        // The button's command, as the page carries it.
        rig.events(
            stage,
            vec![Event::Command {
                url: downgrade::allow_command("intranet"),
            }],
        );
    });
    let loads = loads(&calls);
    assert_eq!(loads[1], "https://intranet/wiki", "{loads:?}");
    assert_eq!(
        loads.last().map(String::as_str),
        Some("http://intranet/wiki"),
        "{loads:?}"
    );
}

#[test]
fn an_offer_does_not_outlive_the_navigation_that_made_it() {
    let (_, calls) = drive("search-stale", Some("about:blank"), |stage, rig| {
        type_address(stage, "stale.example");
        // Something else starts a navigation (a page's window request), then the old host fails.
        rig.events(
            stage,
            vec![Event::Popup {
                url: "https://other.example/".into(),
            }],
        );
        rig.state(
            stage,
            failed("https://stale.example/", "Couldn't connect to server"),
        );
    });
    let loads = loads(&calls);
    assert!(
        !loads.iter().any(|u| u.starts_with("data:")),
        "no question for a stale offer: {loads:?}"
    );
}
