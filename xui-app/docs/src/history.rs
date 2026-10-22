//! The documents visited, for Back and Forward. Pure and host-tested.

use std::path::PathBuf;

/// Most entries each of the back and forward lists keeps.
const HISTORY_LIMIT: usize = 100;

/// A place in the documentation: a document (`None` is the welcome page).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    pub path: Option<PathBuf>,
}

/// The documents visited, for Back and Forward.
#[derive(Default)]
pub struct History {
    back: Vec<Place>,
    forward: Vec<Place>,
}

impl History {
    /// Records that the user left `from` for a new place: Forward is lost.
    pub fn visit(&mut self, from: Place) {
        if self.back.last() != Some(&from) {
            self.back.push(from);
            if self.back.len() > HISTORY_LIMIT {
                self.back.remove(0);
            }
        }
        self.forward.clear();
    }

    /// The place Back leads to from `current`, which Forward then returns to.
    pub fn back(&mut self, current: Place) -> Option<Place> {
        let target = self.back.pop()?;
        self.forward.push(current);
        Some(target)
    }

    /// The place Forward leads to from `current`.
    pub fn forward(&mut self, current: Place) -> Option<Place> {
        let target = self.forward.pop()?;
        self.back.push(current);
        Some(target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(path: &str) -> Place {
        Place {
            path: Some(PathBuf::from(path)),
        }
    }

    #[test]
    fn back_and_forward_walk_the_visited_documents() {
        let mut history = History::default();
        // welcome -> a -> b
        history.visit(Place { path: None });
        history.visit(doc("/a.md"));
        assert_eq!(history.back(doc("/b.md")), Some(doc("/a.md")));
        assert_eq!(history.back(doc("/a.md")), Some(Place { path: None }));
        assert_eq!(history.back(Place { path: None }), None);
        assert_eq!(history.forward(Place { path: None }), Some(doc("/a.md")));
        assert_eq!(history.forward(doc("/a.md")), Some(doc("/b.md")));
        assert_eq!(history.forward(doc("/b.md")), None);
    }

    #[test]
    fn a_new_visit_drops_forward_and_the_list_is_bounded() {
        let mut history = History::default();
        history.visit(doc("/a.md"));
        assert_eq!(history.back(doc("/b.md")), Some(doc("/a.md")));
        history.visit(doc("/a.md"));
        assert_eq!(history.forward(doc("/c.md")), None);
        for n in 0..HISTORY_LIMIT * 2 {
            history.visit(doc(&format!("/{n}.md")));
        }
        assert_eq!(history.back.len(), HISTORY_LIMIT);
    }

    #[test]
    fn visiting_the_same_place_twice_keeps_one_entry() {
        let mut history = History::default();
        history.visit(doc("/a.md"));
        history.visit(doc("/a.md"));
        assert_eq!(history.back(doc("/b.md")), Some(doc("/a.md")));
        assert_eq!(history.back(doc("/a.md")), None);
    }
}
