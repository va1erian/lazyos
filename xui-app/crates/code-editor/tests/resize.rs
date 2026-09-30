//! Window-resize tests for the editor widget: after its node is re-flowed to a
//! narrower viewport, `Editor::on_resize` keeps the caret and scroll valid.
//!
//! The mounted layout calls `on_resize` from its `placed` hook, because the
//! real LazyOS backend moves nodes in a batch without a node-level `Resize`.

use std::cell::RefCell;
use std::rc::Rc;

use xui_app_testkit::TestBackend;
use xui_code_editor::{Editor, FontConfig, Options};
use xui_core::backend::{Backend, PlatformSpec};
use xui_core::geometry::Rect;
use xui_core::units::Dip;
use xui_core::{App, Ui, run_app};

/// A plain app that only hosts the editor for the session.
struct Empty;

impl App for Empty {
    type Msg = ();
    fn update(&mut self, _msg: (), _ui: &mut Ui<()>) {}
}

/// The editor handle a resize check reads back.
struct Probe {
    ui: Ui<()>,
    editor: Rc<Editor<()>>,
}

/// Builds an editor at 400x200, then runs `check` with a live handle.
fn with_editor(check: impl FnOnce(&Probe) + 'static) {
    let backend = Rc::new(TestBackend::new());
    let probe: Rc<RefCell<Option<Probe>>> = Rc::new(RefCell::new(None));
    let hook_probe = Rc::clone(&probe);
    backend.set_run_hook(move || {
        let probe = hook_probe.borrow_mut().take().expect("the app built");
        check(&probe);
    });

    let build_probe = Rc::clone(&probe);
    let handle: Rc<dyn Backend> = Rc::clone(&backend) as Rc<dyn Backend>;
    run_app(
        handle,
        PlatformSpec::new("editor").size(Dip(400.0), Dip(200.0)),
        move |ui| {
            let options = Options {
                font: FontConfig {
                    family: Some("monospace".to_owned()),
                    ..FontConfig::default()
                },
                ..Options::default()
            };
            let editor = Rc::new(
                Editor::with_options(ui, Rect::new(0, 0, 400, 200), options)
                    .expect("the editor built"),
            );
            *build_probe.borrow_mut() = Some(Probe {
                ui: ui.clone(),
                editor,
            });
            Empty
        },
    )
    .expect("the resize session ran");
}

/// A body of 50 long lines, so the caret can sit far below and right of a
/// narrow viewport.
fn long_text() -> String {
    (0..50)
        .map(|_| "x".repeat(200))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn narrowing_keeps_the_caret_in_view() {
    with_editor(|probe| {
        probe.editor.set_text(&long_text());
        probe.editor.goto(49, 199);
        let before = probe.editor.scroll_position();
        assert_eq!(probe.editor.caret_line_col(), (49, 199));

        // Re-flow the node to a much smaller viewport, as the mounted layout
        // does on a window resize.
        probe
            .ui
            .apply_moves(&[(probe.editor.id(), Rect::new(0, 0, 120, 60))]);
        probe.editor.on_resize();

        let after = probe.editor.scroll_position();
        assert!(
            after.0 > before.0,
            "the narrower viewport scrolls down further: {before:?} -> {after:?}"
        );
        assert!(
            after.1 >= before.1,
            "the horizontal scroll keeps up with the narrower viewport: {before:?} -> {after:?}"
        );
        // The caret is never scrolled past: the first visible line/column are
        // at or before it.
        assert!(after.0 <= 49, "the caret line stays visible: {after:?}");
        assert!(after.1 <= 199, "the caret column stays visible: {after:?}");
    });
}

#[test]
fn widening_does_not_lose_the_caret() {
    with_editor(|probe| {
        probe.editor.set_text(&long_text());
        probe.editor.goto(30, 150);
        // Shrink, then grow again.
        probe
            .ui
            .apply_moves(&[(probe.editor.id(), Rect::new(0, 0, 120, 60))]);
        probe.editor.on_resize();
        probe
            .ui
            .apply_moves(&[(probe.editor.id(), Rect::new(0, 0, 400, 200))]);
        probe.editor.on_resize();

        let (line, col) = probe.editor.caret_line_col();
        assert_eq!((line, col), (30, 150), "the caret did not move");
        let (first_line, first_col) = probe.editor.scroll_position();
        assert!(
            first_line <= line && first_col <= col,
            "the caret stays inside the grown viewport: ({first_line}, {first_col})"
        );
    });
}
