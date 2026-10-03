//! The desktop picture (`sys/ui/wallpaper`, drawn by LazyShell): the rows the
//! Appearance page lists and the writes behind them.
//!
//! Row 0 is "None" (the plain background colour); the others are the picture
//! files the system ships, as [`System::wallpapers`](crate::System) lists them.

use crate::store::{ConfigStore, StoreError, Value};

/// The first row: no picture.
pub const NONE: &str = "None";

/// The picture path in effect, or `None` for the plain background.
pub fn current(store: &dyn ConfigStore) -> Option<String> {
    uitheme::wallpaper_path(store.get(uitheme::KEY_WALLPAPER).as_ref()).map(str::to_owned)
}

/// Show the picture at `path`, or (`None`) the plain background again.
pub fn set(store: &dyn ConfigStore, path: Option<&str>) -> Result<(), StoreError> {
    match path {
        Some(path) => store.set(uitheme::KEY_WALLPAPER, Value::Str(path.to_owned())),
        None => store.delete(uitheme::KEY_WALLPAPER),
    }
}

/// A picture's name in the list: its file name without the extension, with
/// `-` and `_` as spaces (`/x/LazyOS-Night.jpg` is "LazyOS Night").
pub fn label(path: &str) -> String {
    let file = path.rsplit('/').next().unwrap_or(path);
    let stem = file.rsplit_once('.').map_or(file, |(stem, _)| stem);
    stem.replace(['-', '_'], " ")
}

/// The list's rows for `pictures`: [`NONE`], then each picture's [`label`].
pub fn rows(pictures: &[String]) -> Vec<String> {
    std::iter::once(NONE.to_owned())
        .chain(pictures.iter().map(|path| label(path)))
        .collect()
}

/// The row showing `current`: 0 without a picture, and no row at all for a
/// picture that is not one of `pictures` (set by hand, say with `confctl`).
pub fn row_of(pictures: &[String], current: Option<&str>) -> Option<usize> {
    match current {
        None => Some(0),
        Some(path) => pictures.iter().position(|p| p == path).map(|i| i + 1),
    }
}

/// Apply the choice of `row`; the status-line text on success.
pub fn choose(
    store: &dyn ConfigStore,
    pictures: &[String],
    row: usize,
) -> Result<&'static str, StoreError> {
    match row.checked_sub(1).and_then(|i| pictures.get(i)) {
        Some(path) => set(store, Some(path)).map(|()| "Desktop picture changed."),
        None => set(store, None).map(|()| "Desktop picture removed."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemStore;

    fn pictures() -> Vec<String> {
        vec![
            "/system/share/wallpapers/Aurora.jpg".to_owned(),
            "/system/share/wallpapers/LazyOS-Night.jpg".to_owned(),
        ]
    }

    #[test]
    fn labels_drop_the_directory_and_extension() {
        assert_eq!(
            label("/system/share/wallpapers/LazyOS-Night.jpg"),
            "LazyOS Night"
        );
        assert_eq!(label("my_photo.final.png"), "my photo.final");
        assert_eq!(label("plain"), "plain");
        assert_eq!(rows(&pictures()), ["None", "Aurora", "LazyOS Night"]);
    }

    #[test]
    fn choosing_a_row_sets_or_clears_the_key() {
        let store = MemStore::new();
        let pictures = pictures();
        assert_eq!(row_of(&pictures, current(&store).as_deref()), Some(0));
        assert_eq!(choose(&store, &pictures, 2), Ok("Desktop picture changed."));
        assert_eq!(current(&store).as_deref(), Some(pictures[1].as_str()));
        assert_eq!(row_of(&pictures, current(&store).as_deref()), Some(2));
        assert_eq!(choose(&store, &pictures, 0), Ok("Desktop picture removed."));
        assert!(store.is_empty());
        // A row past the list is "None", never a panic.
        assert_eq!(choose(&store, &pictures, 9), Ok("Desktop picture removed."));
    }

    #[test]
    fn a_hand_set_picture_matches_no_row_and_bad_values_are_none() {
        let store = MemStore::new();
        set(&store, Some("/home/alice/cat.png")).unwrap();
        assert_eq!(row_of(&pictures(), current(&store).as_deref()), None);
        store.set(uitheme::KEY_WALLPAPER, Value::U64(3)).unwrap();
        assert_eq!(current(&store), None);
    }

    #[test]
    fn a_write_failure_surfaces() {
        let store = MemStore::new();
        *store.fail_writes.borrow_mut() = Some("denied".into());
        assert!(choose(&store, &pictures(), 1).is_err());
    }
}
