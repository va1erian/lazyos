//! Pure operations behind the Default apps page (docs/lazywebplus.md): which web browser opens
//! links, for the user running Settings.
//!
//! LazyWeb and LazyWeb+ both register `http`, `https` and HTML files with `mimed`, and the
//! registry alone lets whichever registered last win. The user's choice is three `confd` keys in
//! their own subtree (`deskmenu::defaults::user_key`, one per type of
//! [`WEB_TYPES`](deskmenu::defaults::WEB_TYPES)) that `mimed` reads for the caller of `Open`:
//! **Automatic** deletes them. Every edit is saved before the caller's copy changes, so the page
//! never shows a state the store refused.

use deskmenu::defaults::{self, BROWSERS, WEB_TYPES};

use crate::store::{AppChoice, ConfigStore, StoreError, Value};

/// The first row: no choice, the registry's own order.
pub const AUTOMATIC: &str = "Automatic (the browser installed last)";

/// The page's state: whose choice it is, the browsers this image has and the one chosen.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Choices {
    pub uid: u32,
    /// The installed browsers, in the order of [`BROWSERS`].
    pub browsers: Vec<AppChoice>,
    /// Index into `browsers` of the user's choice, when they made one.
    pub chosen: Option<usize>,
}

impl Choices {
    /// The rows of the page's list: [`AUTOMATIC`], then each browser's name.
    pub fn rows(&self) -> Vec<String> {
        std::iter::once(String::from(AUTOMATIC))
            .chain(self.browsers.iter().map(|app| app.name.clone()))
            .collect()
    }

    /// The row to show selected.
    pub fn selected_row(&self) -> usize {
        self.chosen.map_or(0, |index| index + 1)
    }
}

/// Read the registry and the user's key: the installed browsers and which one the user chose (a
/// stored id that names no installed browser is no choice).
pub fn load(store: &dyn ConfigStore) -> Result<Choices, StoreError> {
    let uid = store
        .uid()
        .ok_or_else(|| String::from("this session's user is unknown"))?;
    let apps = store.apps();
    let browsers: Vec<AppChoice> = BROWSERS
        .iter()
        .filter_map(|id| apps.iter().find(|app| app.desktop && app.id == *id))
        .cloned()
        .collect();
    let stored = defaults::user_key(uid, WEB_TYPES[1]).and_then(|key| store.get(&key));
    let chosen =
        defaults::app(stored.as_ref()).and_then(|id| browsers.iter().position(|app| app.id == id));
    Ok(Choices {
        uid,
        browsers,
        chosen,
    })
}

/// Choose row `row` of [`Choices::rows`]: row 0 is Automatic. Returns the status line.
pub fn choose(
    store: &dyn ConfigStore,
    choices: &mut Choices,
    row: usize,
) -> Result<String, StoreError> {
    let keys: Vec<String> = WEB_TYPES
        .iter()
        .filter_map(|mime| defaults::user_key(choices.uid, mime))
        .collect();
    let Some(index) = row.checked_sub(1) else {
        for key in &keys {
            store.delete(key)?;
        }
        choices.chosen = None;
        return Ok(String::from(
            "The web browser is the one installed last again.",
        ));
    };
    let app = choices.browsers.get(index).ok_or("no browser selected")?;
    for key in &keys {
        store.set(key, Value::Str(app.id.clone()))?;
    }
    let name = app.name.clone();
    choices.chosen = Some(index);
    Ok(format!("{name} opens web links."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemStore;

    fn app(id: &str, name: &str, desktop: bool) -> AppChoice {
        AppChoice {
            id: id.into(),
            name: name.into(),
            desktop,
        }
    }

    fn store(apps: &[AppChoice]) -> MemStore {
        let store = MemStore::new();
        *store.uid.borrow_mut() = Some(1000);
        *store.apps.borrow_mut() = apps.to_vec();
        store
    }

    fn both() -> Vec<AppChoice> {
        vec![
            app("os.lazy.files", "Files", true),
            app("os.lazy.lazywebplus", "LazyWeb+", true),
            app("os.lazy.lazyweb", "LazyWeb", true),
        ]
    }

    fn https_key() -> String {
        defaults::user_key(1000, "x-scheme-handler/https").unwrap()
    }

    #[test]
    fn the_installed_browsers_are_listed_in_the_fixed_order_with_automatic_first() {
        let store = store(&both());
        let choices = load(&store).unwrap();
        assert_eq!(
            choices.rows(),
            [AUTOMATIC, "LazyWeb", "LazyWeb+"],
            "LazyWeb first whatever the registry's order"
        );
        assert_eq!(choices.selected_row(), 0);
    }

    #[test]
    fn a_browser_that_is_not_installed_is_not_offered_and_a_console_program_is_not_a_browser() {
        let store = store(&[
            app("os.lazy.lazywebplus", "LazyWeb+", false),
            app("os.lazy.files", "Files", true),
        ]);
        assert_eq!(load(&store).unwrap().rows(), [AUTOMATIC]);
    }

    #[test]
    fn choosing_saves_all_three_types_and_automatic_takes_them_away() {
        let store = store(&both());
        let mut choices = load(&store).unwrap();
        let said = choose(&store, &mut choices, 2).unwrap();
        assert_eq!(said, "LazyWeb+ opens web links.");
        for mime in WEB_TYPES {
            let key = defaults::user_key(1000, mime).unwrap();
            assert_eq!(
                store.get(&key),
                Some(Value::Str("os.lazy.lazywebplus".into())),
                "{key}"
            );
        }
        assert_eq!(choices.selected_row(), 2);
        // A fresh load finds it again.
        assert_eq!(load(&store).unwrap().selected_row(), 2);

        choose(&store, &mut choices, 0).unwrap();
        assert!(store.is_empty(), "automatic leaves no key behind");
        assert_eq!(choices.selected_row(), 0);
    }

    #[test]
    fn a_stored_choice_that_names_no_installed_browser_is_no_choice() {
        let store = store(&both());
        store
            .set(&https_key(), Value::Str("os.lazy.gone".into()))
            .unwrap();
        assert_eq!(load(&store).unwrap().selected_row(), 0);
        store.set(&https_key(), Value::Bool(true)).unwrap();
        assert_eq!(load(&store).unwrap().selected_row(), 0);
    }

    #[test]
    fn a_refused_write_changes_nothing_the_page_shows() {
        let store = store(&both());
        let mut choices = load(&store).unwrap();
        *store.fail_writes.borrow_mut() = Some("denied".into());
        assert_eq!(choose(&store, &mut choices, 1), Err("denied".into()));
        assert_eq!(choices.selected_row(), 0);
        assert!(choose(&store, &mut choices, 9).is_err(), "no such row");
    }

    #[test]
    fn without_a_known_user_the_page_cannot_name_a_key() {
        let store = MemStore::new();
        assert!(load(&store).is_err());
    }
}
