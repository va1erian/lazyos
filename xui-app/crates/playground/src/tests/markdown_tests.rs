use xui_markdown::{BlockKind, Document, Inline};

use crate::catalog::{Catalog, NodeId};
use crate::docs::{self, Block, Chip, ChipKind, DocLink, FieldRow};
use crate::markdown::{classify, page, Link};

fn parsed(blocks: &[Block]) -> Document {
    Document::parse(&page(blocks))
}

fn kinds(doc: &Document) -> Vec<BlockKind> {
    doc.blocks().iter().map(|b| b.kind.clone()).collect()
}

/// The texts of the blocks.
fn texts(doc: &Document) -> Vec<&str> {
    doc.blocks().iter().map(|b| doc.block_text(b)).collect()
}

#[test]
fn a_doc_comment_is_markdown_and_shows_as_its_author_wrote_it() {
    let doc = parsed(&[Block::Paragraph(
        "Returns **every** key under `path`:\n\n- one\n- two".to_string(),
    )]);
    assert_eq!(
        kinds(&doc),
        [
            BlockKind::Paragraph,
            BlockKind::ListItem(xui_markdown::Marker::Bullet),
            BlockKind::ListItem(xui_markdown::Marker::Bullet),
        ]
    );
    assert_eq!(texts(&doc)[0], "Returns every key under path:");
    let styles: Vec<Inline> = doc
        .spans(&doc.blocks()[0])
        .iter()
        .map(|s| s.style)
        .collect();
    assert!(styles.contains(&Inline::BOLD));
    assert!(styles.contains(&Inline::CODE));
}

#[test]
fn names_the_playground_writes_are_shown_literally() {
    let doc = parsed(&[
        Block::Title("topic os.lazy.*/x_y_z".to_string()),
        Block::Heading("**not bold**".to_string()),
        Block::List(vec!["1. not a number".to_string(), "a_b_c * d".to_string()]),
    ]);
    assert_eq!(
        texts(&doc),
        [
            "topic os.lazy.*/x_y_z",
            "**not bold**",
            "1. not a number",
            "a_b_c * d"
        ]
    );
    assert_eq!(kinds(&doc)[0], BlockKind::Heading(1));
    assert_eq!(kinds(&doc)[1], BlockKind::Heading(2));
    assert!(doc
        .blocks()
        .iter()
        .flat_map(|b| doc.spans(b))
        .all(|s| s.style == Inline::NONE));
}

#[test]
fn code_and_signatures_are_fenced_whatever_they_hold() {
    let doc = parsed(&[
        Block::Signature("Get(path: String) -> ```x```".to_string()),
        Block::Code {
            label: "msg".to_string(),
            text: "let x = `a`;\n// *not* emphasis\n".to_string(),
        },
    ]);
    let code: Vec<&str> = doc
        .blocks()
        .iter()
        .filter(|b| matches!(b.kind, BlockKind::Code { .. }))
        .map(|b| doc.block_text(b))
        .collect();
    assert_eq!(
        code,
        [
            "Get(path: String) -> ```x```",
            "let x = `a`;\n// *not* emphasis"
        ]
    );
    let rhai = doc.blocks().iter().find_map(|b| match &b.kind {
        BlockKind::Code { lang } if !lang.is_empty() => Some(lang.to_string()),
        _ => None,
    });
    assert_eq!(rhai.as_deref(), Some("rhai"));
}

#[test]
fn fields_are_a_table_with_a_note_column_only_when_there_is_a_note() {
    let row = |name: &str, ty: &str, note: &str| FieldRow {
        name: name.to_string(),
        ty: ty.to_string(),
        note: note.to_string(),
    };
    let doc = parsed(&[Block::Fields(vec![
        row("path", "String", ""),
        row("a|b", "List<`T`>", ""),
    ])]);
    let rows: Vec<&xui_markdown::Block> = doc.blocks().iter().collect();
    assert_eq!(rows.len(), 3, "a header and two rows");
    assert_eq!(doc.cells(rows[2]).len(), 2);
    assert_eq!(doc.block_text(rows[2]), "a|bList<`T`>");
    let doc = parsed(&[Block::Fields(vec![row("x", "u8", "the *x*")])]);
    let header = &doc.blocks()[0];
    assert_eq!(doc.cells(header).len(), 3);
}

#[test]
fn chips_are_quotes_and_links_a_list_of_links() {
    let doc = parsed(&[
        Block::Chips(vec![
            Chip {
                kind: ChipKind::Warning,
                text: "stops a service".to_string(),
            },
            Chip {
                kind: ChipKind::Info,
                text: "one-way".to_string(),
            },
        ]),
        Block::Links(vec![DocLink {
            label: "The (reference)".to_string(),
            path: "/docs/os/idl/a b.md".to_string(),
        }]),
    ]);
    assert!(doc.blocks()[..2].iter().all(|b| b.quote == 1));
    assert_eq!(texts(&doc)[..2], ["Warning: stops a service", "one-way"]);
    let item = &doc.blocks()[2];
    let link = doc.spans(item).iter().find_map(|s| s.link).expect("a link");
    assert_eq!(doc.link(link), "/docs/os/idl/a b.md");
    assert_eq!(doc.block_text(item), "The (reference)");
}

#[test]
fn every_page_of_the_catalog_parses_to_its_title_first() {
    let catalog = Catalog::from_schema();
    for node in catalog.nodes() {
        let doc = parsed(&docs::blocks(&catalog, node));
        assert_eq!(
            doc.blocks().first().map(|b| &b.kind),
            Some(&BlockKind::Heading(1)),
            "{:?}",
            node.id
        );
    }
    let node = catalog.get(&NodeId::interface("os.lazy.confd.v1")).unwrap();
    let doc = parsed(&docs::blocks(&catalog, node));
    assert!(doc
        .blocks()
        .iter()
        .flat_map(|b| doc.spans(b))
        .filter_map(|s| s.link)
        .any(|l| doc.link(l).ends_with("/idl/os.lazy.confd.v1.md")));
}

#[test]
fn links_go_to_their_app_only_when_plainly_safe() {
    assert_eq!(
        classify("https://x.org/a"),
        Link::Url("https://x.org/a".into())
    );
    assert_eq!(classify("MAILTO:a@b.c"), Link::Url("MAILTO:a@b.c".into()));
    assert_eq!(
        classify("/docs/os/a.md#part"),
        Link::File("/docs/os/a.md".into())
    );
    assert_eq!(
        classify("/docs/os/a%2Db.md?ref=x"),
        Link::File("/docs/os/a-b.md".into())
    );
    for refused in [
        "/a%20b.md",
        "/docs/%2E%2E/etc/shadow",
        "/a%0Ab",
        "/a%zz",
        "",
        "relative.md",
        "javascript:alert(1)",
        "file:///etc/passwd",
        "/docs/../etc/shadow",
        "/a b.md",
        "/a\nb",
    ] {
        assert_eq!(classify(refused), Link::Refused, "{refused:?}");
    }
}
