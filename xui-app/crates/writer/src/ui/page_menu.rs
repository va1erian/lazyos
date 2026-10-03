#![forbid(unsafe_code)]

//! The Page setup menu: Paper, Orientation and Margins submenus of radio
//! entries, shown under the Page setup button.

use xui_core::app::Ui;
use xui_core::widget::{Menu, MenuId};

use crate::app::Msg;
use crate::page::{Choice, Margins, Paper};

const A4: usize = 1;
const LETTER: usize = 2;
const PORTRAIT: usize = 3;
const LANDSCAPE: usize = 4;
const NORMAL: usize = 5;
const NARROW: usize = 6;
const WIDE: usize = 7;

/// The menu, its entries unchecked until [`sync`] runs.
pub fn build(ui: &Ui<Msg>) -> Menu<Msg> {
    let id = MenuId::new;
    Menu::context(ui)
        .build(|m| {
            m.submenu(id(10), "Paper", |m| {
                m.radio(id(A4), "A4 (210 x 297 mm)", false);
                m.radio(id(LETTER), "Letter (8.5 x 11 in)", false);
            });
            m.submenu(id(11), "Orientation", |m| {
                m.radio(id(PORTRAIT), "Portrait", false);
                m.radio(id(LANDSCAPE), "Landscape", false);
            });
            m.submenu(id(12), "Margins", |m| {
                m.radio(id(NORMAL), "Normal", false);
                m.radio(id(NARROW), "Narrow", false);
                m.radio(id(WIDE), "Wide", false);
            });
        })
        .on_toggle(|id, checked| {
            let index = (0..=WIDE).find(|&i| MenuId::new(i) == id)?;
            checked.then_some(Msg::PageChoice(index))
        })
}

/// Checks the entries of `choice`; a page no choice describes checks none.
pub fn sync(menu: &Menu<Msg>, choice: Option<Choice>) {
    for index in A4..=WIDE {
        let on = choice.is_some_and(|c| match index {
            A4 => c.paper == Paper::A4,
            LETTER => c.paper == Paper::Letter,
            PORTRAIT => !c.landscape,
            LANDSCAPE => c.landscape,
            NORMAL => c.margins == Margins::Normal,
            NARROW => c.margins == Margins::Narrow,
            _ => c.margins == Margins::Wide,
        });
        menu.set_checked(MenuId::new(index), on);
    }
}

/// `current` with the entry `index` picked.
pub fn apply(index: usize, current: Choice) -> Choice {
    let mut next = current;
    match index {
        A4 => next.paper = Paper::A4,
        LETTER => next.paper = Paper::Letter,
        PORTRAIT => next.landscape = false,
        LANDSCAPE => next.landscape = true,
        NORMAL => next.margins = Margins::Normal,
        NARROW => next.margins = Margins::Narrow,
        WIDE => next.margins = Margins::Wide,
        _ => {}
    }
    next
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_entry_changes_one_part_of_the_choice() {
        let start = Choice {
            paper: Paper::A4,
            landscape: false,
            margins: Margins::Normal,
        };
        assert_eq!(apply(LETTER, start).paper, Paper::Letter);
        assert!(apply(LANDSCAPE, start).landscape);
        assert!(!apply(PORTRAIT, apply(LANDSCAPE, start)).landscape);
        assert_eq!(apply(WIDE, start).margins, Margins::Wide);
        assert_eq!(apply(NARROW, start).margins, Margins::Narrow);
        assert_eq!(apply(99, start), start);
    }
}
