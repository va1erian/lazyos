use super::*;

fn facts() -> Facts {
    Facts {
        version: "0.1.0".into(),
        tabs: 1,
        platform: "linux x86_64".into(),
        page_url: "https://user:secret@Example.COM:8443/a/b?token=abc#frag".into(),
        trace: vec!["WEBPLUS:UP:PASS".into(), "WEBPLUS:HANG:SHOW".into()],
        events: vec!["+3s: page not responding".into()],
    }
}

fn typed() -> Typed<'static> {
    Typed {
        happened: "The page froze",
        expected: "It scrolls",
        steps: "1. open\n2. scroll",
    }
}

#[test]
fn hosts_and_addresses_leave_out_user_port_query_and_fragment() {
    let url = "https://user:secret@Example.COM:8443/a/b?token=abc#frag";
    assert_eq!(host_of(url).as_deref(), Some("example.com"));
    assert_eq!(
        address_of(url).as_deref(),
        Some("https://example.com:8443/a/b")
    );
    assert_eq!(host_of("http://[::1]:8080/x").as_deref(), Some("[::1]"));
    assert_eq!(
        address_of("http://a.test").as_deref(),
        Some("http://a.test")
    );
    assert_eq!(
        address_of("http://a.test?q=1").as_deref(),
        Some("http://a.test")
    );
    assert_eq!(
        address_of("http://a.test#top").as_deref(),
        Some("http://a.test")
    );
    assert_eq!(host_of("HTTP://A.TEST/").as_deref(), Some("a.test"));
}

#[test]
fn only_web_pages_have_a_host_or_an_address() {
    for url in [
        "",
        "data:text/html,<h1>history</h1>",
        "about:blank",
        "file:///home/user/secret.html",
        "mailto:a@b.test",
        "javascript:alert(1)",
        "http://",
        "https://@/",
        "not a url",
    ] {
        assert_eq!(host_of(url), None, "{url:?}");
        assert_eq!(address_of(url), None, "{url:?}");
    }
}

#[test]
fn by_default_only_the_version_is_included() {
    let md = markdown(typed(), Include::default(), &facts());
    assert!(md.starts_with("# LazyWeb+ issue report"));
    assert!(md.contains("## What happened\n\nThe page froze"));
    assert!(md.contains("## What I expected\n\nIt scrolls"));
    assert!(md.contains("1. open\n2. scroll"));
    assert!(md.contains("- LazyWeb+ 0.1.0\n- Tabs: 1\n- Platform: linux x86_64"));
    for absent in [
        "example.com",
        "secret",
        "token",
        "UP:PASS",
        "page not responding",
    ] {
        assert!(!md.contains(absent), "{absent}");
    }
}

#[test]
fn the_host_and_the_address_are_separate_choices() {
    let host_only = markdown(
        typed(),
        Include {
            host: true,
            ..Include::default()
        },
        &facts(),
    );
    assert!(host_only.contains("- Host: example.com"));
    assert!(!host_only.contains("/a/b") && !host_only.contains("8443"));

    let both = markdown(
        typed(),
        Include {
            host: true,
            address: true,
            ..Include::default()
        },
        &facts(),
    );
    assert!(both.contains("https://example.com:8443/a/b"));
    for never in ["secret", "user:", "token", "#frag", "=abc"] {
        assert!(!both.contains(never), "{never}");
    }

    let on_our_page = Facts {
        page_url: String::new(),
        ..facts()
    };
    let md = markdown(
        typed(),
        Include {
            host: true,
            ..Include::default()
        },
        &on_our_page,
    );
    assert!(md.contains("(no web page was on show)"));
}

#[test]
fn the_trace_and_the_events_are_included_when_ticked() {
    let md = markdown(
        typed(),
        Include {
            trace: true,
            events: true,
            ..Include::default()
        },
        &facts(),
    );
    assert!(md.contains("## Window trace (last 2 lines)"));
    assert!(md.contains("```text\nWEBPLUS:UP:PASS\nWEBPLUS:HANG:SHOW\n```"));
    assert!(md.contains("## Engine events\n\n- +3s: page not responding"));
    let none = markdown(
        typed(),
        Include {
            events: true,
            ..Include::default()
        },
        &Facts {
            events: vec![],
            ..facts()
        },
    );
    assert!(none.contains("- none this run"));
}

#[test]
fn only_the_last_200_trace_lines_go_in_and_none_can_end_the_fence() {
    let lines: Vec<String> = (0..500)
        .map(|n| format!("WEBPLUS:LINE:{n} ``` ````` \u{202E}"))
        .collect();
    let md = markdown(
        typed(),
        Include {
            trace: true,
            ..Include::default()
        },
        &Facts {
            trace: lines,
            ..facts()
        },
    );
    assert!(md.contains("last 200 lines"));
    assert!(md.contains("LINE:499") && md.contains("LINE:300 ") && !md.contains("LINE:299 "));
    // The fence is longer than the longest run of backticks in the lines.
    assert!(md.contains("``````text\n"));
    assert!(!md.contains('\u{202E}'));
}

#[test]
fn what_is_typed_is_cleaned_and_capped() {
    let hostile = format!("a\u{202E}b\u{0}c\r\nd\te{}", "x".repeat(MAX_FIELD * 2));
    let md = markdown(
        Typed {
            happened: &hostile,
            expected: "",
            steps: "   \n  ",
        },
        Include::default(),
        &facts(),
    );
    assert!(md.contains("abc\nd ex"));
    assert!(!md.contains('\u{202E}') && !md.contains('\u{0}') && !md.contains('\r'));
    let happened = md.split("## What happened\n\n").nth(1).unwrap();
    let happened = happened.split("\n\n## ").next().unwrap();
    assert_eq!(happened.chars().count(), MAX_FIELD);
    assert_eq!(md.matches("(nothing written)").count(), 2);
}

#[test]
fn the_whole_report_never_passes_the_cap() {
    // Four-byte characters in every line fill the cap long before the line count does.
    let wide = "\u{1F600}".repeat(ring::LINE_MAX);
    let md = markdown(
        Typed {
            happened: &"\u{1F600}".repeat(MAX_FIELD),
            expected: &"\u{1F600}".repeat(MAX_FIELD),
            steps: &"\u{1F600}".repeat(MAX_FIELD),
        },
        Include {
            build: true,
            host: true,
            address: true,
            trace: true,
            events: true,
        },
        &Facts {
            trace: vec![wide; 5_000],
            events: vec!["e".repeat(500); 500],
            ..facts()
        },
    );
    assert!(md.len() <= MAX_TOTAL, "{} bytes", md.len());
    assert!(std::str::from_utf8(md.as_bytes()).is_ok());

    // A cap that bites says so.
    let mut sections = vec![String::from("# t\n")];
    sections.push("x".repeat(MAX_TOTAL * 2));
    sections.push(String::from("never"));
    let out = assemble(&sections);
    assert!(out.len() <= MAX_TOTAL);
    assert!(out.ends_with("(report cut: it was too long)\n"));
    assert!(!out.contains("never"));
}

#[test]
fn previews_show_each_part_in_one_short_line() {
    let p = previews(&facts());
    assert_eq!(p[0], "LazyWeb+ 0.1.0, 1 tab, linux x86_64");
    assert_eq!(p[1], "example.com");
    assert_eq!(p[2], "https://example.com:8443/a/b");
    assert_eq!(p[3], "2 lines, the newest: WEBPLUS:HANG:SHOW");
    assert_eq!(p[4], "1 event: +3s: page not responding");
    let blank = previews(&Facts::default());
    assert_eq!(blank[1], "(no web page on show)");
    assert_eq!(blank[3], "no lines yet");
    assert!(p.iter().all(|line| line.chars().count() <= 90));
}

#[test]
fn saving_never_replaces_a_file_and_makes_the_folder() {
    let dir = std::env::temp_dir().join(format!("lazywebplus-report-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let folder = dir.join("Downloads");
    let first = save(&folder, 1_700_000_000, "one").unwrap();
    let second = save(&folder, 1_700_000_000, "two").unwrap();
    assert_ne!(first, second);
    assert_eq!(std::fs::read_to_string(&first).unwrap(), "one");
    assert_eq!(std::fs::read_to_string(&second).unwrap(), "two");
    assert_eq!(
        first.file_name().unwrap().to_str().unwrap(),
        "lazywebplus-report-1700000000.md"
    );
    assert!(second
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("lazywebplus-report-1700000000 (1)"));
    // A folder that cannot be made is an error, not a panic.
    let blocked = folder.join("lazywebplus-report-x.md");
    std::fs::write(&blocked, "file").unwrap();
    assert!(save(&blocked, 1, "x").is_err());
    let _ = std::fs::remove_dir_all(&dir);
}
