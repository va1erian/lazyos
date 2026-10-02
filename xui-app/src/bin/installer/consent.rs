//! The consent screen: what a package declares, grouped by risk, with the
//! friendly explanations `pkgd` supplied.
//!
//! Every package field is untrusted, so it is elided and stripped of control
//! characters before it reaches a widget. The permissions, MIME types and
//! problems each live in a [`ListView`], so any number of them scrolls rather
//! than overflowing; when there are problems, only `Close` is offered.

use xui_core::app::Ui;
use xui_core::widget::{Button, Label, ListView, Panel};
use xui_core::HasText;

use xui_app::installer::{elide, group_by_risk, permission_line, short_digest, Model, Package};

use crate::msg::Msg;
use crate::view::{fail, rect, MARGIN};

/// The consent screen's widgets.
pub struct ConsentScreen {
    _panel: Panel<Msg>,
    _name: Label<Msg>,
    _author: Label<Msg>,
    _meta: Label<Msg>,
    _description: Label<Msg>,
    _mime_label: Label<Msg>,
    _mime: ListView<Msg>,
    _perms_label: Label<Msg>,
    _perms: ListView<Msg>,
    _problems_label: Label<Msg>,
    _problems: ListView<Msg>,
    _install: Button<Msg>,
    _cancel: Button<Msg>,
    _banner: Label<Msg>,
}

impl ConsentScreen {
    /// Builds the screen at `width` x `height` from `model`.
    pub fn build(
        ui: &Ui<Msg>,
        width: i32,
        height: i32,
        model: &Model,
    ) -> Result<ConsentScreen, String> {
        let fallback = Package::default();
        let package = model.inspected.as_ref().unwrap_or(&fallback);
        let has_problems = !package.problems.is_empty();

        let panel = Panel::new(ui, rect(0, 0, width, height)).map_err(fail)?;
        let page = panel.ui();

        let name = display_name(package);
        let name_label =
            Label::new(page, rect(MARGIN, 10, width - 2 * MARGIN, 24), &name).map_err(fail)?;
        let author = format!("Author (unverified): {}", or_unknown(&package.author, 60));
        let author_label =
            Label::new(page, rect(MARGIN, 36, width - 2 * MARGIN, 16), &author).map_err(fail)?;
        let meta = format!(
            "Version {}   ·   installs to {}/{}   ·   sha256 {}",
            or_unknown(&package.version, 20),
            fhs::state::APPS_ROOT,
            elide(&package.install_dir, 60),
            short_digest(&package.digest),
        );
        let meta_label =
            Label::new(page, rect(MARGIN, 54, width - 2 * MARGIN, 16), &meta).map_err(fail)?;
        let description_label = Label::new(
            page,
            rect(MARGIN, 74, width - 2 * MARGIN, 16),
            &elide(&package.description, 180),
        )
        .map_err(fail)?;

        let mime_label = Label::new(
            page,
            rect(MARGIN, 100, width - 2 * MARGIN, 16),
            "Handled file types",
        )
        .map_err(fail)?;
        let mime_strings = mime_items(package);
        let mime_refs: Vec<&str> = mime_strings.iter().map(String::as_str).collect();
        let mime = ListView::new(page, rect(MARGIN, 118, width - 2 * MARGIN, 64), &mime_refs)
            .map_err(fail)?;
        mime.select(None);

        let perms_label = Label::new(
            page,
            rect(MARGIN, 190, width - 2 * MARGIN, 16),
            "Permissions requested",
        )
        .map_err(fail)?;
        let permission_strings = permission_items(package);
        let permission_refs: Vec<&str> = permission_strings.iter().map(String::as_str).collect();
        let perms_h = (height - 208 - 72).max(40);
        let perms = ListView::new(
            page,
            rect(MARGIN, 208, width - 2 * MARGIN, perms_h),
            &permission_refs,
        )
        .map_err(fail)?;
        perms.select(None);

        let problems_label = Label::new(
            page,
            rect(MARGIN, 100, width - 2 * MARGIN, 16),
            "This package cannot be installed",
        )
        .map_err(fail)?;
        let problem_strings = problem_items(package);
        let problem_refs: Vec<&str> = problem_strings.iter().map(String::as_str).collect();
        let problems_h = (height - 118 - 72).max(40);
        let problems = ListView::new(
            page,
            rect(MARGIN, 118, width - 2 * MARGIN, problems_h),
            &problem_refs,
        )
        .map_err(fail)?;
        problems.select(None);

        let install = Button::new(
            page,
            rect(width - MARGIN - 228, height - 48, 100, 30),
            "Install",
        )
        .map_err(fail)?
        .on_click(|| Some(Msg::Install));
        let cancel = Button::new(
            page,
            rect(width - MARGIN - 116, height - 48, 100, 30),
            "Cancel",
        )
        .map_err(fail)?
        .on_click(|| Some(Msg::Cancel));
        if has_problems {
            cancel.set_text("Close");
        }

        let banner_text = model.banner.as_deref().unwrap_or("");
        let banner = Label::new(
            page,
            rect(MARGIN, height - 72, width - 2 * MARGIN, 18),
            &elide(banner_text, 160),
        )
        .map_err(fail)?;

        ui.set_visible(mime_label.id(), !has_problems);
        ui.set_visible(mime.id(), !has_problems);
        ui.set_visible(perms_label.id(), !has_problems);
        ui.set_visible(perms.id(), !has_problems);
        ui.set_visible(install.id(), !has_problems);
        ui.set_visible(problems_label.id(), has_problems);
        ui.set_visible(problems.id(), has_problems);

        Ok(ConsentScreen {
            _panel: panel,
            _name: name_label,
            _author: author_label,
            _meta: meta_label,
            _description: description_label,
            _mime_label: mime_label,
            _mime: mime,
            _perms_label: perms_label,
            _perms: perms,
            _problems_label: problems_label,
            _problems: problems,
            _install: install,
            _cancel: cancel,
            _banner: banner,
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
