//! The window built offscreen against a scripted Fireworks: consent, the key
//! page, a streamed Markdown reply, Stop, a refused key, scrolling and the
//! link and copy actions, in both themes. Snapshots land in
//! `target/snapshots/agent-*.png` for a human to look at.

mod support;

use std::time::Duration;

use lazyagent_core::Secret;
use lazyagent_ui::{Msg, Report, ViewAction};
use support::*;
use xui_core::widget::TaskDialogAction;
use xui_core::Theme;

const REPLY: &str = "## What a DHCP lease is\n\nA **lease** is the address a server lends you for a while. See [the RFC](https://www.rfc-editor.org/rfc/rfc2131) for details.\n\n- the client asks\n- the server offers\n- the client renews at half the time\n\n> Renewal is quiet when it works.\n\n```sh\nip addr show dev eth0\n```\n\nThat is all.";

fn reply_events(text: &str) -> Vec<Step> {
    let mut steps = vec![sse_head()];
    // Several chunks, so the reply really streams.
    let mut at = 0;
    while at < text.len() {
        let mut end = (at + 24).min(text.len());
        while !text.is_char_boundary(end) {
            end += 1;
        }
        steps.push(event(&text_delta(&text[at..end])));
        at = end;
    }
    steps.extend([
        event(&finish("stop")),
        event(&usage(40, 90, 0)),
        Step::Send(chunk("data: [DONE]\n\n")),
        end(),
    ]);
    steps
}

fn chat_server(pki: &Pki) -> Server {
    serve(pki, |n, _| {
        if n == 0 {
            reply_events(REPLY)
        } else {
            answer_of("\"DHCP leases\"")
        }
    })
}

#[test]
fn the_consent_and_key_pages_render_in_both_themes() {
    watchdog(|| {
        let pki = Pki::new();
        let server = chat_server(&pki);
        for (theme, tag) in [(Theme::light(), "light"), (Theme::dark(), "dark")] {
            let shot = render(
                theme,
                &pki,
                &server,
                temp_dir("consent"),
                |stage, heard_| {
                    tick_until(stage, Duration::from_secs(10), || {
                        heard(heard_, |r| *r == Report::Ready)
                    });
                },
            );
            assert!(shot.reports.contains(&Report::Ready), "{tag}");
            save(&shot.image, &format!("agent-consent-{tag}.png"));
        }
        let shot = render(
            Theme::light(),
            &pki,
            &server,
            temp_dir("key"),
            |stage, heard_| {
                sign_in(stage, heard_);
                assert!(heard(heard_, |r| *r == Report::Consented));
            },
        );
        save(&shot.image, "agent-key-light.png");
    });
}

#[test]
fn a_reply_streams_as_markdown_and_both_themes_draw_it() {
    watchdog(|| {
        let pki = Pki::new();
        for (theme, tag) in [(Theme::light(), "light"), (Theme::dark(), "dark")] {
            let server = chat_server(&pki);
            let shot = render(theme, &pki, &server, temp_dir("chat"), |stage, heard_| {
                sign_in(stage, heard_);
                stage.emit(Msg::KeyEntered(Secret::new(KEY)));
                stage.emit(Msg::SendText("What is a DHCP lease?".into()));
                tick_until(stage, Duration::from_secs(30), || {
                    heard(heard_, |r| matches!(r, Report::Done { .. }))
                });
                // The finished conversation is shaped and drawn.
                tick_until(stage, Duration::from_millis(1500), || false);
            });
            assert!(shot.reports.contains(&Report::Consented), "{tag}");
            assert!(shot.reports.contains(&Report::KeySet), "{tag}");
            assert!(
                shot.reports
                    .iter()
                    .any(|r| matches!(r, Report::FirstChunk(_))),
                "{tag}"
            );
            let done = shot.reports.iter().find_map(|r| match r {
                Report::Done {
                    prompt, completion, ..
                } => Some((*prompt, *completion)),
                _ => None,
            });
            assert_eq!(done, Some((40, 90)), "{tag}");
            assert!(
                shot.reports.contains(&Report::Rows(2)),
                "{tag}: {:?}",
                shot.reports
            );
            let requests = server.seen.lock().unwrap();
            assert_eq!(
                requests[0].header("authorization"),
                Some(format!("Bearer {KEY}").as_str())
            );
            let drawn = ink(&shot.image);
            assert!(drawn > 3000, "{tag}: only {drawn} pixels of text and fills");
            save(&shot.image, &format!("agent-chat-{tag}.png"));
        }
    });
}

#[test]
fn a_turn_that_fails_before_it_starts_frees_the_window() {
    watchdog(|| {
        let pki = Pki::new();
        let server = chat_server(&pki);
        // A file where the app's folder should be: nothing can be stored.
        let dir = temp_dir("nostore");
        std::fs::create_dir_all(dir.parent().unwrap()).unwrap();
        std::fs::write(&dir, b"not a folder").unwrap();
        let shot = render(Theme::light(), &pki, &server, dir, |stage, heard_| {
            sign_in(stage, heard_);
            stage.emit(Msg::KeyEntered(Secret::new(KEY)));
            stage.emit(Msg::SendText("first".into()));
            tick_until(stage, Duration::from_secs(10), || {
                heard(heard_, |r| matches!(r, Report::Failed(_)))
            });
            tick_until(stage, Duration::from_millis(300), || false);
            // Busy would ignore this one.
            stage.emit(Msg::SendText("second".into()));
            tick_until(stage, Duration::from_secs(10), || {
                heard_
                    .borrow()
                    .iter()
                    .filter(|r| matches!(r, Report::Failed(_)))
                    .count()
                    == 2
            });
        });
        let sent = shot
            .reports
            .iter()
            .filter(|r| matches!(r, Report::Sent(_)))
            .count();
        assert_eq!(sent, 2, "{:?}", shot.reports);
        assert_eq!(server.requests(), 0, "nothing was sent to the model");
    });
}

#[test]
fn stop_keeps_the_partial_answer_and_marks_it() {
    watchdog(|| {
        let pki = Pki::new();
        let server = serve(&pki, |_, _| {
            vec![
                sse_head(),
                event(&text_delta("Half an answer, ")),
                Step::Sleep(Duration::from_secs(30)),
            ]
        });
        let shot = render(
            Theme::light(),
            &pki,
            &server,
            temp_dir("stop"),
            |stage, heard_| {
                sign_in(stage, heard_);
                stage.emit(Msg::KeyEntered(Secret::new(KEY)));
                stage.emit(Msg::SendText("Tell me a long story".into()));
                tick_until(stage, Duration::from_secs(15), || {
                    heard(heard_, |r| matches!(r, Report::FirstChunk(_)))
                });
                stage.emit(Msg::Stop);
                tick_until(stage, Duration::from_secs(10), || {
                    heard(heard_, |r| matches!(r, Report::Failed(s) if s == "stopped"))
                });
                tick_until(stage, Duration::from_millis(1200), || false);
            },
        );
        assert!(shot.reports.contains(&Report::Failed("stopped".into())));
        save(&shot.image, "agent-stopped-light.png");
    });
}

#[test]
fn a_refused_key_goes_back_to_the_key_page() {
    watchdog(|| {
        let pki = Pki::new();
        let server = serve(&pki, |_, _| {
            status(
                401,
                &[],
                r#"{"error":{"message":"The API key you provided is invalid."}}"#,
            )
        });
        let shot = render(
            Theme::light(),
            &pki,
            &server,
            temp_dir("401"),
            |stage, heard_| {
                sign_in(stage, heard_);
                stage.emit(Msg::KeyEntered(Secret::new("wrong-key")));
                stage.emit(Msg::SendText("hello".into()));
                tick_until(stage, Duration::from_secs(15), || {
                    heard(heard_, |r| matches!(r, Report::Failed(s) if s == "401"))
                });
                tick_until(stage, Duration::from_millis(800), || false);
            },
        );
        assert!(shot.reports.contains(&Report::Failed("401".into())));
        let log = shot.log.0.lock().unwrap().join("\n");
        assert!(!log.contains("wrong-key"));
        save(&shot.image, "agent-key-refused-light.png");
    });
}

#[test]
fn a_long_conversation_scrolls_and_follows_the_end() {
    watchdog(|| {
        let pki = Pki::new();
        let long: String = (1..=40)
            .map(|n| format!("Paragraph number {n} says something useful about leases.\n\n"))
            .collect();
        let server = serve(&pki, move |n, _| {
            if n == 0 {
                reply_events(&long)
            } else {
                answer_of("\"Leases\"")
            }
        });
        let shot = render(
            Theme::light(),
            &pki,
            &server,
            temp_dir("long"),
            |stage, heard_| {
                sign_in(stage, heard_);
                stage.emit(Msg::KeyEntered(Secret::new(KEY)));
                stage.emit(Msg::SendText("Explain leases at length".into()));
                tick_until(stage, Duration::from_secs(30), || {
                    heard(heard_, |r| matches!(r, Report::Done { .. }))
                });
                tick_until(stage, Duration::from_millis(1500), || false);
                assert!(
                    !heard(heard_, |r| *r == Report::Following(false)),
                    "a finished reply stays at the end"
                );
                wheel(stage, 15);
                tick_until(stage, Duration::from_millis(1200), || false);
            },
        );
        assert!(
            shot.reports.contains(&Report::Following(false)),
            "scrolling up stops following"
        );
        save(&shot.image, "agent-scrolled-light.png");
    });
}

#[test]
fn links_ask_first_and_code_copies() {
    watchdog(|| {
        let pki = Pki::new();
        let server = chat_server(&pki);
        let shot = render(
            Theme::light(),
            &pki,
            &server,
            temp_dir("actions"),
            |stage, heard_| {
                sign_in(stage, heard_);
                stage.emit(Msg::KeyEntered(Secret::new(KEY)));
                stage.emit(Msg::SendText("What is a DHCP lease?".into()));
                tick_until(stage, Duration::from_secs(30), || {
                    heard(heard_, |r| matches!(r, Report::Done { .. }))
                });
                let url = "https://www.rfc-editor.org/rfc/rfc2131".to_string();
                stage.emit(Msg::Transcript(ViewAction::Link(url.clone())));
                tick_until(stage, Duration::from_millis(500), || false);
                assert!(heard(heard_, |r| *r == Report::LinkAsked(url.clone())));
                // Nothing opened before the answer; Cancel opens nothing.
                stage.emit(Msg::LinkChoice(TaskDialogAction::Cancel));
                stage.emit(Msg::Transcript(ViewAction::Link(url.clone())));
                stage.emit(Msg::LinkChoice(TaskDialogAction::Command(0)));
                stage.emit(Msg::Transcript(ViewAction::Copy(
                    "ip addr show dev eth0".into(),
                )));
                tick_until(stage, Duration::from_millis(500), || false);
            },
        );
        let opened: Vec<String> = shot
            .log
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|l| l.starts_with("OPEN:"))
            .cloned()
            .collect();
        assert_eq!(
            opened,
            ["OPEN:https://www.rfc-editor.org/rfc/rfc2131"],
            "only the confirmed link opened"
        );
        assert!(shot.reports.contains(&Report::Copied));
    });
}
