//! The state of the suggestions dropdown: its rows, which one the person moved to, and what each
//! does. No window here; `app/suggest.rs` draws it and feeds it keys.

use super::{completion, Match, MAX_ROWS};

/// What the person did to the dropdown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    /// Up (`-1`) or Down (`1`) one row; past the ends it goes back to the typed text.
    Move(i32),
    /// Escape: put it away.
    Close,
    /// Shift+Delete: forget the highlighted page.
    Remove,
    /// End: finish the host the best match suggests.
    Accept,
    /// A click on row `n`.
    Pick(usize),
}

/// One row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Row {
    /// A visited page.
    Visit { url: String, title: String },
    /// The last row: search for the typed text.
    Search(String),
}

/// The longest label drawn for a row, in characters.
const MAX_LABEL: usize = 110;

impl Row {
    /// What the row says.
    pub fn label(&self) -> String {
        match self {
            Row::Visit { url, title } => {
                let short = super::Item::short_of(url);
                let text = if title.trim().is_empty() {
                    short
                } else {
                    format!("{}  -  {short}", title.trim())
                };
                cut(&text, MAX_LABEL)
            }
            Row::Search(query) => cut(&format!("Search for \"{}\"", query.trim()), MAX_LABEL),
        }
    }
}

/// `text` of at most `max` characters, ending in an ellipsis when it was cut.
fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('\u{2026}');
    out
}

/// The rows for what is typed, and the highlighted one (none until the person moves).
#[derive(Debug, Default)]
pub struct Dropdown {
    query: String,
    rows: Vec<Row>,
    selected: Option<usize>,
    completion: Option<String>,
}

impl Dropdown {
    /// Shows `matches` for `query`, then the search row. Nothing for an empty query.
    pub fn set(&mut self, query: &str, matches: &[Match<'_>]) {
        self.selected = None;
        self.completion = matches.first().and_then(|best| completion(query, best));
        self.query = query.trim().to_owned();
        self.rows.clear();
        if self.query.is_empty() {
            return;
        }
        self.rows
            .extend(matches.iter().take(MAX_ROWS).map(|m| Row::Visit {
                url: m.url.clone(),
                title: m.title.clone(),
            }));
        self.rows.push(Row::Search(self.query.clone()));
    }

    pub fn clear(&mut self) {
        self.rows.clear();
        self.selected = None;
        self.completion = None;
    }

    pub fn is_open(&self) -> bool {
        !self.rows.is_empty()
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn labels(&self) -> Vec<String> {
        self.rows.iter().map(Row::label).collect()
    }

    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// The row the person moved to, if any.
    pub fn chosen(&self) -> Option<&Row> {
        self.selected.and_then(|n| self.rows.get(n))
    }

    /// The address text with the best host finished, if it can be.
    pub fn completion(&self) -> Option<&str> {
        self.completion.as_deref()
    }

    /// Moves the highlight by `delta` rows through the rows and the typed text (no highlight).
    pub fn step(&mut self, delta: i32) {
        let places = self.rows.len() as i64 + 1;
        if places == 1 {
            return;
        }
        // Place 0 is the typed text, place n + 1 is row n.
        let at = self.selected.map_or(0, |n| n as i64 + 1);
        let to = (at + i64::from(delta)).rem_euclid(places);
        self.selected = (to > 0).then(|| to as usize - 1);
    }

    /// Highlights row `n` (a click).
    pub fn select(&mut self, n: usize) {
        if n < self.rows.len() {
            self.selected = Some(n);
        }
    }

    /// Drops the highlighted page row and returns its address; the search row is not a page.
    pub fn remove_chosen(&mut self) -> Option<String> {
        let n = self.selected?;
        let Row::Visit { url, .. } = self.rows.get(n)? else {
            return None;
        };
        let url = url.clone();
        self.rows.remove(n);
        // The highlight stays on the row that took its place (the search row at the end).
        self.selected = Some(n.min(self.rows.len().saturating_sub(1)));
        self.completion = None;
        Some(url)
    }
}

#[cfg(test)]
mod tests {
    use super::super::Index;
    use super::*;
    use crate::visits::Visit;

    fn index() -> Index {
        let visit = |url: &str, title: &str| Visit {
            time: 1_000,
            url: url.into(),
            title: title.into(),
        };
        Index::from_visits(&[
            visit("https://a.example/", "Alpha"),
            visit("https://b.example/x", ""),
        ])
    }

    fn dropdown(query: &str) -> Dropdown {
        let index = index();
        let mut dropdown = Dropdown::default();
        dropdown.set(query, &index.rank(query, 1_000, MAX_ROWS));
        dropdown
    }

    #[test]
    fn rows_are_the_matches_then_the_search_and_nothing_is_highlighted() {
        let d = dropdown("example");
        assert!(d.is_open());
        assert_eq!(d.rows().len(), 3);
        assert_eq!(
            d.labels(),
            [
                "Alpha  -  a.example",
                "b.example/x",
                "Search for \"example\""
            ]
        );
        assert_eq!(d.selected(), None);
        assert!(d.chosen().is_none());
    }

    #[test]
    fn no_match_still_offers_the_search_and_no_text_offers_nothing() {
        let d = dropdown("zzz");
        assert_eq!(d.rows(), [Row::Search("zzz".into())]);
        assert!(!dropdown("   ").is_open());
    }

    #[test]
    fn up_and_down_walk_the_rows_and_the_typed_text() {
        let mut d = dropdown("example");
        d.step(1);
        assert_eq!(d.selected(), Some(0));
        d.step(1);
        d.step(1);
        assert_eq!(d.selected(), Some(2), "the search row");
        d.step(1);
        assert_eq!(d.selected(), None, "back to what was typed");
        d.step(-1);
        assert_eq!(
            d.selected(),
            Some(2),
            "Up from the typed text wraps to the end"
        );
        d.step(-1);
        assert_eq!(d.chosen(), Some(&d.rows()[1]));
    }

    #[test]
    fn a_page_can_be_removed_but_the_search_row_cannot() {
        let mut d = dropdown("example");
        d.step(1);
        assert_eq!(d.remove_chosen().as_deref(), Some("https://a.example/"));
        assert_eq!(d.rows().len(), 2);
        assert_eq!(d.selected(), Some(0), "the next row takes the highlight");
        d.step(1);
        assert_eq!(d.chosen(), Some(&Row::Search("example".into())));
        assert_eq!(d.remove_chosen(), None);
        assert_eq!(d.rows().len(), 2);
        let mut none = dropdown("example");
        assert_eq!(none.remove_chosen(), None, "nothing highlighted");
    }

    #[test]
    fn a_click_highlights_that_row_only_if_it_exists() {
        let mut d = dropdown("example");
        d.select(1);
        assert_eq!(d.selected(), Some(1));
        d.select(9);
        assert_eq!(d.selected(), Some(1));
    }

    #[test]
    fn long_labels_are_cut() {
        let long = "x".repeat(300);
        let row = Row::Visit {
            url: format!("https://a.example/{long}"),
            title: String::new(),
        };
        let label = row.label();
        assert_eq!(label.chars().count(), MAX_LABEL);
        assert!(label.ends_with('\u{2026}'));
    }

    #[test]
    fn the_completion_follows_the_best_match() {
        assert_eq!(dropdown("b.ex").completion(), Some("b.example"));
        assert_eq!(dropdown("alp").completion(), None, "a title is not a host");
    }
}
