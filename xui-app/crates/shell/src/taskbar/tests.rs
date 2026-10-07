use super::*;

fn window(id: u64, title: &'static str) -> Surface<'static> {
    Surface {
        id,
        role: Role::Window,
        title: Some(title),
        minimized: false,
        focused: false,
    }
}

fn panel(id: u64) -> Surface<'static> {
    Surface {
        role: Role::Shell,
        ..window(id, "panel")
    }
}

fn ids(bar: &Taskbar) -> Vec<u64> {
    bar.windows().iter().map(|w| w.surface).collect()
}

#[test]
fn seeding_skips_the_shells_own_surfaces_and_sorts_by_creation() {
    let mut bar = Taskbar::new();
    let focused = Surface {
        focused: true,
        ..window(9, "Files")
    };
    let n = bar.seed([window(12, "Editor"), panel(1), focused, panel(2)]);
    assert_eq!(n, 2);
    assert_eq!(ids(&bar), vec![9, 12]);
    assert_eq!(bar.focused(), Some(9));
}

#[test]
fn created_and_destroyed_add_and_remove_entries() {
    let mut bar = Taskbar::new();
    assert_eq!(
        bar.apply(ChangeKind::Created, &window(5, "Terminal")),
        Delta::Added(5, "Terminal".into())
    );
    assert_eq!(
        bar.apply(ChangeKind::Created, &window(3, "Paint")),
        Delta::Added(3, "Paint".into())
    );
    assert_eq!(ids(&bar), vec![3, 5], "creation (id) order");
    assert_eq!(
        bar.apply(ChangeKind::Destroyed, &window(5, "")),
        Delta::Removed(5)
    );
    assert_eq!(
        bar.apply(ChangeKind::Destroyed, &window(5, "")),
        Delta::None
    );
    assert_eq!(ids(&bar), vec![3]);
}

#[test]
fn panels_and_the_desktop_never_get_an_entry() {
    let mut bar = Taskbar::new();
    assert_eq!(bar.apply(ChangeKind::Created, &panel(1)), Delta::None);
    assert_eq!(bar.apply(ChangeKind::Title, &panel(1)), Delta::None);
    assert!(bar.windows().is_empty());
}

#[test]
fn a_change_for_a_missed_window_adds_it_and_a_repeat_create_updates() {
    let mut bar = Taskbar::new();
    bar.seed([window(4, "Old")]);
    assert_eq!(
        bar.apply(ChangeKind::Created, &window(4, "New")),
        Delta::Changed
    );
    assert_eq!(bar.windows()[0].title, "New");
    assert_eq!(
        bar.apply(ChangeKind::Title, &window(8, "Late")),
        Delta::Added(8, "Late".into())
    );
}

#[test]
fn minimize_restore_and_title_mark_the_entry() {
    let mut bar = Taskbar::new();
    bar.seed([window(2, "Files")]);
    assert_eq!(
        bar.apply(ChangeKind::Minimized, &window(2, "Files")),
        Delta::Changed
    );
    assert!(bar.windows()[0].minimized);
    assert_eq!(
        bar.apply(ChangeKind::Restored, &window(2, "Files")),
        Delta::Changed
    );
    assert!(!bar.windows()[0].minimized);
    assert_eq!(
        bar.apply(ChangeKind::Other, &window(2, "Files")),
        Delta::None
    );
    let untitled = Surface {
        title: Some("\u{7}  "),
        ..window(6, "")
    };
    assert_eq!(
        bar.apply(ChangeKind::Created, &untitled),
        Delta::Added(6, "Window".into())
    );
}

#[test]
fn focus_follows_focus_changed_and_clears_on_destroy() {
    let mut bar = Taskbar::new();
    bar.seed([window(1, "a"), window(2, "b")]);
    assert!(bar.set_focus(Some(2)));
    assert!(!bar.set_focus(Some(2)));
    bar.apply(ChangeKind::Destroyed, &window(2, "b"));
    assert_eq!(bar.focused(), None);
}

#[test]
fn clicking_the_focused_visible_entry_minimizes_anything_else_activates() {
    let mut bar = Taskbar::new();
    bar.seed([window(1, "a"), window(2, "b")]);
    bar.set_focus(Some(1));
    assert_eq!(bar.click(1), Some(Action::Minimize(1)));
    assert_eq!(bar.click(2), Some(Action::Activate(2)));
    bar.apply(ChangeKind::Minimized, &window(1, "a"));
    assert_eq!(bar.click(1), Some(Action::Activate(1)), "restore");
    assert_eq!(bar.click(42), None);
}

#[test]
fn entries_follow_the_contract_geometry() {
    // 1024 wide, a 200 px clock: plenty of room, so full 160 px entries.
    let rects = entry_rects(3, 1024, 200);
    let centres: Vec<(i32, i32)> = rects
        .iter()
        .map(|r| r.map(|r| (r.x + r.w / 2, r.y + r.h / 2)).unwrap())
        .collect();
    // Entry i centre = (92 + i*164 + 80, H-16); panel-local y 16 = H-16.
    assert_eq!(centres, vec![(172, 16), (336, 16), (500, 16)]);
    assert_eq!(rects[0], Some(Rect::new(92, 3, 160, 26)));
    // Screen y of an entry is H-29.
    assert_eq!(rects[0].unwrap().offset(0, 768 - BAR_H).y, 768 - 29);
}

#[test]
fn entries_squeeze_to_an_equal_share_and_hide_past_the_minimum() {
    let rects = entry_rects(10, 1024, 200);
    let right = 1024 - 200 - ENTRY_GAP;
    let width = rects[0].unwrap().w;
    assert!(width < ENTRY_MAX_W);
    for rect in rects.iter().flatten() {
        assert_eq!(rect.w, width);
        assert!(rect.x + rect.w <= right);
    }
    let crowded = entry_rects(100, 640, 200);
    assert!(crowded.iter().any(Option::is_none), "some are hidden");
    assert!(crowded[0].is_some());
    assert!(entry_rects(0, 1024, 200).is_empty());
}

#[test]
fn hit_testing_finds_the_entry_under_the_pointer() {
    let rects = entry_rects(2, 1024, 200);
    assert_eq!(entry_at(&rects, 172, 16), Some(0));
    assert_eq!(entry_at(&rects, 336, 16), Some(1));
    assert_eq!(entry_at(&rects, 254, 16), None, "the gap");
    assert_eq!(entry_at(&rects, 172, 1), None, "above the entries");
}

#[test]
fn the_start_button_and_clock_sit_where_the_contract_says() {
    // Screen click point (44, H-16) is panel-local (44, 16).
    assert!(START_BUTTON.contains(44, 16));
    assert_eq!(
        START_BUTTON.offset(0, 768 - BAR_H),
        Rect::new(4, 738, 80, 28)
    );
    let clock = clock_rect(1024, 150);
    assert_eq!(clock, Rect::new(1024 - 174, 0, 174, BAR_H));
    assert_eq!(clock_rect(100, 500).w, 100, "clamped to the bar");
}
