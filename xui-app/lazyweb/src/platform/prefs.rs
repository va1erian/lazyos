//! The person's browser choices from `confd` (`crate::prefs`): `user/<uid>/lazyweb/search/template`
//! and `user/<uid>/lazyweb/session/restore`.

use super::*;
use xui_app::context::user_key;

/// Reads the choices, once, before the window opens. `confd` answers a call only when it is
/// registered, so a browser that starts first (an app at login) runs with the defaults rather than
/// waiting for it.
pub(super) fn load() -> Prefs {
    let store = ConfdStore::new();
    if !store.registered() {
        return Prefs::default();
    }
    let Some(uid) = store.uid() else {
        return Prefs::default();
    };
    let text = |name: &str| match store.get(&user_key(uid, "lazyweb", name)) {
        Some(StoreValue::Str(text)) => Some(text),
        _ => None,
    };
    Prefs::from_texts(
        text("search/template").as_deref(),
        text("session/restore").as_deref(),
    )
}
