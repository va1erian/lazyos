use super::*;

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("lazyweb-session-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn a_session_survives_the_file() {
    let session = Session::single(
        "https://example.com/a?b=\"c\"&d=\\e",
        "Tab\u{e9} \"quoted\" \u{1f600}",
    );
    let text = session.to_json();
    assert!(text.starts_with("{\"version\":1,\"windows\":[{\"tabs\":[{\"url\":"));
    assert_eq!(Session::parse(&text), Some(session));
}

#[test]
fn the_documented_format_parses() {
    let text = r#" { "version": 1, "windows": [ { "tabs": [
        {"url": "https://a.example/", "title": "A"},
        {"url": "http://b.example/", "title": "B"} ], "active": 1 } ] } "#;
    let session = Session::parse(text).unwrap();
    assert_eq!(session.windows[0].active, 1);
    assert_eq!(session.page().unwrap().url, "http://b.example/");
}

#[test]
fn only_web_pages_come_back_and_the_active_tab_follows_the_ones_dropped() {
    let text = r#"{"version":1,"windows":[{"tabs":[
        {"url":"javascript:alert(1)","title":"x"},
        {"url":"data:text/html,hi","title":"own page"},
        {"url":"file:///etc/passwd","title":"file"},
        {"url":"https://ok.example/","title":"ok"},
        {"url":"https://two.example/"}],"active":3}]}"#;
    let session = Session::parse(text).unwrap();
    let urls: Vec<_> = session.windows[0]
        .tabs
        .iter()
        .map(|t| t.url.as_str())
        .collect();
    assert_eq!(urls, ["https://ok.example/", "https://two.example/"]);
    assert_eq!(session.page().unwrap().url, "https://two.example/");
    assert_eq!(session.windows[0].tabs[1].title, "");
}

#[test]
fn hostile_or_broken_files_give_nothing_and_never_panic() {
    for text in [
        "",
        "null",
        "[]",
        "{}",
        r#"{"version":2,"windows":[]}"#,
        r#"{"version":"1","windows":[]}"#,
        r#"{"version":1}"#,
        r#"{"version":1,"windows":{}}"#,
        r#"{"version":1,"windows":[{"tabs":5}]}"#,
        r#"{"version":1,"windows":[]} trailing"#,
        r#"{"version":1,"windows":["#,
        "{\"version\":1,\"windows\":[{\"tabs\":[{\"url\":\"https://a/\u{0}\"}]}]}",
        r#"{"version":1,"windows":[{"tabs":[{"url":"\ud800"}]}]}"#,
        r#"{"version":1,"windows":[{"tabs":[{"url":"\q"}]}]}"#,
    ] {
        match Session::parse(text) {
            None => {}
            Some(session) => assert!(session.windows.is_empty(), "{text:?} gave {session:?}"),
        }
    }
    // Nesting a file deeper than the format ever is.
    let deep = format!("{}1{}", "[".repeat(10_000), "]".repeat(10_000));
    assert_eq!(Session::parse(&deep), None);
}

#[test]
fn urls_titles_tabs_and_windows_are_bounded() {
    let long = format!("https://a.example/{}", "x".repeat(MAX_URL));
    // A control character escaped in the file (a raw one makes the file invalid).
    let title = "t\\u0007".repeat(MAX_TITLE);
    let text = format!(
        r#"{{"version":1,"windows":[{{"tabs":[{{"url":"{long}"}},{{"url":"https://a.example/","title":"{title}"}}],"active":99}}]}}"#
    );
    let session = Session::parse(&text).unwrap();
    let tab = &session.windows[0].tabs[0];
    assert_eq!(
        tab.url, "https://a.example/",
        "the over-long address was dropped"
    );
    assert_eq!(tab.title.chars().count(), MAX_TITLE);
    assert!(!tab.title.contains('\u{7}'));
    assert_eq!(
        session.windows[0].active, 0,
        "an out-of-range active tab is clamped"
    );

    let tabs = vec![r#"{"url":"https://a.example/"}"#; MAX_TABS + 20].join(",");
    let windows = vec![format!(r#"{{"tabs":[{tabs}]}}"#); MAX_WINDOWS + 3].join(",");
    let session = Session::parse(&format!(r#"{{"version":1,"windows":[{windows}]}}"#)).unwrap();
    assert_eq!(session.windows.len(), MAX_WINDOWS);
    assert_eq!(session.windows[0].tabs.len(), MAX_TABS);
}

#[test]
fn the_marker_says_the_last_run_did_not_end_and_a_clean_end_clears_it() {
    let dir = scratch("marker");
    assert!(!begin(&dir), "the first run left nothing");
    assert!(dir.join(RUNNING).is_file());
    // The process died here: the next start finds the marker.
    assert!(begin(&dir));
    end(&dir);
    assert!(!dir.join(RUNNING).exists());
    assert!(!begin(&dir), "after a clean end the next start is normal");
    end(&dir);
    end(&dir);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn saving_replaces_the_file_whole_and_a_huge_one_is_not_read_whole() {
    let dir = scratch("save");
    assert_eq!(load(&dir), None);
    save(&dir, &Session::single("https://one.example/", "One")).unwrap();
    save(&dir, &Session::single("https://two.example/", "Two")).unwrap();
    assert!(!dir.join("session.json.tmp").exists());
    assert_eq!(
        load(&dir).unwrap().page().unwrap().url,
        "https://two.example/"
    );
    // A file of garbage far over the cap is read only as far as the cap.
    fs::write(dir.join(FILE), vec![b'['; (MAX_FILE * 4) as usize]).unwrap();
    assert_eq!(load(&dir), None);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn writes_are_at_most_one_a_second_and_the_newest_page_wins() {
    let tab = |n: u32| Tab {
        url: format!("https://p{n}.example/"),
        title: String::new(),
    };
    let mut throttle = Throttle::default();
    assert_eq!(
        throttle.note(tab(1), 10_000),
        Some(tab(1)),
        "the first goes at once"
    );
    assert_eq!(throttle.note(tab(2), 10_100), None);
    assert_eq!(throttle.note(tab(3), 10_200), None);
    assert_eq!(throttle.due(10_900), None);
    assert_eq!(
        throttle.due(11_000),
        Some(tab(3)),
        "the newest, once the second is up"
    );
    assert_eq!(throttle.due(12_000), None, "nothing left waiting");
    assert_eq!(
        throttle.note(tab(3), 12_500),
        None,
        "the page on disk is not written again"
    );
    // Going back to the page on disk cancels a write that was waiting.
    assert_eq!(throttle.note(tab(5), 13_000), Some(tab(5)));
    assert_eq!(throttle.note(tab(6), 13_100), None);
    assert_eq!(throttle.note(tab(5), 13_200), None);
    assert_eq!(throttle.due(15_000), None);
}
