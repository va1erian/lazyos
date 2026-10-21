//! The Gallery's examples, each run against the in-memory fabric and judged
//! by its own `// expect:` lines. A script that waits for events is fed by
//! the test's scenario and ends when the fabric has nothing more to say.

use std::collections::HashSet;

use crate::gallery::{self, check_expectations, glob_match, Example, Need, Section};
use crate::testkit::services::gallery_world;
use crate::testkit::Lab;

fn example(id: &str) -> Example {
    gallery::find(id).unwrap_or_else(|| panic!("no example {id}"))
}

#[test]
fn the_gallery_has_every_example_but_the_toy_interface() {
    let ids = gallery::ids();
    assert_eq!(ids.len(), 15);
    assert!(gallery::problems().is_empty(), "{:?}", gallery::problems());
    assert_eq!(gallery::examples().len(), 15);
    // 14 is not shipped: it needs a new .midl.
    assert!(ids.iter().all(|id| !id.starts_with("14")));
    let numbers: Vec<_> = gallery::examples()
        .iter()
        .map(|e| e.number().to_string())
        .collect();
    assert_eq!(
        numbers,
        [
            "01", "02", "03", "04", "05", "06", "07", "08", "09", "10", "11", "11b", "12", "12b",
            "13"
        ]
    );
}

#[test]
fn the_embedded_files_are_the_files_in_the_package_directory() {
    let dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/playground/examples");
    let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(".rhai"))
        .map(|n| n.trim_end_matches(".rhai").to_string())
        .collect();
    on_disk.sort();
    let mut embedded: Vec<String> = gallery::ids().iter().map(|s| s.to_string()).collect();
    embedded.sort();
    assert_eq!(
        on_disk, embedded,
        "an example file is missing from gallery.rs or the other way round"
    );
}

#[test]
fn headers_are_complete_and_consistent() {
    let all = gallery::examples();
    let mut titles = HashSet::new();
    for ex in &all {
        assert!(
            titles.insert(ex.title.clone()),
            "duplicate title {}",
            ex.title
        );
        assert!(!ex.expect.is_empty());
        assert!(ex.tags.len() >= 2, "{}: a section and a topic", ex.id);
        assert!(ex.source.starts_with("// title: "), "{}", ex.id);
        if let Some(after) = &ex.after {
            let first = all
                .iter()
                .find(|e| e.id == *after)
                .unwrap_or_else(|| panic!("{} follows {after}", ex.id));
            assert!(ex.needs.contains(&Need::SecondRun) && first.needs.contains(&Need::SecondRun));
        }
        assert!(!ex.source.contains("#!"), "{}", ex.id);
    }
    let sections: Vec<_> = all.iter().map(|e| e.section).collect();
    assert_eq!(sections[0], Section::Explore);
    assert_eq!(
        sections.iter().filter(|s| **s == Section::Explore).count(),
        3
    );
    assert_eq!(
        sections.iter().filter(|s| **s == Section::Listen).count(),
        4
    );
    assert_eq!(sections.iter().filter(|s| **s == Section::Do).count(), 5);
    assert_eq!(sections.iter().filter(|s| **s == Section::Build).count(), 3);
    assert_eq!(example("10_volume_wave").needs, [Need::SoundCard]);
    assert!(example("11b_service_watchdog_trigger")
        .needs
        .contains(&Need::AdminLogin));
}

#[test]
fn a_bad_header_is_reported() {
    for (text, want) in [
        ("// tags: do\n// needs: none\n// expect: x\n", "no title"),
        (
            "// title: t\n// tags: nope\n// needs: none\n// expect: x\n",
            "first tag",
        ),
        ("// title: t\n// tags: do\n// expect: x\n", "no needs"),
        (
            "// title: t\n// tags: do\n// needs: warp drive\n// expect: x\n",
            "unknown need",
        ),
        ("// title: t\n// tags: do\n// needs: none\n", "no expect"),
    ] {
        let err = gallery::parse("x", text).unwrap_err();
        assert!(err.message.contains(want), "{text:?}: {err}");
    }
}

#[test]
fn examples_never_write_system_settings_or_touch_credentials() {
    for ex in gallery::examples() {
        for banned in [
            "confd::set(",
            "sys::keyd",
            "sys::accounts",
            "os.lazy.keyd",
            "os.lazy.accounts",
        ] {
            assert!(!ex.source.contains(banned), "{} uses {banned}", ex.id);
        }
    }
}

#[test]
fn every_example_compiles_on_the_real_engine() {
    let lab = Lab::over(gallery_world().bus);
    for ex in gallery::examples() {
        lab.compile(ex.source)
            .unwrap_or_else(|e| panic!("{}: {e}", ex.id));
    }
}

#[test]
fn globs_match_whole_lines() {
    assert!(glob_match("a*c", "abc"));
    assert!(glob_match("a*c", "ac"));
    assert!(glob_match("*", ""));
    assert!(glob_match("exact", " exact "));
    assert!(glob_match("x * y * z", "x 1 y 2 z"));
    assert!(!glob_match("a*c", "abd"));
    assert!(!glob_match("ab*bc", "abc"), "the two ends must not overlap");
    assert!(!glob_match("exact", "exactly"));
    let output = vec!["one".to_string(), "two".to_string(), "three".to_string()];
    assert!(check_expectations(&["one".into(), "t*".into()], &output).is_ok());
    assert_eq!(
        check_expectations(&["two".into(), "one".into()], &output),
        Err("one".to_string())
    );
}
