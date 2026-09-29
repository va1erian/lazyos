use super::*;

#[test]
fn the_flow_wraps_and_sizes_the_strip() {
    assert_eq!(preferred_height(19, 800, 96), cell_px(96));
    // At a narrow width the same cells wrap onto more rows.
    assert!(preferred_height(19, 100, 96) > preferred_height(19, 800, 96));
    let bounds = Rect::new(0, 0, 60, 100);
    assert!(
        item_rect(bounds, 96, 2).top > item_rect(bounds, 96, 0).top,
        "the third cell wrapped"
    );
    assert_eq!(item_rect(bounds, 96, 0).width(), cell_px(96));
}

#[test]
fn disabled_items_are_not_hit() {
    let state = State {
        items: vec![StripItem::Undo, StripItem::Tool(Tool::Pencil)],
        bounds: Cell::new(Rect::new(0, 0, 200, 28)),
        dpi: Cell::new(96),
        active_tool: Cell::new(Tool::Pencil),
        active_size: Cell::new(1),
        can_undo: Cell::new(false),
        can_redo: Cell::new(false),
        can_io: Cell::new(false),
        hover: Cell::new(None),
        pressed: Cell::new(None),
    };
    assert_eq!(state.index_at(Point::new(5, 5)), None);
    assert_eq!(state.index_at(Point::new(35, 5)), Some(1));
}
