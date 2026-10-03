//! Wizard steps 2 and 3: what a package is, then the permissions it asks for,
//! grouped by risk, with the friendly explanations `pkgd` supplied.
//!
//! Every package field is untrusted, so it is elided and stripped of control
//! characters before it reaches a widget. The MIME types, permissions and
//! problems each live in a [`ListView`], so any number of them scrolls rather
//! than overflowing. A package with problems stops at Review: the problems
//! replace the file types and `Next` is disabled.

use xui_core::app::Ui;
use xui_core::widget::{Label, ListView, Panel};

use xui_app::installer::{elide, group_by_risk, permission_line, short_digest, Model, Package};

use crate::msg::Msg;
use crate::view::{fail, rect, MARGIN};
use crate::wizard::{banner, Header, NavBar, CONTENT_TOP, FOOTER_H};

/// Step 2: the package's identity and the file types it handles.
pub struct ReviewScreen {
    _panel: Panel<Msg>,
    _header: Header,
    _name: Label<Msg>,
    _author: Label<Msg>,
    _meta: Label<Msg>,
    _description: Label<Msg>,
    _list_label: Label<Msg>,
    _list: ListView<Msg>,
    _banner: Label<Msg>,
    _nav: NavBar,
}

impl ReviewScreen {
    /// Builds the screen at `width` x `height` from `model`.
    pub fn build(
        ui: &Ui<Msg>,
        width: i32,
        height: i32,
        model: &Model,
    ) -> Result<ReviewScreen, String> {
        let fallback = Package::default();
        let package = model.inspected.as_ref().unwrap_or(&fallback);
        let has_problems = !package.problems.is_empty();

        let panel = Panel::new(ui, rect(0, 0, width, height)).map_err(fail)?;
        let page = panel.ui();
        let header = Header::build(page, width, model.screen)?;
        let inner = width - 2 * MARGIN;
        let top = CONTENT_TOP;

        let name =
            Label::new(page, rect(MARGIN, top, inner, 24), &display_name(package)).map_err(fail)?;
        let author = format!("Author (unverified): {}", or_unknown(&package.author, 60));
        let author = Label::new(page, rect(MARGIN, top + 26, inner, 16), &author).map_err(fail)?;
        let meta = format!(
            "Version {}   ·   installs to /data/apps/{}   ·   sha256 {}",
            or_unknown(&package.version, 20),
            elide(&package.install_dir, 60),
            short_digest(&package.digest),
        );
        let meta = Label::new(page, rect(MARGIN, top + 44, inner, 16), &meta).map_err(fail)?;
        let description = Label::new(
            page,
            rect(MARGIN, top + 64, inner, 16),
            &elide(&package.description, 180),
        )
        .map_err(fail)?;

        // A broken package shows its problems where the file types would be.
        let (title, items) = if has_problems {
            ("This package cannot be installed", problem_items(package))
        } else {
            ("Handled file types", mime_items(package))
        };
        let list_label =
            Label::new(page, rect(MARGIN, top + 92, inner, 16), title).map_err(fail)?;
        let list_top = top + 110;
        let list_h = (height - FOOTER_H - list_top).max(40);
        let refs: Vec<&str> = items.iter().map(String::as_str).collect();
        let list =
            ListView::new(page, rect(MARGIN, list_top, inner, list_h), &refs).map_err(fail)?;
        list.select(None);

        let banner = banner(page, width, height, model)?;
        let nav = NavBar::build(page, width, height, true, ("Next >", Msg::Next))?;
        nav.set_next_enabled(model.can_advance());
        if !has_problems {
            nav.focus_next(page);
        }

        Ok(ReviewScreen {
            _panel: panel,
            _header: header,
            _name: name,
            _author: author,
            _meta: meta,
            _description: description,
            _list_label: list_label,
            _list: list,
            _banner: banner,
            _nav: nav,
        })
    }
}

/// Step 3: the consent. `Install` forwards the user's yes to `pkgd`.
pub struct PermissionsScreen {
    _panel: Panel<Msg>,
    _header: Header,
    _intro: Label<Msg>,
    _perms: ListView<Msg>,
    _banner: Label<Msg>,
    _nav: NavBar,
}

impl PermissionsScreen {
    /// Builds the screen at `width` x `height` from `model`.
    pub fn build(
        ui: &Ui<Msg>,
        width: i32,
        height: i32,
        model: &Model,
    ) -> Result<PermissionsScreen, String> {
        let fallback = Package::default();
        let package = model.inspected.as_ref().unwrap_or(&fallback);

        let panel = Panel::new(ui, rect(0, 0, width, height)).map_err(fail)?;
        let page = panel.ui();
        let header = Header::build(page, width, model.screen)?;
        let inner = width - 2 * MARGIN;

        let intro = format!(
            "{} asks for these permissions. Install only if you trust it.",
            display_name(package)
        );
        let intro = Label::new(
            page,
            rect(MARGIN, CONTENT_TOP, inner, 18),
            &elide(&intro, 120),
        )
        .map_err(fail)?;
        let list_top = CONTENT_TOP + 24;
        let list_h = (height - FOOTER_H - list_top).max(40);
        let items = permission_items(package);
        let refs: Vec<&str> = items.iter().map(String::as_str).collect();
        let perms =
            ListView::new(page, rect(MARGIN, list_top, inner, list_h), &refs).map_err(fail)?;
        perms.select(None);

        let banner = banner(page, width, height, model)?;
        let nav = NavBar::build(page, width, height, true, ("Install", Msg::Install))?;
        nav.set_next_enabled(model.can_install());
        // Install is deliberately not focused: a second Enter after Review's
        // Next must not consent on the user's behalf.

        Ok(PermissionsScreen {
            _panel: panel,
            _header: header,
            _intro: intro,
            _perms: perms,
            _banner: banner,
            _nav: nav,
        })
    }
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
fn permission_items(package: &Package) -> Vec<String> {
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
