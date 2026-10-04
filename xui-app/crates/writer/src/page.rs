#![forbid(unsafe_code)]

//! The page setup choices the Page setup menu offers, and how a document's
//! `PageSetup` reads back as those choices.

use xui_core::Dip;
use xui_rich_text::model::{PageSetup, mm};

/// The paper sizes offered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Paper {
    /// ISO A4, 210 x 297 mm: the default.
    A4,
    /// US Letter, 8.5 x 11 in.
    Letter,
}

/// The margin presets offered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Margins {
    /// 25 mm all round on A4, 1 in on Letter.
    Normal,
    /// 12.7 mm (half an inch) all round.
    Narrow,
    /// 50.8 mm (2 in) left and right, Normal top and bottom.
    Wide,
}

/// One choice of paper, orientation and margins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Choice {
    pub paper: Paper,
    pub landscape: bool,
    pub margins: Margins,
}

impl Choice {
    /// The page this choice describes.
    pub fn page(self) -> PageSetup {
        let base = match self.paper {
            Paper::A4 => PageSetup::a4(),
            Paper::Letter => PageSetup::letter(),
        };
        let normal = base.top;
        let page = match self.margins {
            Margins::Normal => base,
            Margins::Narrow => base.with_margins(mm(12.7)),
            Margins::Wide => PageSetup {
                left: mm(50.8),
                right: mm(50.8),
                ..base.with_margins(normal)
            },
        };
        // Margins are named for the portrait sheet; turning it keeps the
        // wide sides left and right on screen.
        if self.landscape {
            PageSetup {
                width: page.height,
                height: page.width,
                ..page
            }
        } else {
            page
        }
    }

    /// The choice `page` was made from, if it matches one exactly; `None` for
    /// a page set up some other way (another program, a later version).
    pub fn of(page: &PageSetup) -> Option<Choice> {
        const PAPERS: [Paper; 2] = [Paper::A4, Paper::Letter];
        const MARGINS: [Margins; 3] = [Margins::Normal, Margins::Narrow, Margins::Wide];
        PAPERS
            .iter()
            .flat_map(|&paper| {
                [false, true].into_iter().flat_map(move |landscape| {
                    MARGINS.iter().map(move |&margins| Choice {
                        paper,
                        landscape,
                        margins,
                    })
                })
            })
            .find(|c| same(&c.page(), page))
    }
}

/// Whether two pages match to a hundredth of a dip (JSON round trips floats).
fn same(a: &PageSetup, b: &PageSetup) -> bool {
    let close = |x: Dip, y: Dip| (x.0 - y.0).abs() < 0.01;
    close(a.width, b.width)
        && close(a.height, b.height)
        && close(a.left, b.left)
        && close(a.top, b.top)
        && close(a.right, b.right)
        && close(a.bottom, b.bottom)
}

/// The status bar's page label: `Page 2 of 5`, from a page index from 0.
pub fn pages_label(page: usize, count: usize) -> String {
    format!("Page {} of {count}", page + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_choice_is_a_valid_page_that_reads_back() {
        for paper in [Paper::A4, Paper::Letter] {
            for landscape in [false, true] {
                for margins in [Margins::Normal, Margins::Narrow, Margins::Wide] {
                    let choice = Choice {
                        paper,
                        landscape,
                        margins,
                    };
                    let page = choice.page();
                    assert!(page.check().is_ok(), "{choice:?}");
                    assert_eq!(page.is_landscape(), landscape);
                    assert_eq!(Choice::of(&page), Some(choice));
                }
            }
        }
    }

    #[test]
    fn the_default_page_is_a4_portrait_normal() {
        assert_eq!(
            Choice::of(&PageSetup::default()),
            Some(Choice {
                paper: Paper::A4,
                landscape: false,
                margins: Margins::Normal,
            })
        );
        let odd = PageSetup::a4().with_margins(Dip(10.0));
        assert_eq!(Choice::of(&odd), None);
    }

    #[test]
    fn the_page_label_counts_from_one() {
        assert_eq!(pages_label(0, 1), "Page 1 of 1");
        assert_eq!(pages_label(2, 5), "Page 3 of 5");
    }
}
