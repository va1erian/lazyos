//! Developer-tool packages in the Installer (`docs/devtool-packages-plan.md`
//! section 4): the pure half. Nothing here calls a service.
//!
//! `pkgd` marks a developer tool by the rows its inspection returns
//! (`pkgstore::explain::tool`): one `kind` row carrying the class text, one
//! `risk` row per shared risk, one `denied` row per refused interface, then
//! the manifest's own permissions. The Installer reads the class from the
//! same `kind` row `elevd` reads it from ([`is_developer_tool`]), shows the
//! rows as `pkgd` sent them ([`tool_items`]) and installs through the
//! `elevd` operation [`OPERATION`] instead of asking `pkgd` itself: `pkgd`
//! accepts a tool only from `elevd`, after an administrator approved it on
//! the trusted prompt. The decision is only about what to show and which door
//! to knock on; `pkgd` and `elevd` make it again, so a package that hides its
//! class gets a refusal, never an install.

use super::model::{Model, Package, Permission};
use super::view::{clean, elide, group_by_risk, permission_line};

/// The `kind` of the permission row that names the class
/// (`pkgstore::explain::tool::KIND_ROW`).
pub const KIND_ROW: &str = "kind";
/// The `kind` of a row naming a risk every developer tool shares.
pub const RISK_ROW: &str = "risk";
/// The `kind` of a row naming something a developer tool may not use.
pub const DENIED_ROW: &str = "denied";
/// The `elevd` operation that installs (or replaces) a developer tool
/// (`elevpolicy`'s `pkg.install-devtool`; its argument is the package path).
pub const OPERATION: &str = "pkg.install-devtool";

/// Whether `permissions` open the package as a developer tool: the `kind`
/// row naming the class, the test `elevd` applies to the same rows.
pub fn is_developer_tool(permissions: &[Permission]) -> bool {
    permissions
        .iter()
        .any(|row| row.kind == KIND_ROW && row.value == devtool::KIND)
}

impl Package {
    /// Whether this is a developer tool ([`is_developer_tool`]).
    pub fn is_developer_tool(&self) -> bool {
        is_developer_tool(&self.permissions)
    }
}

/// Whether `row` describes the class rather than one request of the manifest.
fn is_class_row(row: &Permission) -> bool {
    matches!(row.kind.as_str(), KIND_ROW | RISK_ROW | DENIED_ROW)
}

impl Model {
    /// Whether the package under consent is a developer tool.
    pub fn inspected_is_tool(&self) -> bool {
        self.inspected
            .as_ref()
            .is_some_and(Package::is_developer_tool)
    }

    /// The notes a developer tool adds to the consent: that an administrator
    /// must approve it, and which installed version it replaces (an update
    /// takes the same operation). Empty for anything else.
    pub(super) fn tool_notes(&self) -> Vec<String> {
        let Some(package) = self.inspected.as_ref().filter(|p| p.is_developer_tool()) else {
            return Vec::new();
        };
        let mut notes = vec![String::from(
            "Developer tool: an administrator must approve it",
        )];
        let replaced = self
            .packages
            .iter()
            .find(|app| !app.core && app.system_name == package.system_name);
        if let Some(app) = replaced {
            notes.push(format!(
                "Replaces version {} you have installed",
                elide(&app.version, 20)
            ));
        }
        notes
    }
}

/// The warning that leads the consent screen: the class row's own words as
/// `pkgd` sent them, or the shared text when the row has none.
pub fn warning_text(package: &Package) -> String {
    let sent = package
        .permissions
        .iter()
        .find(|row| row.kind == KIND_ROW && row.value == devtool::KIND)
        .map(|row| elide(&row.explanation, 400))
        .unwrap_or_default();
    if sent.is_empty() {
        devtool::CONSENT_TEXT.to_owned()
    } else {
        sent
    }
}

/// What installing a developer tool involves, said before the permissions.
pub fn approval_note(name: &str) -> String {
    format!(
        "Installing {} needs an administrator's name and password.",
        elide(name, 30)
    )
}

/// One row of a class section: the sentence `pkgd` supplied, falling back to
/// the usual row text, indented under its heading.
fn class_line(row: &Permission) -> String {
    let text = clean(&row.explanation);
    let text = if text.is_empty() {
        permission_line(row)
    } else {
        text
    };
    format!("    {}", elide(&text, 110))
}

/// The consent list of a developer tool: the package's own permissions first
/// (what sets it apart, and what a long list below would hide), then what
/// every tool can do and what none may use, both as `pkgd` sent them. The
/// `kind` row is not repeated here; it leads the screen as [`warning_text`].
/// Never empty.
pub fn tool_items(package: &Package) -> Vec<String> {
    let mut items = Vec::new();
    let own: Vec<Permission> = package
        .permissions
        .iter()
        .filter(|row| !is_class_row(row))
        .cloned()
        .collect();
    let groups = group_by_risk(&own);
    if !groups.is_empty() {
        items.push("This package also asks for".to_owned());
    }
    for group in groups {
        items.push(format!("  {}", group.risk.title()));
        for row in group.permissions {
            items.push(format!("    {}", elide(&permission_line(row), 110)));
        }
    }
    for (kind, heading) in [
        (RISK_ROW, "What every developer tool can do"),
        (DENIED_ROW, "What a developer tool may not use"),
    ] {
        let rows = package.permissions.iter().filter(|row| row.kind == kind);
        let lines: Vec<String> = rows.map(class_line).collect();
        if !lines.is_empty() {
            items.push(heading.to_owned());
            items.extend(lines);
        }
    }
    if items.is_empty() {
        items.push("This package requests no special permissions.".to_owned());
    }
    items
}

/// The number of `risk` and `denied` rows, for the serial evidence.
pub fn class_counts(package: &Package) -> (usize, usize) {
    let count = |kind: &str| {
        package
            .permissions
            .iter()
            .filter(|row| row.kind == kind)
            .count()
    };
    (count(RISK_ROW), count(DENIED_ROW))
}

/// The system name in `elevd`'s success detail (`Installed <system_name>
/// <version>`), when it has the expected shape.
pub fn installed_name(detail: &str) -> Option<&str> {
    let mut words = detail.strip_prefix("Installed ")?.split(' ');
    words.next().filter(|name| !name.is_empty())
}

/// A refusal of `pkg.install-devtool` (or of the call that carried it) as the
/// sentence the wizard shows. `code` is the call's error code (a negative
/// errno, as the platform client reports it) and `message` the service's own
/// words; either may be all there is.
pub fn refusal_text(code: i64, message: &str) -> String {
    use crate::sys::errno;

    let message = clean(message);
    match -code {
        errno::ECANCELED => String::from(
            "Not installed: the administrator prompt was cancelled. A developer tool needs an \
             administrator's approval; nothing was changed.",
        ),
        errno::ETIMEDOUT => String::from(
            "Not installed: nobody answered the administrator prompt in time. Try again when an \
             administrator is at the screen.",
        ),
        errno::EACCES => String::from(
            "Not installed: no administrator approved it. A developer tool needs an \
             administrator's name and password; ask an administrator to install it.",
        ),
        _ if message.contains("not a developer tool") => String::from(
            "Not installed: this package is not a developer tool, so it cannot be installed \
             this way. Go back and choose the package again.",
        ),
        _ if message.contains("may not install developer tools") => String::from(
            "Not installed: only the Installer, run by a person, can install a developer tool.",
        ),
        _ if message.is_empty() => {
            format!(
                "Not installed: the elevation service refused the request (errno {}).",
                -code
            )
        }
        _ => format!("Not installed: {message}"),
    }
}

#[cfg(test)]
mod tests;
