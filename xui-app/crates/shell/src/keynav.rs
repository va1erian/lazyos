//! Keyboard navigation of the shell's panel menus (issue #648): the start
//! menu, its category submenus, a tray item's menu and the tray itself.
//!
//! While one of them is open the compositor routes keys to the shell
//! (`GrabPanelKeys`, `PanelKey` in `os.lazy.display.v1`), and these rules turn
//! each key into a [`Step`] the panel carries out, the same for every menu:
//!
//! * **Up/Down** move between rows, wrapping, and skip rows that cannot be
//!   chosen (separators, disabled rows); **Home/End** jump to the first or
//!   last one. With nothing selected Down selects the first row, Up the last.
//! * **Right** opens the selected row's submenu; **Enter** (or Space) opens
//!   it too, or activates an ordinary row (launch, check, radio, Quit).
//! * **Left** closes one level (a submenu back to its parent); **Escape**
//!   closes one level, and the whole menu at the top.
//!
//! The tray ([`tray_step`]) is a row of cells: Left/Right move, Enter or
//! Space activates the cell as a left click would, Up/Down (or the
//! context-menu key, Shift+F10) open its menu as a right click would, and
//! Escape leaves it. The pointer path is unchanged.

/// A key the menus understand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavKey {
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    /// Enter or Space.
    Enter,
    Escape,
    /// The context-menu key or Shift+F10.
    Menu,
}

/// What a row is, for navigation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    /// Activates something (an app, a check, a radio, Quit, a power row).
    Item,
    /// Opens a submenu.
    Parent,
    /// A separator or a disabled row: skipped.
    Inert,
}

impl RowKind {
    fn selectable(self) -> bool {
        self != RowKind::Inert
    }
}

/// What a key asks the panel to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Light row `index` (the selection moved).
    Select(usize),
    /// Open the selected row's submenu and move into it.
    OpenChild(usize),
    /// Activate row `index`.
    Activate(usize),
    /// Close this submenu and go back to its parent.
    Back,
    /// Close the whole menu.
    Close,
    /// Nothing to do.
    Nothing,
}

/// The step `key` makes on a panel whose rows are `kinds`, with `selected`
/// lit; `child` when the panel is a submenu (Left and Escape go back to its
/// parent instead of closing everything).
pub fn step(kinds: &[RowKind], selected: Option<usize>, key: NavKey, child: bool) -> Step {
    let current = selected.filter(|&index| kinds.get(index).is_some_and(|k| k.selectable()));
    match key {
        NavKey::Down => select(next(kinds, selected, true)),
        NavKey::Up => select(next(kinds, selected, false)),
        NavKey::Home => select(next(kinds, None, true)),
        NavKey::End => select(next(kinds, None, false)),
        NavKey::Right => match current {
            Some(index) if kinds[index] == RowKind::Parent => Step::OpenChild(index),
            _ => Step::Nothing,
        },
        NavKey::Enter => match current {
            Some(index) if kinds[index] == RowKind::Parent => Step::OpenChild(index),
            Some(index) => Step::Activate(index),
            None => Step::Nothing,
        },
        NavKey::Left if child => Step::Back,
        NavKey::Left | NavKey::Menu => Step::Nothing,
        NavKey::Escape if child => Step::Back,
        NavKey::Escape => Step::Close,
    }
}

fn select(index: Option<usize>) -> Step {
    index.map_or(Step::Nothing, Step::Select)
}

/// The next selectable row after `from` (before it when `!forward`),
/// wrapping; from nothing, the first (last) selectable row.
pub fn next(kinds: &[RowKind], from: Option<usize>, forward: bool) -> Option<usize> {
    let count = kinds.len();
    if count == 0 {
        return None;
    }
    let start = match (from.filter(|&index| index < count), forward) {
        (Some(index), true) => index + 1,
        (Some(index), false) => index + count - 1,
        (None, true) => 0,
        (None, false) => count - 1,
    };
    (0..count)
        .map(|offset| {
            if forward {
                (start + offset) % count
            } else {
                (start + count - offset) % count
            }
        })
        .find(|&index| kinds[index].selectable())
}

/// What a key does on the tray.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayStep {
    /// Light cell `index`.
    Select(usize),
    /// Activate cell `index` (a left click).
    Activate(usize),
    /// Open cell `index`'s menu (a right click) and move into it.
    Menu(usize),
    /// Leave the tray.
    Leave,
    Nothing,
}

/// The step `key` makes on a tray of `cells` cells with `selected` lit.
pub fn tray_step(cells: usize, selected: Option<usize>, key: NavKey) -> TrayStep {
    if cells == 0 {
        return TrayStep::Leave;
    }
    let kinds = vec![RowKind::Item; cells];
    let current = selected.filter(|&index| index < cells);
    let lit = |step: Option<usize>| step.map_or(TrayStep::Nothing, TrayStep::Select);
    match key {
        NavKey::Right => lit(next(&kinds, current, true)),
        NavKey::Left => lit(next(&kinds, current, false)),
        NavKey::Home => TrayStep::Select(0),
        NavKey::End => TrayStep::Select(cells - 1),
        NavKey::Enter => current.map_or(TrayStep::Nothing, TrayStep::Activate),
        NavKey::Up | NavKey::Down | NavKey::Menu => {
            current.map_or(TrayStep::Nothing, TrayStep::Menu)
        }
        NavKey::Escape => TrayStep::Leave,
    }
}

#[cfg(test)]
mod tests;
