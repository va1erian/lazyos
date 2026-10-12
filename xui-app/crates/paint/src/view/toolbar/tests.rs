use super::*;

#[test]
fn cells_flow_two_to_a_row_from_the_top_left() {
    let bounds = Rect::new(0, 0, 80, 300);
    let first = item_rect(bounds, 96, 0);
    let second = item_rect(bounds, 96, 1);
    let third = item_rect(bounds, 96, 2);
    assert_eq!(first.width(), cell_px(96));
    assert_eq!(
        second.left, first.right,
        "the second cell is beside the first"
    );
    assert_eq!(second.top, first.top);
    assert_eq!(third.left, first.left, "the third starts a new row");
    assert_eq!(third.top, first.bottom);
}

#[test]
fn the_grid_fits_its_design_width() {
    for dpi in [96, 192] {
        let width = WIDTH.to_px(dpi).value();
        let last = item_rect(Rect::new(0, 0, width, 500), dpi, 1);
        assert!(last.right <= width, "two cells fit at {dpi} dpi");
    }
}

#[test]
fn a_hit_finds_the_tool_under_the_point() {
    let state = State {
        items: Tool::ALL.to_vec(),
        bounds: Cell::new(Rect::new(0, 0, 72, 300)),
        dpi: Cell::new(96),
        active: Cell::new(Tool::Pencil),
        hover: Cell::new(None),
        pressed: Cell::new(None),
    };
    let centre = |index: usize| {
        let rect = state.rect(index);
        Point::new((rect.left + rect.right) / 2, (rect.top + rect.bottom) / 2)
    };
    assert_eq!(state.index_at(centre(0)), Some(0));
    assert_eq!(state.index_at(centre(3)), Some(3));
    assert_eq!(state.index_at(Point::new(500, 5)), None);
}
