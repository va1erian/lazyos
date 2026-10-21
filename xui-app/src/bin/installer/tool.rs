//! Installing a developer tool: the `elevd` half.
//!
//! `pkgd` accepts a developer-tool package from nobody a session owns, an
//! administrator included, so the Installer never calls `pkgd.Install` for one.
//! It names the operation `pkg.install-devtool` to `elevd` instead: the
//! compositor shows the trusted prompt (which leads with the class warning
//! `elevd` composes itself), an administrator types a name and password, and
//! `elevd` installs the exact bytes it inspected before the prompt. An update
//! of an installed tool takes the same road. The reply names what was
//! installed; the Installer then asks `pkgd` for the recorded row, so the
//! success screen shows what the system holds, not what the file claimed.
//!
//! The request blocks the window while the prompt is up, as the core-app
//! update through `elevd` always has: the prompt is modal and the progress
//! screen was painted a tick before.

use xui_app::installer::{installed_name, refusal_text, Installed, DEVTOOL_OPERATION};
use xui_app::platform::{elevd, pkg};
use xui_app::trace::{self, Channel};

/// Install the developer tool at `path` (consent shown for `expected`, its
/// system name) through `elevd`, returning the row `pkgd` recorded.
pub fn install(path: &str, expected: &str) -> Result<Installed, String> {
    trace::line!(Channel::Installer, "ELEVATE {DEVTOOL_OPERATION}");
    let (detail, _) = elevd::request(DEVTOOL_OPERATION, &[path])
        .map_err(|error| refusal_text(error.code, &error.message))?;
    // The reply's own name wins: it is what `elevd` installed.
    let name = installed_name(&detail).unwrap_or(expected);
    pkg::installed(name)?
        .ok_or_else(|| String::from("The developer tool was installed, but pkgd does not list it."))
}
