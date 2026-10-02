//! Pure list operations behind the Menu page: the desktop right-click menu is
//! one confd value (`deskmenu::KEY`), and every edit here saves the whole new
//! list before the caller's copy changes, so the page never shows a state the
//! store refused.

use deskmenu::{Entry, MAX_ENTRIES, MAX_LABEL};

use crate::store::{AppChoice, ConfigStore, StoreError};

/// The stored list; the defaults when nothing usable is stored. Any
/// well-formed id is kept, including apps this image does not ship (the
/// registry lists only shipped apps): dropping them here would erase them on
/// the next save, and a launch of an unshipped app just answers unavailable.
pub fn load(store: &dyn ConfigStore) -> Vec<Entry> {
    deskmenu::from_value(store.get(deskmenu::KEY).as_ref(), &|_| true)
}

/// Write `entries`; `list` becomes `entries` only when the write succeeded.
fn commit(
    store: &dyn ConfigStore,
    list: &mut Vec<Entry>,
    entries: Vec<Entry>,
) -> Result<(), StoreError> {
    store.set(deskmenu::KEY, deskmenu::to_value(&entries))?;
    *list = entries;
    Ok(())
}

/// Drop the stored list and return to the built-in defaults.
pub fn reset(store: &dyn ConfigStore, list: &mut Vec<Entry>) -> Result<(), StoreError> {
    store.delete(deskmenu::KEY)?;
    *list = deskmenu::defaults();
    Ok(())
}

/// Move entry `index` by `delta` rows, clamped to the ends. Returns its new
/// index.
pub fn move_by(
    store: &dyn ConfigStore,
    list: &mut Vec<Entry>,
    index: usize,
    delta: isize,
) -> Result<usize, StoreError> {
    if index >= list.len() {
        return Err("no entry selected".into());
    }
    let target = index.saturating_add_signed(delta).min(list.len() - 1);
    if target == index {
        return Ok(index);
    }
    let mut next = list.clone();
    let entry = next.remove(index);
    next.insert(target, entry);
    commit(store, list, next)?;
    Ok(target)
}

/// Remove entry `index`. The last entry cannot go (an empty stored list reads
/// back as the defaults, which would surprise). Returns the index to select.
pub fn remove(
    store: &dyn ConfigStore,
    list: &mut Vec<Entry>,
    index: usize,
) -> Result<usize, StoreError> {
    if index >= list.len() {
        return Err("no entry selected".into());
    }
    if list.len() == 1 {
        return Err("the menu needs at least one entry".into());
    }
    let mut next = list.clone();
    next.remove(index);
    commit(store, list, next)?;
    Ok(index.min(list.len() - 1))
}

/// Give entry `index` a new label. Control characters are stripped; a label
/// that is then empty or longer than [`MAX_LABEL`] is refused.
pub fn rename(
    store: &dyn ConfigStore,
    list: &mut Vec<Entry>,
    index: usize,
    label: &str,
) -> Result<(), StoreError> {
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
    let mut next = list.clone();
    next[index].label = clean.to_owned();
    commit(store, list, next)
}

/// Append `app` with its display name as label. Returns the new index.
pub fn add(
    store: &dyn ConfigStore,
    list: &mut Vec<Entry>,
    app: &AppChoice,
) -> Result<usize, StoreError> {
    if list.len() >= MAX_ENTRIES {
        return Err(format!("the menu is limited to {MAX_ENTRIES} entries"));
    }
    if list.iter().any(|e| e.app == app.id) {
        return Err("that app is already in the menu".into());
    }
    let entry = Entry::new(&app.id, &app.name).ok_or("not a launchable app id")?;
    let mut next = list.clone();
    next.push(entry);
    commit(store, list, next)?;
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

    fn ids(list: &[Entry]) -> Vec<&str> {
        list.iter().map(|e| e.app.as_str()).collect()
    }

    #[test]
    fn empty_store_loads_defaults_and_keeps_unshipped_apps() {
        let store = MemStore::new();
        assert_eq!(load(&store), deskmenu::defaults());
        let mut list = deskmenu::defaults();
        let same = list.clone();
        commit(&store, &mut list, same).unwrap();
        // An app the registry does not list (not shipped) survives a reload.
        assert!(ids(&load(&store)).contains(&"os.lazy.docs"));
    }

    #[test]
    fn move_round_trips_through_the_store() {
        let store = MemStore::new();
        let mut list = deskmenu::defaults();
        assert_eq!(move_by(&store, &mut list, 1, -1), Ok(0));
        assert_eq!(list[0].app, "os.lazy.sysmon");
        assert_eq!(load(&store), list);
    }

    #[test]
    fn move_past_the_ends_clamps_without_writing() {
        let store = MemStore::new();
        let mut list = deskmenu::defaults();
        assert_eq!(move_by(&store, &mut list, 0, -1), Ok(0));
        let last = list.len() - 1;
        assert_eq!(move_by(&store, &mut list, last, 1), Ok(last));
        assert!(store.is_empty());
        assert_eq!(move_by(&store, &mut list, 0, 100), Ok(last));
        assert_eq!(list[last].app, "terminal");
        assert!(move_by(&store, &mut list, 99, 1).is_err());
    }

    #[test]
    fn remove_refuses_the_last_entry() {
        let store = MemStore::new();
        let mut list = deskmenu::defaults();
        while list.len() > 1 {
            remove(&store, &mut list, 0).unwrap();
        }
        assert!(remove(&store, &mut list, 0).is_err());
        assert_eq!(list.len(), 1);
        assert_eq!(load(&store).len(), 1);
    }

    #[test]
    fn remove_selects_a_neighbour() {
        let store = MemStore::new();
        let mut list = deskmenu::defaults();
        let last = list.len() - 1;
        assert_eq!(remove(&store, &mut list, last), Ok(last - 1));
        assert_eq!(remove(&store, &mut list, 0), Ok(0));
        assert!(remove(&store, &mut list, 50).is_err());
    }

    #[test]
    fn rename_cleans_and_rejects_bad_labels() {
        let store = MemStore::new();
        let mut list = deskmenu::defaults();
        rename(&store, &mut list, 0, "  Shell\t\n ").unwrap();
        assert_eq!(list[0].label, "Shell");
        assert!(rename(&store, &mut list, 0, "").is_err());
        assert!(rename(&store, &mut list, 0, " \t\u{7} ").is_err());
        assert!(rename(&store, &mut list, 0, &"x".repeat(MAX_LABEL + 1)).is_err());
        rename(&store, &mut list, 0, &"x".repeat(MAX_LABEL)).unwrap();
        assert_eq!(list[0].label.len(), MAX_LABEL);
        assert!(rename(&store, &mut list, 99, "a").is_err());
        assert_eq!(load(&store)[0].label.len(), MAX_LABEL);
    }

    #[test]
    fn add_appends_and_rejects_duplicates() {
        let store = MemStore::new();
        let mut list = vec![Entry::new("terminal", "Terminal").unwrap()];
        assert_eq!(add(&store, &mut list, &choice("paint")), Ok(1));
        assert_eq!(list[1].label, "PAINT");
        assert!(add(&store, &mut list, &choice("paint")).is_err());
        assert!(add(&store, &mut list, &choice("Bad Id")).is_err());
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn add_stops_at_the_cap() {
        let store = MemStore::new();
        let mut list = Vec::new();
        for i in 0..MAX_ENTRIES {
            add(&store, &mut list, &choice(&format!("app{i}"))).unwrap();
        }
        assert!(add(&store, &mut list, &choice("one-more")).is_err());
        assert_eq!(list.len(), MAX_ENTRIES);
    }

    #[test]
    fn available_excludes_listed_apps_in_registry_order() {
        let list = vec![Entry::new("os.lazy.paint", "Paint").unwrap()];
        let apps: Vec<AppChoice> = deskmenu::defaults()
            .iter()
            .map(|e| choice(&e.app))
            .collect();
        let free = available(&list, &apps);
        assert_eq!(free.len(), apps.len() - 1);
        assert!(free.iter().all(|a| a.id != "os.lazy.paint"));
        assert_eq!(free[0].id, "terminal");
    }

    #[test]
    fn reset_restores_defaults_and_clears_the_key() {
        let store = MemStore::new();
        let mut list = deskmenu::defaults();
        remove(&store, &mut list, 0).unwrap();
        reset(&store, &mut list).unwrap();
        assert_eq!(list, deskmenu::defaults());
        assert!(store.is_empty());
    }

    #[test]
    fn store_failure_leaves_state_unchanged() {
        let store = MemStore::new();
        let mut list = deskmenu::defaults();
        let before = list.clone();
        *store.fail_writes.borrow_mut() = Some("denied".into());
        assert_eq!(move_by(&store, &mut list, 1, -1), Err("denied".into()));
        assert_eq!(remove(&store, &mut list, 0), Err("denied".into()));
        assert_eq!(rename(&store, &mut list, 0, "X"), Err("denied".into()));
        assert_eq!(
            add(&store, &mut Vec::new(), &choice("x")),
            Err("denied".into())
        );
        assert_eq!(reset(&store, &mut list), Err("denied".into()));
        assert_eq!(list, before);
    }

    #[test]
    fn row_text_shows_the_app_only_when_renamed() {
        let same = Entry::new("paint", "").unwrap();
        assert_eq!(row_text(&same), "paint");
        let named = Entry::new("paint", "Canvas").unwrap();
        assert_eq!(row_text(&named), "Canvas  (paint)");
    }
}
