//! The person's browser choices from `confd` (`crate::prefs`): `user/<uid>/lazyweb/search/template`
//! and `user/<uid>/lazyweb/session/restore`.

use super::*;

/// Reads the choices, once, before the window opens. `confd` answers a call only when it is
/// registered, so a browser that starts first (an app at login) runs with the defaults rather than
/// waiting for it.
pub(super) fn load() -> Prefs {
    if Service::try_connect(blocker::CONFD).is_none() {
        return Prefs::default();
    }
    let store = ConfdStore::new();
    let Some(uid) = store.uid() else {
        return Prefs::default();
    };
    let text = |name: &str| match store.get(&format!("user/{uid}/lazyweb/{name}")) {
        Some(StoreValue::Str(text)) => Some(text),
        _ => None,
    };
    Prefs::from_texts(
        text("search/template").as_deref(),
        text("session/restore").as_deref(),
    )
}
