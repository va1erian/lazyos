//! Help > Report an issue, driven offscreen: the form opens over the page, what is typed and
//! ticked reaches the saved Markdown, and nothing of the page but what was ticked is in it.
//! (Copy Markdown is not driven here: the offscreen backend shares the real clipboard.)
//! Snapshots go to `xui-app/target/snapshots/lazywebplus-report-*.png`.

mod support;

use std::fs;

use support::*;

use xui_core::backend::Event as UiEvent;
use xui_webkit::events::Event;
use xui_webplus::app::Msg;
use xui_webplus::chrome::Command;
use xui_webplus::report::form::ReportMsg;

fn saved(rig: &Rig) -> String {
    let mut files: Vec<_> = fs::read_dir(rig.downloads())
        .expect("the Downloads folder was made")
        .map(|e| e.unwrap().path())
        .collect();
    files.sort();
    assert_eq!(files.len(), 1, "{files:?}");
    fs::read_to_string(&files[0]).unwrap()
}

fn type_text(stage: &xui_canvas::snapshot::Stage<'_, Msg>, text: &str) {
    for c in text.chars() {
        stage.inject(UiEvent::Char(c));
    }
}

#[test]
fn the_form_opens_and_a_default_report_has_the_version_and_nothing_of_the_page() {
    let (image, _) = drive(
        "report_default",
        Some("https://example.com/a?q=1"),
        |stage, rig| {
            rig.state(
                stage,
                loaded("https://example.com/a?q=1", "A Secret Title", 1),
            );
            stage.emit(Msg::Menu(Command::ReportIssue));
            type_text(stage, "it froze");
            stage.emit(Msg::Report(ReportMsg::SaveMarkdown));
            let md = saved(rig);
            assert!(md.contains("## What happened\n\nit froze"), "{md}");
            assert!(md.contains("- LazyWeb+ "), "{md}");
            assert!(md.contains("- Tabs: 1"), "{md}");
            for absent in ["example.com", "Secret", "q=1", "WEBPLUS:"] {
                assert!(!md.contains(absent), "{absent}: {md}");
            }
        },
    );
    save(&image, "lazywebplus-report-form.png");
}

#[test]
fn ticking_the_boxes_adds_the_host_the_trace_and_the_events() {
    let (_, _) = drive(
        "report_ticks",
        Some("https://example.com/a?q=1#f"),
        |stage, rig| {
            rig.state(stage, loaded("https://example.com/a?q=1#f", "Title", 1));
            // The engine says the page stopped answering, then the person answered the bar.
            rig.events(stage, vec![Event::PageResponsive(false)]);
            stage.emit(Msg::Hang(xui_webplus::hang::Answer::Wait));
            stage.emit(Msg::Menu(Command::ReportIssue));
            for tick in ["host", "address", "trace", "events"] {
                let (x, y) = rig.centre(&format!("report_tick_{tick}"));
                stage.click(x, y);
            }
            stage.emit(Msg::Report(ReportMsg::SaveMarkdown));
            let md = saved(rig);
            assert!(md.contains("- Host: example.com"), "{md}");
            assert!(md.contains("https://example.com/a\n"), "{md}");
            assert!(md.contains("## Window trace"), "{md}");
            assert!(md.contains("WEBPLUS:HANG:SHOW"), "{md}");
            assert!(md.contains("page not responding"), "{md}");
            assert!(md.contains("answered: wait"), "{md}");
            for never in ["q=1", "#f", "Title"] {
                assert!(!md.contains(never), "{never}: {md}");
            }
        },
    );
}

#[test]
fn cancel_forgets_what_was_typed() {
    let (_, _) = drive("report_cancel", None, |stage, rig| {
        stage.emit(Msg::Menu(Command::ReportIssue));
        type_text(stage, "private words");
        stage.emit(Msg::Report(ReportMsg::Cancel));
        stage.emit(Msg::Menu(Command::ReportIssue));
        stage.emit(Msg::Report(ReportMsg::SaveMarkdown));
        let md = saved(rig);
        assert!(!md.contains("private words"), "{md}");
        assert!(md.contains("(nothing written)"), "{md}");
    });
}

#[test]
fn a_lost_view_is_an_engine_event_with_only_the_kind_of_reason() {
    let (_, _) = drive("report_lost", Some("https://example.com/"), |stage, rig| {
        rig.state(stage, loaded("https://example.com/", "T", 1));
        rig.state(
            stage,
            xui_webkit::mailbox::PageState {
                crashed: true,
                error: "The page's process crashed".into(),
                ..loaded("https://example.com/", "T", 1)
            },
        );
        stage.emit(Msg::Menu(Command::ReportIssue));
        let (x, y) = rig.centre("report_tick_events");
        stage.click(x, y);
        stage.emit(Msg::Report(ReportMsg::SaveMarkdown));
        let md = saved(rig);
        assert!(md.contains("view lost: crashed"), "{md}");
        assert!(!md.contains("process crashed"), "only the kind: {md}");
    });
}
