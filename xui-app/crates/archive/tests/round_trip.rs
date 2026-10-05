//! Create, open, extract, add and delete across every writable format.

mod common;

use std::fs;

use common::{files, progress, sample_tree, tree, Scratch};
use lazyarc::extract::{self, Options, Overwrite};
use lazyarc::{create, rewrite, Archive, Error, Format, Level, Source};

const MULTI: [(Format, &str); 4] = [
    (Format::Zip, "out.zip"),
    (Format::Tar, "out.tar"),
    (Format::TarGz, "out.tar.gz"),
    (Format::TarZst, "out.tar.zst"),
];

#[test]
fn every_writable_format_round_trips_a_tree() {
    for (format, name) in MULTI {
        for level in [Level::Store, Level::Normal] {
            let scratch = Scratch::new("rt");
            let project = sample_tree(&scratch.0);
            let dest = scratch.join(name);
            let report = create::create(
                &dest,
                format,
                level,
                &Source::under("", std::slice::from_ref(&project)),
                &progress(),
            )
            .unwrap();
            assert_eq!(report.files, 4, "{format:?}");
            assert!(report.skipped.is_empty());
            let archive = Archive::open(&dest, &progress()).unwrap();
            assert_eq!(archive.format, format);
            let out = scratch.join("out");
            let report =
                extract::extract(&archive, &|_| true, &out, &Options::default(), &progress())
                    .unwrap();
            assert!(
                report.skipped.is_empty(),
                "{format:?}: {:?}",
                report.skipped
            );
            assert_eq!(report.top_level, vec![out.join("project")]);
            assert_eq!(
                tree(&out.join("project")),
                tree(&project),
                "{format:?} {level:?}"
            );
            // No partial file is left beside the archive.
            let leftovers: Vec<_> = fs::read_dir(&scratch.0)
                .unwrap()
                .filter_map(|e| e.ok())
                .filter(|e| e.file_name().to_string_lossy().contains(".partial-"))
                .collect();
            assert!(leftovers.is_empty());
        }
    }
}

#[test]
fn single_file_formats_round_trip_and_refuse_folders() {
    for (format, name) in [(Format::Gz, "main.rs.gz"), (Format::Zst, "main.rs.zst")] {
        let scratch = Scratch::new("single");
        let project = sample_tree(&scratch.0);
        let file = project.join("src/main.rs");
        let dest = scratch.join(name);
        create::create(
            &dest,
            format,
            Level::Normal,
            &Source::under("", std::slice::from_ref(&file)),
            &progress(),
        )
        .unwrap();
        let archive = Archive::open(&dest, &progress()).unwrap();
        assert_eq!(archive.format, format);
        assert_eq!(archive.entries.len(), 1);
        assert_eq!(archive.entries[0].path, "main.rs");
        assert_eq!(files(&archive)[0].1, fs::read(&file).unwrap());
        let folder = create::create(
            &scratch.join("x.gz"),
            format,
            Level::Normal,
            &Source::under("", &[project]),
            &progress(),
        );
        assert!(matches!(folder, Err(Error::Unsupported(_))));
    }
}

#[test]
fn extracting_a_folder_strips_its_prefix() {
    let scratch = Scratch::new("strip");
    let project = sample_tree(&scratch.0);
    let dest = scratch.join("a.zip");
    create::create(
        &dest,
        Format::Zip,
        Level::Fast,
        &Source::under("", &[project]),
        &progress(),
    )
    .unwrap();
    let archive = Archive::open(&dest, &progress()).unwrap();
    let out = scratch.join("out");
    let options = Options {
        strip: "project/src".into(),
        overwrite: Overwrite::Replace,
    };
    extract::extract(
        &archive,
        &|e| e.is_under("project/src/deep"),
        &out,
        &options,
        &progress(),
    )
    .unwrap();
    assert_eq!(tree(&out).len(), 1);
    assert!(out.join("deep/data.bin").is_file());
}

#[test]
fn overwrite_policies_are_honoured() {
    let scratch = Scratch::new("overwrite");
    let project = sample_tree(&scratch.0);
    let dest = scratch.join("a.tar");
    create::create(
        &dest,
        Format::Tar,
        Level::Normal,
        &Source::under("", &[project]),
        &progress(),
    )
    .unwrap();
    let archive = Archive::open(&dest, &progress()).unwrap();
    let out = scratch.join("out");
    fs::create_dir_all(out.join("project")).unwrap();
    fs::write(out.join("project/README.md"), "mine").unwrap();
    let only_readme = |e: &lazyarc::Entry| e.path == "project/README.md";
    let run = |overwrite| {
        let options = Options {
            strip: String::new(),
            overwrite,
        };
        extract::extract(&archive, &only_readme, &out, &options, &progress()).unwrap()
    };
    let skipped = run(Overwrite::Skip);
    assert_eq!(skipped.skipped.len(), 1);
    assert_eq!(
        fs::read_to_string(out.join("project/README.md")).unwrap(),
        "mine"
    );
    run(Overwrite::Rename);
    assert_eq!(
        fs::read_to_string(out.join("project/README (2).md")).unwrap(),
        "# Project\n"
    );
    run(Overwrite::Replace);
    assert_eq!(
        fs::read_to_string(out.join("project/README.md")).unwrap(),
        "# Project\n"
    );
}

#[test]
fn adding_and_deleting_rewrite_every_writable_format() {
    for (format, name) in MULTI {
        let scratch = Scratch::new("rewrite");
        let project = sample_tree(&scratch.0);
        let dest = scratch.join(name);
        create::create(
            &dest,
            format,
            Level::Normal,
            &Source::under("", std::slice::from_ref(&project)),
            &progress(),
        )
        .unwrap();
        let extra = scratch.join("notes.txt");
        fs::write(&extra, "added later").unwrap();
        let archive = Archive::open(&dest, &progress()).unwrap();
        rewrite::add(
            &archive,
            &Source::under("project/src", &[extra]),
            Level::Normal,
            &progress(),
        )
        .unwrap();
        let archive = Archive::open(&dest, &progress()).unwrap();
        let after_add = files(&archive);
        assert!(
            after_add.contains(&("project/src/notes.txt".to_owned(), b"added later".to_vec())),
            "{format:?}"
        );
        assert_eq!(after_add.len(), 5);
        rewrite::delete(&archive, &["project/src".to_owned()], &progress()).unwrap();
        let archive = Archive::open(&dest, &progress()).unwrap();
        let names: Vec<_> = files(&archive).into_iter().map(|(p, _)| p).collect();
        assert_eq!(names, ["project/README.md", "project/empty"], "{format:?}");
        assert!(!archive
            .entries
            .iter()
            .any(|e| e.path.starts_with("project/src")));
    }
}

#[test]
fn adding_a_file_replaces_the_member_of_the_same_path() {
    let scratch = Scratch::new("replace");
    let project = sample_tree(&scratch.0);
    let dest = scratch.join("a.zip");
    create::create(
        &dest,
        Format::Zip,
        Level::Normal,
        &Source::under("", &[project]),
        &progress(),
    )
    .unwrap();
    let newer = scratch.join("README.md");
    fs::write(&newer, "new readme").unwrap();
    let archive = Archive::open(&dest, &progress()).unwrap();
    rewrite::add(
        &archive,
        &Source::under("project", &[newer]),
        Level::Normal,
        &progress(),
    )
    .unwrap();
    let archive = Archive::open(&dest, &progress()).unwrap();
    let readmes: Vec<_> = archive
        .entries
        .iter()
        .filter(|e| e.path == "project/README.md")
        .collect();
    assert_eq!(readmes.len(), 1);
    assert!(files(&archive).contains(&("project/README.md".to_owned(), b"new readme".to_vec())));
}

#[test]
fn read_only_formats_refuse_changes() {
    let archive = Archive::open(&common::fixture("lzma2.7z"), &progress()).unwrap();
    assert!(matches!(
        rewrite::delete(&archive, &["tree".into()], &progress()),
        Err(Error::Unsupported(_))
    ));
}

#[test]
fn a_cancelled_create_leaves_nothing_behind() {
    let scratch = Scratch::new("cancel");
    let project = sample_tree(&scratch.0);
    let dest = scratch.join("a.tar.gz");
    let progress = progress();
    progress.cancel();
    let result = create::create(
        &dest,
        Format::TarGz,
        Level::Normal,
        &Source::under("", &[project]),
        &progress,
    );
    assert!(matches!(result, Err(Error::Cancelled)));
    assert!(!dest.exists());
    assert_eq!(fs::read_dir(&scratch.0).unwrap().count(), 1); // only `project`
}

#[test]
fn a_cancelled_rewrite_keeps_the_original() {
    let scratch = Scratch::new("cancel-rewrite");
    let project = sample_tree(&scratch.0);
    let dest = scratch.join("a.zip");
    create::create(
        &dest,
        Format::Zip,
        Level::Normal,
        &Source::under("", &[project]),
        &progress(),
    )
    .unwrap();
    let before = fs::read(&dest).unwrap();
    let archive = Archive::open(&dest, &progress()).unwrap();
    let progress = progress();
    progress.cancel();
    assert!(rewrite::delete(&archive, &["project/src".into()], &progress).is_err());
    assert_eq!(fs::read(&dest).unwrap(), before);
}

#[test]
fn the_archive_being_written_is_never_added_to_itself() {
    let scratch = Scratch::new("self");
    let project = sample_tree(&scratch.0);
    let dest = project.join("self.zip");
    fs::write(&dest, b"old").unwrap();
    let report = create::create(
        &dest,
        Format::Zip,
        Level::Normal,
        &Source::under("", &[project]),
        &progress(),
    )
    .unwrap();
    assert!(report
        .skipped
        .iter()
        .any(|(_, why)| why == "the archive itself"));
}

#[test]
fn a_test_passes_for_good_archives_and_reports_damage() {
    let scratch = Scratch::new("test");
    let project = sample_tree(&scratch.0);
    let dest = scratch.join("a.zip");
    create::create(
        &dest,
        Format::Zip,
        Level::Store,
        &Source::under("", &[project]),
        &progress(),
    )
    .unwrap();
    let archive = Archive::open(&dest, &progress()).unwrap();
    let report = extract::test(&archive, &progress()).unwrap();
    assert_eq!((report.files, report.skipped.len()), (4, 0));
    let mut bytes = fs::read(&dest).unwrap();
    let at = bytes.windows(8).position(|w| w == b"line 100").unwrap();
    bytes[at] = b'L';
    fs::write(&dest, bytes).unwrap();
    let archive = Archive::open(&dest, &progress()).unwrap();
    let report = extract::test(&archive, &progress()).unwrap();
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(report.skipped[0].0, "project/src/main.rs");
}

#[test]
fn a_damaged_member_never_costs_the_file_it_would_replace() {
    let scratch = Scratch::new("keep-on-damage");
    let project = sample_tree(&scratch.0);
    let dest = scratch.join("a.zip");
    create::create(
        &dest,
        Format::Zip,
        Level::Store,
        &Source::under("", std::slice::from_ref(&project)),
        &progress(),
    )
    .unwrap();
    let mut bytes = fs::read(&dest).unwrap();
    let at = bytes.windows(8).position(|w| w == b"line 100").unwrap();
    bytes[at] = b'L';
    fs::write(&dest, bytes).unwrap();
    let out = scratch.join("out");
    fs::create_dir_all(out.join("project/src")).unwrap();
    fs::write(out.join("project/src/main.rs"), "mine").unwrap();
    let archive = Archive::open(&dest, &progress()).unwrap();
    let options = Options {
        strip: String::new(),
        overwrite: Overwrite::Replace,
    };
    let report = extract::extract(&archive, &|_| true, &out, &options, &progress()).unwrap();
    assert!(report
        .skipped
        .iter()
        .any(|(path, _)| path == "project/src/main.rs"));
    assert_eq!(
        fs::read_to_string(out.join("project/src/main.rs")).unwrap(),
        "mine"
    );
    // No temporary file is left beside it.
    let names: Vec<_> = fs::read_dir(out.join("project/src"))
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert!(
        names
            .iter()
            .all(|n| !n.to_string_lossy().contains(".part-")),
        "{names:?}"
    );
}
