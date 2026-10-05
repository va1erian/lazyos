//! Wizard steps 2 and 3: what a package is, then the permissions it asks for,
//! grouped by risk, with the friendly explanations `pkgd` supplied.
//!
//! Every package field is untrusted, so it is elided and stripped of control
//! characters before it reaches a widget. The MIME types, permissions and
//! problems each live in a [`ListView`], so any number of them scrolls rather
//! than overflowing. A package with problems stops at Review: the problems
//! replace the file types and `Next` is disabled.

use xui_core::arrange::{build, column, label, Build, Layout, LayoutExt};
use xui_core::widget::ListView;
use xui_core::Rect;

use xui_app::installer::{elide, group_by_risk, permission_line, short_digest, Model, Package};

use crate::msg::Msg;
use crate::wizard::{page, Next};

/// A list of `items` with nothing selected.
pub fn items_list<M: 'static>(items: Vec<String>) -> Build<ListView<M>, M> {
    build(move |ui| {
        let refs: Vec<&str> = items.iter().map(String::as_str).collect();
        let list = ListView::new(ui, Rect::default(), &refs)?;
        list.select(None);
        Ok(list)
    })
}

/// Step 2: the package's identity and the file types it handles.
pub fn review(model: &Model) -> Layout<Msg> {
    let fallback = Package::default();
    let package = model.inspected.as_ref().unwrap_or(&fallback);
    let has_problems = !package.problems.is_empty();

    let meta = format!(
        "Version {}   ·   installs to {}/{}   ·   sha256 {}",
        or_unknown(&package.version, 20),
        fhs::state::APPS_ROOT,
        elide(&package.install_dir, 60),
        short_digest(&package.digest),
    );
    // "Updates built-in app <name>" / "Starts when you log in" lead the
    // description line.
    let mut about = model.consent_notes();
    let summary = elide(&package.description, 180);
    if !summary.is_empty() {
        about.push(summary);
    }
    // A broken package shows its problems where the file types would be.
    let (title, items) = if has_problems {
        ("This package cannot be installed", problem_items(package))
    } else {
        ("Handled file types", mime_items(package))
    };
    let content = column().gap(4).children((
        label(display_name(package)),
        label(format!(
            "Author (unverified): {}",
            or_unknown(&package.author, 60)
        )),
        label(meta),
        label(elide(&about.join("   ·   "), 200)),
        label(title),
        items_list(items).fill(1),
    ));
    page(
        model.screen,
        model,
        content,
        true,
        Next {
            text: "Next >",
            msg: Msg::Next,
            enabled: model.can_advance(),
            focus: !has_problems,
        },
    )
}

/// Step 3: the consent. `Install` forwards the user's yes to `pkgd`.
pub fn permissions(model: &Model) -> Layout<Msg> {
    let fallback = Package::default();
    let package = model.inspected.as_ref().unwrap_or(&fallback);
    let intro = format!(
        "{} asks for these permissions. Install only if you trust it.",
        display_name(package)
    );
    let mut content = column().gap(6).child(label(elide(&intro, 120)));
    // What installing does beyond the permissions (replace a built-in app,
    // start at log-in), only when it says something.
    let notes = model.consent_notes().join("   ·   ");
    if !notes.is_empty() {
        content = content.child(label(elide(&notes, 160)));
    }
    let content = content.child(items_list(permission_items(package)).fill(1));
    // Install is deliberately not focused: a second Enter after Review's
    // Next must not consent on the user's behalf.
    page(
        model.screen,
        model,
        content,
        true,
        Next {
            text: "Install",
            msg: Msg::Install,
            enabled: model.can_install(),
            focus: false,
        },
    )
}

/// The package's display name, or a placeholder when it sent none.
fn display_name(package: &Package) -> String {
    let name = elide(&package.name, 60);
    if name.is_empty() {
        "Unnamed package".to_owned()
    } else {
        name
    }
}

/// `text`, elided, or `unknown` when it is empty.
fn or_unknown(text: &str, max: usize) -> String {
    let text = elide(text, max);
    if text.is_empty() {
        "unknown".to_owned()
    } else {
        text
    }
}

/// One row per handled file type, or a friendly sentence when there are none.
fn mime_items(package: &Package) -> Vec<String> {
    if package.mime.is_empty() {
        return vec!["No file types are associated with this package.".to_owned()];
    }
    package
        .mime
        .iter()
        .map(|handler| {
            let mime = elide(&handler.mime_type, 60);
            let verbs: Vec<String> = handler.verbs.iter().map(|verb| elide(verb, 16)).collect();
            let mut line = if verbs.is_empty() {
                mime
            } else {
                format!("{mime}  ({})", verbs.join(", "))
            };
            if handler.has_icon {
                line.push_str("  [icon]");
            }
            elide(&line, 100)
        })
        .collect()
}

/// The permission rows: a heading per risk group (high first) then one indented
/// row per permission, or a friendly sentence when the package asks for none.
pub(crate) fn permission_items(package: &Package) -> Vec<String> {
    let groups = group_by_risk(&package.permissions);
    if groups.is_empty() {
        return vec!["This package requests no special permissions.".to_owned()];
    }
    let mut items = Vec::new();
    for group in groups {
        items.push(group.risk.title().to_owned());
        for permission in group.permissions {
            items.push(format!("    {}", elide(&permission_line(permission), 110)));
        }
    }
    items
}

/// One row per problem, elided; empty when the package has none.
fn problem_items(package: &Package) -> Vec<String> {
    package
        .problems
        .iter()
        .map(|problem| elide(problem, 140))
        .collect()
}
