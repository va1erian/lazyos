//! Pure list operations behind the Menu page: the start menu's pinned apps
//! are one confd value (`deskmenu::KEY`), a machine setting whose every
//! write asks an administrator. The edits here only change the page's draft;
//! [`save`] writes the whole draft once, so a session of moves, renames and
//! pins costs one prompt, and the page keeps showing what is stored until it
//! succeeds.

use deskmenu::{Entry, MAX_ENTRIES, MAX_LABEL};

use crate::store::{AppChoice, ConfigStore, StoreError};

/// The stored list; the defaults when nothing usable is stored. Any
/// well-formed id is kept, including apps this image does not ship (the
/// registry lists only shipped apps): dropping them here would erase them on
/// the next save, and a launch of an unshipped app just answers unavailable.
pub fn load(store: &dyn ConfigStore) -> Vec<Entry> {
    deskmenu::from_value(store.get(deskmenu::KEY).as_ref(), &|_| true)
}

/// Write `draft` as the menu, in one request: the defaults are stored by
/// dropping the key, anything else as the whole list.
pub fn save(store: &dyn ConfigStore, draft: &[Entry]) -> Result<(), StoreError> {
    if draft == deskmenu::defaults().as_slice() {
        if store.get(deskmenu::KEY).is_none() {
            return Ok(());
        }
        return store.delete(deskmenu::KEY);
    }
    store.set(deskmenu::KEY, deskmenu::to_value(draft))
}

/// The built-in defaults (nothing pinned).
pub fn reset(list: &mut Vec<Entry>) {
    *list = deskmenu::defaults();
}

/// Move entry `index` by `delta` rows, clamped to the ends. Returns its new
/// index.
pub fn move_by(list: &mut Vec<Entry>, index: usize, delta: isize) -> Result<usize, StoreError> {
    if index >= list.len() {
        return Err("no entry selected".into());
    }
    let target = index.saturating_add_signed(delta).min(list.len() - 1);
    if target != index {
        let entry = list.remove(index);
        list.insert(target, entry);
    }
    Ok(target)
}

/// Remove entry `index`. Returns the index to select (`0` once the list is
/// empty).
pub fn remove(list: &mut Vec<Entry>, index: usize) -> Result<usize, StoreError> {
    if index >= list.len() {
        return Err("no entry selected".into());
    }
    list.remove(index);
    Ok(index.min(list.len().saturating_sub(1)))
}

/// Give entry `index` a new label. Control characters are stripped; a label
/// that is then empty or longer than [`MAX_LABEL`] is refused.
pub fn rename(list: &mut [Entry], index: usize, label: &str) -> Result<(), StoreError> {
    if index >= list.len() {
        return Err("no entry selected".into());
    }
    let clean: String = label.chars().filter(|c| !c.is_control()).collect();
    let clean = clean.trim();
    if clean.is_empty() {
        return Err("the label cannot be empty".into());
    }
    if clean.chars().count() > MAX_LABEL {
        return Err(format!("the label is limited to {MAX_LABEL} characters"));
    }
    list[index].label = clean.to_owned();
    Ok(())
}

/// Append `app` with its display name as label. Returns the new index.
pub fn add(list: &mut Vec<Entry>, app: &AppChoice) -> Result<usize, StoreError> {
    if list.len() >= MAX_ENTRIES {
        return Err(format!("at most {MAX_ENTRIES} apps can be pinned"));
    }
    if list.iter().any(|e| e.app == app.id) {
        return Err("that app is already pinned".into());
    }
    let entry = Entry::new(&app.id, &app.name).ok_or("not a launchable app id")?;
    list.push(entry);
    Ok(list.len() - 1)
}

/// The registry apps not yet in `list`, in registry order.
pub fn available(list: &[Entry], apps: &[AppChoice]) -> Vec<AppChoice> {
    apps.iter()
        .filter(|a| !list.iter().any(|e| e.app == a.id))
        .cloned()
        .collect()
}

/// A list row: the label, with the app id when the label differs.
pub fn row_text(entry: &Entry) -> String {
    if entry.label == entry.app {
        entry.label.clone()
    } else {
        format!("{}  ({})", entry.label, entry.app)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemStore;

    fn choice(id: &str) -> AppChoice {
        AppChoice {
            id: id.into(),
            name: id.to_uppercase(),
            desktop: true,
        }
    }

    /// A pinned list to edit: the menu the start menu used to show.
    fn pinned() -> Vec<Entry> {
        [
            ("terminal", "Terminal"),
            ("os.lazy.sysmon", "System Monitor"),
            ("os.lazy.paint", "Paint"),
            ("os.lazy.docs", "Docs"),
            ("devices", "Devices"),
        ]
        .iter()
        .filter_map(|(app, label)| Entry::new(app, label))
        .collect()
    }

    fn ids(list: &[Entry]) -> Vec<&str> {
        list.iter().map(|e| e.app.as_str()).collect()
    }

    /// A store that counts its writes (each one is a prompt on LazyOS).
    #[derive(Default)]
    struct Counting {
        inner: MemStore,
        writes: std::cell::Cell<usize>,
    }

    impl ConfigStore for Counting {
        fn get(&self, key: &str) -> Option<crate::store::Value> {
            self.inner.get(key)
        }
        fn set(&self, key: &str, value: crate::store::Value) -> Result<(), StoreError> {
            self.writes.set(self.writes.get() + 1);
            self.inner.set(key, value)
        }
        fn delete(&self, key: &str) -> Result<(), StoreError> {
            self.writes.set(self.writes.get() + 1);
            self.inner.delete(key)
        }
    }

    #[test]
    fn empty_store_loads_defaults_and_keeps_unshipped_apps() {
        let store = MemStore::new();
        assert_eq!(load(&store), deskmenu::defaults());
        save(&store, &pinned()).unwrap();
        // An app the registry does not list (not shipped) survives a reload.
        assert!(ids(&load(&store)).contains(&"os.lazy.docs"));
    }

    #[test]
    fn a_session_of_edits_is_one_write() {
        let store = Counting::default();
        let mut draft = pinned();
        assert_eq!(move_by(&mut draft, 1, -1), Ok(0));
        assert_eq!(move_by(&mut draft, 0, 2), Ok(2));
        rename(&mut draft, 0, "Shell").unwrap();
        remove(&mut draft, 4).unwrap();
        add(&mut draft, &choice("os.lazy.files")).unwrap();
        assert_eq!(store.writes.get(), 0, "an edit wrote");
        save(&store, &draft).unwrap();
        assert_eq!(store.writes.get(), 1);
        assert_eq!(load(&store), draft);
    }

    #[test]
    fn saving_the_defaults_drops_the_key_and_only_when_stored() {
        let store = Counting::default();
        save(&store, &deskmenu::defaults()).unwrap();
        assert_eq!(store.writes.get(), 0, "nothing stored, nothing to drop");
        save(&store, &pinned()).unwrap();
        let mut draft = load(&store);
        reset(&mut draft);
        save(&store, &draft).unwrap();
        assert_eq!(store.writes.get(), 2);
        assert!(store.inner.is_empty());
        assert_eq!(load(&store), deskmenu::defaults());
    }

    #[test]
    fn move_past_the_ends_clamps() {
        let mut list = pinned();
        assert_eq!(move_by(&mut list, 0, -1), Ok(0));
        let last = list.len() - 1;
        assert_eq!(move_by(&mut list, last, 1), Ok(last));
        assert_eq!(list, pinned());
        assert_eq!(move_by(&mut list, 0, 100), Ok(last));
        assert_eq!(list[last].app, "terminal");
        assert!(move_by(&mut list, 99, 1).is_err());
    }

    #[test]
    fn remove_can_unpin_everything_and_selects_a_neighbour() {
        let mut list = pinned();
        let last = list.len() - 1;
        assert_eq!(remove(&mut list, last), Ok(last - 1));
        while !list.is_empty() {
            assert_eq!(remove(&mut list, 0), Ok(0));
        }
        assert!(remove(&mut list, 0).is_err());
    }

    #[test]
    fn rename_cleans_and_rejects_bad_labels() {
        let mut list = pinned();
        rename(&mut list, 0, "  Shell\t\n ").unwrap();
        assert_eq!(list[0].label, "Shell");
        assert!(rename(&mut list, 0, "").is_err());
        assert!(rename(&mut list, 0, " \t\u{7} ").is_err());
        assert!(rename(&mut list, 0, &"x".repeat(MAX_LABEL + 1)).is_err());
        rename(&mut list, 0, &"x".repeat(MAX_LABEL)).unwrap();
        assert_eq!(list[0].label.len(), MAX_LABEL);
        assert!(rename(&mut list, 99, "a").is_err());
    }

    #[test]
    fn add_appends_and_rejects_duplicates() {
        let mut list = vec![Entry::new("terminal", "Terminal").unwrap()];
        assert_eq!(add(&mut list, &choice("paint")), Ok(1));
        assert_eq!(list[1].label, "PAINT");
        assert!(add(&mut list, &choice("paint")).is_err());
        assert!(add(&mut list, &choice("Bad Id")).is_err());
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn add_stops_at_the_cap() {
        let mut list = Vec::new();
        for i in 0..MAX_ENTRIES {
            add(&mut list, &choice(&format!("app{i}"))).unwrap();
        }
        assert!(add(&mut list, &choice("one-more")).is_err());
        assert_eq!(list.len(), MAX_ENTRIES);
    }

    #[test]
    fn available_excludes_listed_apps_in_registry_order() {
        let list = vec![Entry::new("os.lazy.paint", "Paint").unwrap()];
        let apps: Vec<AppChoice> = pinned().iter().map(|e| choice(&e.app)).collect();
        let free = available(&list, &apps);
        assert_eq!(free.len(), apps.len() - 1);
        assert!(free.iter().all(|a| a.id != "os.lazy.paint"));
        assert_eq!(free[0].id, "terminal");
    }

    #[test]
    fn a_refused_save_leaves_the_store_alone() {
        let store = MemStore::new();
        save(&store, &pinned()).unwrap();
        *store.fail_writes.borrow_mut() = Some("denied".into());
        let mut draft = pinned();
        remove(&mut draft, 0).unwrap();
        assert_eq!(save(&store, &draft), Err("denied".into()));
        assert_eq!(save(&store, &deskmenu::defaults()), Err("denied".into()));
        assert_eq!(load(&store), pinned());
    }

    #[test]
    fn row_text_shows_the_app_only_when_renamed() {
        let same = Entry::new("paint", "").unwrap();
        assert_eq!(row_text(&same), "paint");
        let named = Entry::new("paint", "Canvas").unwrap();
        assert_eq!(row_text(&named), "Canvas  (paint)");
    }
}
