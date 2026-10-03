use alloc::format;
use alloc::string::String;

use crate::state::{APPS_ROOT, APP_DATA_DIR, HOME_ROOT};

/// The path of `binary` inside the install directory `install_dir`
/// (`<system_name>/<version>-<digest>`) under `state::APPS_ROOT`.
///
/// Composes only; the caller has validated both parts (`pkgstore::layout`).
pub fn install_path(install_dir: &str, binary: &str) -> String {
    format!("{APPS_ROOT}/{install_dir}/{binary}")
}

/// The 32-pixel app icon every package ships, relative to its install
/// directory (`lazypkg`'s required icons; the desktop launchers draw it).
pub const APP_ICON: &str = "icons/app-32.png";

/// The path of the installed app's [`APP_ICON`] in `install_dir`.
pub fn icon_path(install_dir: &str) -> String {
    install_path(install_dir, APP_ICON)
}

/// The home directory of the account `name`, `/home/<name>`.
///
/// Composes only: the caller has validated `name` as one path component (an
/// account name from `/system/etc/passwd`). The authoritative home of an
/// account is the one its passwd row names; this is the convention the build
/// and the tools follow when they create it.
pub fn home_of(name: &str) -> String {
    format!("{HOME_ROOT}/{name}")
}

/// The data directory of the app `system_name` inside `home`,
/// `<home>/.apps/<system_name>`. A trailing `/` on `home` is ignored.
///
/// Composes only: the caller has validated `system_name`
/// (`lazypkg`'s reverse-DNS rule).
pub fn app_data_dir(home: &str, system_name: &str) -> String {
    format!(
        "{}/{APP_DATA_DIR}/{system_name}",
        home.trim_end_matches('/')
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_under_the_apps_root() {
        assert_eq!(
            install_path("org.lazy.paint/1.0.0-deadbeef", "paint"),
            "/apps/org.lazy.paint/1.0.0-deadbeef/paint"
        );
    }

    #[test]
    fn homes_and_app_data_compose() {
        assert_eq!(home_of("user"), "/home/user");
        assert_eq!(
            app_data_dir("/home/user", "org.lazy.counter"),
            "/home/user/.apps/org.lazy.counter"
        );
        assert_eq!(app_data_dir("/home/user/", "a.b"), "/home/user/.apps/a.b");
    }
}
