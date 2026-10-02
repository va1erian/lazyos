//! Pure operations behind the Hidden apps page (issue #509 §5): which apps
//! the start menu leaves out, for the user running Settings.
//!
//! Each app has two `bool` keys ([`deskmenu::hidden`]): the user's
//! `user/<uid>/menu/hidden/<id>` and the machine default
//! `sys/menu/hidden/<id>`, which only uid 0 writes. The page only ever writes
//! the user's key, so a user can show an app the machine hides by storing
//! `false`. Every edit is saved before the caller's copy changes, so the page
//! never shows a state the store refused.

use deskmenu::hidden;

use crate::store::{AppChoice, ConfigStore, StoreError};

/// One app on the page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HiddenRow {
    pub app: AppChoice,
    /// The user's own choice, when stored.
    pub user: Option<bool>,
    /// The machine default, when stored.
    pub machine: Option<bool>,
}

impl HiddenRow {
    /// Whether the start menu leaves this app out for the user.
    pub fn hidden(&self) -> bool {
        hidden::is_hidden(&self.app.id, self.user, self.machine)
    }

    /// The row's text: the app's name, and the machine default when it hides
    /// the app (so a user who shows it again knows why it was hidden).
    pub fn text(&self) -> String {
        if self.machine == Some(true) {
            format!("{}  (hidden by default)", self.app.name)
        } else {
            self.app.name.clone()
        }
    }
}

/// The page's state: whose choices they are and one row per app.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HiddenList {
    pub uid: u32,
    pub rows: Vec<HiddenRow>,
}

/// Read both layers for every desktop app with a well-formed id, in registry
/// order (console programs never appear in the start menu, so there is
/// nothing to hide). Fails when the store cannot say who the user is: the
/// user's keys cannot be named then.
pub fn load(store: &dyn ConfigStore) -> Result<HiddenList, StoreError> {
    let uid = store
        .uid()
        .ok_or_else(|| String::from("this session's user is unknown"))?;
    let rows = store
        .apps()
        .into_iter()
        .filter(|app| app.desktop)
        .filter_map(|app| {
            let user_key = hidden::user_key(uid, &app.id)?;
            let sys_key = hidden::sys_key(&app.id)?;
            Some(HiddenRow {
                user: hidden::flag(store.get(&user_key).as_ref()),
                machine: hidden::flag(store.get(&sys_key).as_ref()),
                app,
            })
        })
        .collect();
    Ok(HiddenList { uid, rows })
}

/// Hide (`true`) or show row `index` for the user. Writes the user's key
/// even when it matches the machine default, so the choice survives a later
/// change of that default.
pub fn set(
    store: &dyn ConfigStore,
    list: &mut HiddenList,
    index: usize,
    hide: bool,
) -> Result<(), StoreError> {
    let row = list.rows.get(index).ok_or("no app selected")?;
    let key = hidden::user_key(list.uid, &row.app.id).ok_or("not an app id")?;
    store.set(&key, confd::Value::Bool(hide))?;
    list.rows[index].user = Some(hide);
    Ok(())
}

/// Delete every one of the user's `menu/hidden` keys, so the machine
/// defaults apply again. Keys for apps no longer in the registry go too.
/// Returns how many keys were deleted.
pub fn reset(store: &dyn ConfigStore, list: &mut HiddenList) -> Result<usize, StoreError> {
    let prefix = hidden::user_prefix(list.uid);
    let mut keys: Vec<String> = store
        .list(&prefix)
        .into_iter()
        .filter(|key| hidden::app_of(key, &prefix).is_some())
        .collect();
    // A store that cannot list still knows the keys this page loaded.
    for row in list.rows.iter().filter(|row| row.user.is_some()) {
        if let Some(key) = hidden::user_key(list.uid, &row.app.id) {
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
    }
    for key in &keys {
        store.delete(key)?;
    }
    for row in &mut list.rows {
        row.user = None;
    }
    Ok(keys.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemStore;
    use confd::Value;

    const UID: u32 = 1000;

    fn store_with(apps: &[&str]) -> MemStore {
        let store = MemStore::new();
        *store.uid.borrow_mut() = Some(UID);
        *store.apps.borrow_mut() = apps
            .iter()
            .map(|id| AppChoice {
                id: (*id).into(),
                name: id.rsplit('.').next().unwrap().to_uppercase(),
                desktop: *id != "top",
            })
            .collect();
        store
    }

    fn user_key(app: &str) -> String {
        hidden::user_key(UID, app).unwrap()
    }

    #[test]
    fn an_unknown_user_cannot_load() {
        let store = store_with(&["os.lazy.paint"]);
        *store.uid.borrow_mut() = None;
        assert!(load(&store).is_err());
    }

    #[test]
    fn load_reads_both_layers_and_skips_malformed_ids() {
        let store = store_with(&[
            "os.lazy.paint",
            "os.lazy.files",
            "Bad Id",
            "top",
            "terminal",
        ]);
        store
            .set("sys/menu/hidden/os.lazy.paint", Value::Bool(true))
            .unwrap();
        store
            .set(&user_key("os.lazy.files"), Value::Bool(true))
            .unwrap();
        // Another user's choice and a non-bool value do not count.
        store
            .set("user/7/menu/hidden/terminal", Value::Bool(true))
            .unwrap();
        store
            .set(&user_key("terminal"), Value::Str("yes".into()))
            .unwrap();
        let list = load(&store).unwrap();
        let ids: Vec<&str> = list.rows.iter().map(|r| r.app.id.as_str()).collect();
        assert_eq!(ids, ["os.lazy.paint", "os.lazy.files", "terminal"]);
        let hidden: Vec<bool> = list.rows.iter().map(HiddenRow::hidden).collect();
        assert_eq!(hidden, [true, true, false]);
        assert_eq!(list.rows[0].text(), "PAINT  (hidden by default)");
        assert_eq!(list.rows[1].text(), "FILES");
    }

    #[test]
    fn a_user_can_show_an_app_the_machine_hides() {
        let store = store_with(&["os.lazy.paint"]);
        store
            .set("sys/menu/hidden/os.lazy.paint", Value::Bool(true))
            .unwrap();
        let mut list = load(&store).unwrap();
        set(&store, &mut list, 0, false).unwrap();
        assert!(!list.rows[0].hidden());
        assert_eq!(
            store.get(&user_key("os.lazy.paint")),
            Some(Value::Bool(false))
        );
        // The machine default is untouched, and a reload agrees.
        assert_eq!(
            store.get("sys/menu/hidden/os.lazy.paint"),
            Some(Value::Bool(true))
        );
        assert_eq!(load(&store).unwrap(), list);
    }

    #[test]
    fn hiding_writes_only_the_users_key() {
        let store = store_with(&["os.lazy.editor"]);
        let mut list = load(&store).unwrap();
        set(&store, &mut list, 0, true).unwrap();
        assert!(list.rows[0].hidden());
        assert_eq!(store.list("sys"), Vec::<String>::new());
        assert_eq!(store.len(), 1);
        assert!(set(&store, &mut list, 9, true).is_err());
    }

    #[test]
    fn reset_deletes_the_users_keys_only() {
        let store = store_with(&["os.lazy.paint", "os.lazy.files"]);
        store
            .set("sys/menu/hidden/os.lazy.paint", Value::Bool(true))
            .unwrap();
        store
            .set("user/7/menu/hidden/os.lazy.files", Value::Bool(true))
            .unwrap();
        store.set(&user_key("gone.app"), Value::Bool(true)).unwrap();
        store
            .set("user/1000/menu/other", Value::Bool(true))
            .unwrap();
        let mut list = load(&store).unwrap();
        set(&store, &mut list, 0, false).unwrap();
        set(&store, &mut list, 1, true).unwrap();
        assert_eq!(reset(&store, &mut list), Ok(3));
        assert!(store.list(&hidden::user_prefix(UID)).is_empty());
        assert!(store.get("user/1000/menu/other").is_some());
        assert!(store.get("user/7/menu/hidden/os.lazy.files").is_some());
        // The machine default applies again.
        assert!(list.rows[0].hidden());
        assert!(!list.rows[1].hidden());
        assert_eq!(load(&store).unwrap(), list);
    }

    #[test]
    fn store_failure_leaves_state_unchanged() {
        let store = store_with(&["os.lazy.paint"]);
        let mut list = load(&store).unwrap();
        set(&store, &mut list, 0, true).unwrap();
        let before = list.clone();
        *store.fail_writes.borrow_mut() = Some("denied".into());
        assert_eq!(set(&store, &mut list, 0, false), Err("denied".into()));
        assert_eq!(reset(&store, &mut list), Err("denied".into()));
        assert_eq!(list, before);
    }
}
