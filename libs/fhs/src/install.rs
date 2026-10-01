use alloc::format;
use alloc::string::String;

use crate::state::APPS_ROOT;

/// The path of `binary` inside the install directory `install_dir`
/// (`<system_name>/<version>-<digest>`) under `state::APPS_ROOT`.
///
/// Composes only; the caller has validated both parts (`pkgstore::layout`).
pub fn install_path(install_dir: &str, binary: &str) -> String {
    format!("{APPS_ROOT}/{install_dir}/{binary}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_under_the_apps_root() {
        assert_eq!(
            install_path("org.lazy.paint/1.0.0-deadbeef", "paint"),
            "/data/apps/org.lazy.paint/1.0.0-deadbeef/paint"
        );
    }
}
