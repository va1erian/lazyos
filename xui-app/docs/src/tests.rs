use super::*;

use xui_markdown::{BlockKind, Inline};

fn parse(markdown: &str) -> Document {
    Document::parse(&page(markdown))
}

#[test]
fn raw_html_is_text_not_markup() {
    let doc = parse("<script>alert(1)</script>\n\ninline <img src=x onerror=y> here");
    assert!(doc.text().contains("<script>alert(1)</script>"));
    assert!(doc.text().contains("<img src=x onerror=y>"));
}

#[test]
fn oversized_input_is_truncated_on_a_char_boundary() {
    let big = "\u{e9}".repeat(MAX_BYTES); // 2 bytes each, so the cut lands mid-run
    let cut = page(&big);
    assert!(cut.ends_with(TRUNCATED));
    assert!(cut.len() <= MAX_BYTES + TRUNCATED.len());
    assert!(parse(&big).text().ends_with("(document truncated)"));
}

#[test]
fn a_document_at_the_limit_is_shown_whole() {
    let text = "a".repeat(MAX_BYTES);
    assert_eq!(page(&text), text);
}

#[test]
fn headings_get_github_style_anchors() {
    let doc = parse("# The `rhai` command\n\n## Errors & limits\n\n## Errors & limits\n");
    assert_eq!(doc.anchor("the-rhai-command"), Some(0));
    assert_eq!(doc.anchor("errors--limits"), Some(1));
    assert_eq!(doc.anchor("errors--limits-1"), Some(2));
}

#[test]
fn empty_input_is_an_empty_document() {
    assert!(parse("").blocks().is_empty());
}

/// A reader that counts how much was pulled from it.
struct Counting<'a>(&'a std::cell::Cell<usize>, usize);

impl Read for Counting<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = buf.len().min(self.1);
        buf[..n].fill(b'a');
        self.1 -= n;
        self.0.set(self.0.get() + n);
        Ok(n)
    }
}

#[test]
fn a_huge_file_is_read_only_up_to_the_limit_plus_one() {
    let pulled = std::cell::Cell::new(0);
    let text = read_bounded(Counting(&pulled, 50 * MAX_BYTES)).expect("reads");
    assert_eq!(text.len(), MAX_BYTES + 1);
    assert_eq!(
        pulled.get(),
        MAX_BYTES + 1,
        "nothing past the limit was read"
    );
    assert!(page(&text).ends_with(TRUNCATED));
}

#[test]
fn a_file_at_the_limit_is_not_reported_truncated() {
    let text = read_bounded(&vec![b'a'; MAX_BYTES][..]).expect("reads");
    assert!(!page(&text).ends_with(TRUNCATED));
}

#[test]
fn a_multibyte_character_cut_by_the_limit_does_not_fail_the_read() {
    // 2-byte characters: the limit + 1 byte lands mid-character.
    let bytes = "\u{e9}".repeat(MAX_BYTES).into_bytes();
    let text = read_bounded(&bytes[..]).expect("reads");
    assert!(text.ends_with('\u{FFFD}'));
    assert!(page(&text).ends_with(TRUNCATED));
}

#[test]
fn invalid_utf8_is_replaced_not_rejected() {
    let text = read_bounded(&b"ok \xFF\xFE done"[..]).expect("reads");
    assert_eq!(text, "ok \u{FFFD}\u{FFFD} done");
}

/// The fixture the image ships as `/system/share/samples/testdoc.md`, which
/// the Docs session opens.
const TESTDOC: &str = include_str!("../testdata/testdoc.md");

#[test]
fn the_test_document_has_every_construct_it_advertises() {
    let doc = parse(TESTDOC);
    let kinds: Vec<&BlockKind> = doc.blocks().iter().map(|b| &b.kind).collect();
    for level in 1..=4 {
        assert!(kinds.contains(&&BlockKind::Heading(level)), "h{level}");
    }
    let has = |want: fn(&BlockKind) -> bool| kinds.iter().any(|k| want(k));
    assert!(has(|k| matches!(
        k,
        BlockKind::TableRow { header: true, .. }
    )));
    assert!(has(|k| matches!(k, BlockKind::Code { .. })));
    assert!(has(|k| matches!(
        k,
        BlockKind::ListItem(xui_markdown::Marker::Bullet)
    )));
    assert!(has(|k| matches!(
        k,
        BlockKind::ListItem(xui_markdown::Marker::Number(_))
    )));
    assert!(has(|k| matches!(k, BlockKind::Rule)));
    assert!(doc.blocks().iter().any(|b| b.quote > 0), "a block quote");
    let styled = |style: Inline| {
        doc.blocks()
            .iter()
            .flat_map(|b| doc.spans(b))
            .any(|s| s.style.contains(style))
    };
    for style in [Inline::BOLD, Inline::ITALIC, Inline::STRIKE, Inline::CODE] {
        assert!(styled(style), "{style:?}");
    }
    assert!(
        doc.blocks()
            .iter()
            .flat_map(|b| doc.spans(b))
            .any(|s| s.link.is_some()),
        "a link"
    );
    assert!(!doc.text().contains("(document truncated)"));
    assert!(doc.blocks().len() > 40, "long enough to scroll");
}

#[test]
fn load_file_reads_and_parses_a_file() {
    let dir = std::env::temp_dir().join(format!("xui-docs-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("note.md");
    std::fs::write(&path, "# From disk\n\ntext").unwrap();
    let doc = load_file(&path).expect("loads");
    assert_eq!(doc.anchor("from-disk"), Some(0));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn load_file_reports_a_missing_file() {
    let error = load_file(Path::new("/definitely/not/here.md")).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::NotFound);
}

#[test]
fn the_error_page_shows_the_message_as_written() {
    let doc = Document::parse(&error_page("no <b>such</b> & *file*_x_ [a](b)\nmore"));
    assert_eq!(doc.blocks()[0].kind, BlockKind::Heading(1));
    assert_eq!(
        doc.block_text(&doc.blocks()[1]),
        "no <b>such</b> & *file*_x_ [a](b) more"
    );
    let body = &doc.blocks()[1];
    assert!(doc
        .spans(body)
        .iter()
        .all(|s| s.style == Inline::NONE && s.link.is_none()));
}
