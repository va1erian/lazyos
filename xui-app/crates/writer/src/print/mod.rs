#![forbid(unsafe_code)]

//! Printing to an IPP Everywhere printer such as the HP DeskJet 3700
//! (docs/printing-plan.md): the print bar's choices ([`Options`]), rendering
//! pages to PWG Raster ([`render`]) and the job sent over IPP ([`job`]).
//!
//! Pages are rendered on the UI thread, one per timer tick, because the
//! document and its shaper live there; the encoded bytes go to a worker
//! thread that owns the connection, so the window never waits on the network.

pub mod job;
pub mod render;

use xui_rich_text::model::PageSetup;

/// The print quality picker's entries, in order, and their `print-quality`.
pub const QUALITIES: [(&str, i32); 3] = [("Draft", 3), ("Normal", 4), ("High", 5)];
/// The colour picker's entries, in order.
pub const COLORS: [&str; 2] = ["Colour", "Grey"];

/// What the print bar asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    /// The printer's address as typed.
    pub printer: String,
    pub copies: u32,
    /// The page range as typed: empty or `all` for every page, else
    /// `1-3, 5`.
    pub pages: String,
    pub grey: bool,
    /// `print-quality`: 3, 4 or 5.
    pub quality: i32,
}

/// The pages (from 0) a range names, in order and without repeats, out of
/// `count`. Pages are counted from 1 as the status bar shows them.
pub fn parse_pages(text: &str, count: usize) -> Result<Vec<usize>, String> {
    let text = text.trim();
    if text.is_empty() || text.eq_ignore_ascii_case("all") {
        return Ok((0..count).collect());
    }
    let mut pages = Vec::new();
    for part in text.split(',') {
        let part = part.trim();
        let (from, to) = match part.split_once('-') {
            Some((a, b)) => (number(a, count)?, number(b, count)?),
            None => {
                let n = number(part, count)?;
                (n, n)
            }
        };
        if from > to {
            return Err(format!("\"{part}\" runs backwards"));
        }
        for page in from - 1..to {
            if !pages.contains(&page) {
                pages.push(page);
            }
        }
    }
    Ok(pages)
}

fn number(text: &str, count: usize) -> Result<usize, String> {
    let text = text.trim();
    let n: usize = text
        .parse()
        .map_err(|_| format!("\"{text}\" is not a page number"))?;
    if n == 0 || n > count {
        let pages = if count == 1 { "1 page" } else { "pages" };
        return Err(format!(
            "page {n} does not exist (the document has {count} {pages})"
        ));
    }
    Ok(n)
}

/// The PWG media name of `page`'s paper, as a portrait sheet: the standard
/// name for A4 and Letter, else a custom size in millimetres.
pub fn media_name(page: &PageSetup) -> String {
    let mm = |d: xui_core::Dip| d.0 * 25.4 / 96.0;
    let (w, h) = (mm(page.width), mm(page.height));
    let (w, h) = if w > h { (h, w) } else { (w, h) };
    let near = |a: f32, b: f32| (a - b).abs() < 1.0;
    if near(w, 210.0) && near(h, 297.0) {
        "iso_a4_210x297mm".to_owned()
    } else if near(w, 215.9) && near(h, 279.4) {
        "na_letter_8.5x11in".to_owned()
    } else {
        format!("custom_lazywriter_{}x{}mm", w.round(), h.round())
    }
}

/// The user name a job is submitted under.
pub fn user_name() -> String {
    std::env::var("USER")
        .ok()
        .filter(|u| !u.is_empty() && u.len() < 64)
        .unwrap_or_else(|| "lazywriter".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use xui_rich_text::model::mm;

    #[test]
    fn page_ranges_read_like_a_print_dialog() {
        assert_eq!(parse_pages("", 3), Ok(vec![0, 1, 2]));
        assert_eq!(parse_pages(" All ", 2), Ok(vec![0, 1]));
        assert_eq!(parse_pages("2", 3), Ok(vec![1]));
        assert_eq!(parse_pages("1-2, 5, 4-5", 5), Ok(vec![0, 1, 4, 3]));
        assert!(parse_pages("0", 3).is_err());
        assert!(parse_pages("4", 3).unwrap_err().contains("has 3 pages"));
        assert!(parse_pages("3-1", 3).is_err());
        assert!(parse_pages("x", 3).is_err());
        assert!(parse_pages("1,,2", 3).is_err());
    }

    #[test]
    fn paper_sizes_get_their_pwg_names() {
        assert_eq!(media_name(&PageSetup::a4()), "iso_a4_210x297mm");
        assert_eq!(media_name(&PageSetup::a4().rotated()), "iso_a4_210x297mm");
        assert_eq!(media_name(&PageSetup::letter()), "na_letter_8.5x11in");
        let odd = PageSetup {
            width: mm(100.0),
            height: mm(150.0),
            ..PageSetup::a4()
        };
        assert_eq!(media_name(&odd), "custom_lazywriter_100x150mm");
    }
}
