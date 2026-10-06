//! One tray item as the shell keeps it, and the validation that turns a
//! decoded `os.lazy.shell.tray.v1` request into it.
//!
//! Everything here comes from an untrusted app, so every field is checked
//! before it is kept (docs/tray-plan.md section 8): lengths, image sizes
//! against their data, menu ids and parents, and enum values. A broken
//! *icon* is not an error: it is dropped and the item falls back to the
//! package icon (section 6.2), so an item never lacks a picture. Anything
//! else that is out of bounds refuses the whole call with [`Invalid`].

use messenger_generated::os_lazy_shell_tray_v1 as wire;

/// Longest tooltip, in characters.
pub const TOOLTIP_MAX: usize = 256;
/// Longest badge, in characters.
pub const BADGE_MAX: usize = 3;
/// Most menu rows one item may carry (the shell's Quit row not counted).
pub const MENU_MAX: usize = 64;
/// Longest menu row label, in characters.
pub const LABEL_MAX: usize = 128;
/// Largest image side, in pixels.
pub const IMAGE_MAX: u32 = 64;
/// Most `pixels` images (one per scale, 1x and 2x).
pub const PIXELS_MAX: usize = 2;
/// Longest `file` icon name, in bytes.
pub const PACKAGE_NAME_MAX: usize = 64;
/// Longest Lucide name accepted before it is looked up.
pub const LUCIDE_NAME_MAX: usize = 64;

/// What a field broke, for the `EINVAL` reply and the `SHELL:TRAY:DENY` line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Invalid {
    Tooltip,
    Badge,
    Status,
    Activation,
    MenuSize,
    MenuId,
    MenuParent,
    MenuKind,
    MenuLabel,
    MenuDefault,
}

impl Invalid {
    /// A short word for logs and error texts.
    pub fn as_str(self) -> &'static str {
        match self {
            Invalid::Tooltip => "tooltip",
            Invalid::Badge => "badge",
            Invalid::Status => "status",
            Invalid::Activation => "activate",
            Invalid::MenuSize => "menu-size",
            Invalid::MenuId => "menu-id",
            Invalid::MenuParent => "menu-parent",
            Invalid::MenuKind => "menu-kind",
            Invalid::MenuLabel => "menu-label",
            Invalid::MenuDefault => "menu-default",
        }
    }
}

/// An item's state (`Status` on the wire).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Status {
    #[default]
    Active,
    /// Goes to the overflow before any active item.
    Passive,
    /// Drawn with a pulse.
    Attention,
}

/// What a primary click does (`Activation` on the wire).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Activation {
    #[default]
    Event,
    Menu,
    DefaultItem,
}

/// Straight RGBA8 pixels whose size matches their data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

/// A validated icon source. The Lucide name is only known to be well
/// formed; whether the named-icon library knows it is the fallback chain's
/// question ([`super::icon`]), since that library lives with the painter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Lucide(String),
    Mask(Image),
    Pixels(Vec<Image>),
    /// A file name under the app's own install directory's `icons/`.
    Package(String),
}

/// A menu row's kind (`MenuKind` on the wire).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuKind {
    Normal,
    Check,
    Radio,
    Separator,
    Submenu,
}

/// One validated menu row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuRow {
    pub id: u32,
    /// 0 for a top-level row, else the id of a top-level `Submenu` row.
    pub parent: u32,
    pub label: String,
    pub kind: MenuKind,
    pub enabled: bool,
    pub checked: bool,
    pub default: bool,
}

/// An app's custom item.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Item {
    /// `None` when the app gave none or it was unusable: the fallback.
    pub icon: Option<Source>,
    pub tooltip: String,
    pub status: Status,
    pub badge: Option<String>,
    pub menu: Vec<MenuRow>,
    pub activate: Activation,
}

/// The parts of an item an `Update` replaces.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Patch {
    /// `Some(None)`: the new icon was unusable, fall back.
    pub icon: Option<Option<Source>>,
    pub tooltip: Option<String>,
    pub status: Option<Status>,
    /// `Some(None)` removes the badge.
    pub badge: Option<Option<String>>,
    pub menu: Option<Vec<MenuRow>>,
}

impl Item {
    /// Validate a `Set`'s item.
    pub fn from_wire(item: wire::Item) -> Result<Item, Invalid> {
        let menu = menu(item.menu)?;
        let activate = activation(item.activate)?;
        check_default(&menu, activate)?;
        Ok(Item {
            icon: icon(item.icon),
            tooltip: text(&item.tooltip, TOOLTIP_MAX).ok_or(Invalid::Tooltip)?,
            status: status(item.status)?,
            badge: badge(item.badge.as_deref())?,
            menu,
            activate,
        })
    }

    /// Apply a validated `Update`; `Err` leaves the item unchanged (a patch
    /// that would leave `DefaultItem` without its row is refused).
    pub fn apply(&mut self, patch: Patch) -> Result<(), Invalid> {
        if let Some(menu) = &patch.menu {
            check_default(menu, self.activate)?;
        }
        if let Some(icon) = patch.icon {
            self.icon = icon;
        }
        if let Some(tooltip) = patch.tooltip {
            self.tooltip = tooltip;
        }
        if let Some(status) = patch.status {
            self.status = status;
        }
        if let Some(badge) = patch.badge {
            self.badge = badge;
        }
        if let Some(menu) = patch.menu {
            self.menu = menu;
        }
        Ok(())
    }
}

impl Patch {
    /// Validate an `Update`'s fields.
    pub fn from_wire(update: wire::UpdateArgs) -> Result<Patch, Invalid> {
        let tooltip = match update.tooltip {
            Some(tooltip) => Some(text(&tooltip, TOOLTIP_MAX).ok_or(Invalid::Tooltip)?),
            None => None,
        };
        let status = update.status.map(status).transpose()?;
        let badge = match update.badge {
            Some(badge) => Some(badge_text(&badge)?),
            None => None,
        };
        let menu = update.menu.map(|menu| self::menu(menu.rows)).transpose()?;
        Ok(Patch {
            icon: update.icon.map(icon),
            tooltip,
            status,
            badge,
            menu,
        })
    }
}

/// `value` with control characters as spaces, when it has at most `max`
/// characters.
fn text(value: &str, max: usize) -> Option<String> {
    if value.chars().count() > max {
        return None;
    }
    Some(
        value
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect(),
    )
}

fn status(value: u32) -> Result<Status, Invalid> {
    match value {
        wire::STATUS_ACTIVE => Ok(Status::Active),
        wire::STATUS_PASSIVE => Ok(Status::Passive),
        wire::STATUS_ATTENTION => Ok(Status::Attention),
        _ => Err(Invalid::Status),
    }
}

fn activation(value: u32) -> Result<Activation, Invalid> {
    match value {
        wire::ACTIVATION_EVENT => Ok(Activation::Event),
        wire::ACTIVATION_MENU => Ok(Activation::Menu),
        wire::ACTIVATION_DEFAULT_ITEM => Ok(Activation::DefaultItem),
        _ => Err(Invalid::Activation),
    }
}

fn badge(value: Option<&str>) -> Result<Option<String>, Invalid> {
    value.map_or(Ok(None), badge_text)
}

/// A badge: trimmed, at most [`BADGE_MAX`] characters; empty is none.
fn badge_text(value: &str) -> Result<Option<String>, Invalid> {
    let kept = text(value, BADGE_MAX).ok_or(Invalid::Badge)?;
    let kept = kept.trim();
    Ok((!kept.is_empty()).then(|| kept.to_owned()))
}

/// The icon's single usable source, or `None` (fall back) for no source,
/// more than one, or one that fails its checks.
fn icon(icon: wire::Icon) -> Option<Source> {
    let wire::Icon {
        lucide,
        mask,
        pixels,
        file,
    } = icon;
    let given = usize::from(lucide.is_some())
        + usize::from(mask.is_some())
        + usize::from(!pixels.is_empty())
        + usize::from(file.is_some());
    if given != 1 {
        return None;
    }
    if let Some(name) = lucide {
        return lucide_name(&name).then_some(Source::Lucide(name));
    }
    if let Some(mask) = mask {
        return image(mask).map(Source::Mask);
    }
    if let Some(name) = file {
        return package_name(&name).then_some(Source::Package(name));
    }
    if pixels.len() > PIXELS_MAX {
        return None;
    }
    let images: Option<Vec<Image>> = pixels.into_iter().map(image).collect();
    images.map(Source::Pixels)
}

/// A kebab-case name of sane length (`lazyicons` decides if it is known).
fn lucide_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= LUCIDE_NAME_MAX
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// A plain `.png` file name: no directory part, so it resolves only inside
/// the app's own `icons/` (section 8, spoofing).
pub fn package_name(name: &str) -> bool {
    name.len() <= PACKAGE_NAME_MAX
        && name.ends_with(".png")
        && name.len() > ".png".len()
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// An image within [`IMAGE_MAX`] whose data is exactly `width * height * 4`.
fn image(image: wire::Image) -> Option<Image> {
    let side_ok = |side: u32| (1..=IMAGE_MAX).contains(&side);
    if !side_ok(image.width) || !side_ok(image.height) {
        return None;
    }
    let len = (image.width as usize) * (image.height as usize) * 4;
    (image.data.len() == len).then_some(Image {
        width: image.width,
        height: image.height,
        data: image.data,
    })
}

fn menu_kind(value: u32) -> Result<MenuKind, Invalid> {
    match value {
        wire::MENU_KIND_NORMAL => Ok(MenuKind::Normal),
        wire::MENU_KIND_CHECK => Ok(MenuKind::Check),
        wire::MENU_KIND_RADIO => Ok(MenuKind::Radio),
        wire::MENU_KIND_SEPARATOR => Ok(MenuKind::Separator),
        wire::MENU_KIND_SUBMENU => Ok(MenuKind::Submenu),
        _ => Err(Invalid::MenuKind),
    }
}

/// The menu rows: at most [`MENU_MAX`], unique non-zero ids, each `parent`
/// 0 or an earlier top-level `Submenu` (so the depth is at most 2), and at
/// most one `default` row, which must be a row one can pick.
fn menu(rows: Vec<wire::MenuItem>) -> Result<Vec<MenuRow>, Invalid> {
    if rows.len() > MENU_MAX {
        return Err(Invalid::MenuSize);
    }
    let mut out: Vec<MenuRow> = Vec::with_capacity(rows.len());
    for row in rows {
        if row.id == 0 || out.iter().any(|seen| seen.id == row.id) {
            return Err(Invalid::MenuId);
        }
        let kind = menu_kind(row.kind)?;
        if row.parent != 0 {
            let submenu = out
                .iter()
                .any(|seen| seen.id == row.parent && seen.kind == MenuKind::Submenu);
            if !submenu || kind == MenuKind::Submenu {
                return Err(Invalid::MenuParent);
            }
        }
        let pickable = !matches!(kind, MenuKind::Separator | MenuKind::Submenu);
        if row.is_default && (!pickable || out.iter().any(|seen| seen.default)) {
            return Err(Invalid::MenuDefault);
        }
        out.push(MenuRow {
            id: row.id,
            parent: row.parent,
            label: text(&row.label, LABEL_MAX).ok_or(Invalid::MenuLabel)?,
            kind,
            enabled: row.enabled,
            checked: row.checked,
            default: row.is_default,
        });
    }
    Ok(out)
}

/// `DefaultItem` needs the row it runs.
fn check_default(menu: &[MenuRow], activate: Activation) -> Result<(), Invalid> {
    if activate == Activation::DefaultItem && !menu.iter().any(|row| row.default) {
        Err(Invalid::MenuDefault)
    } else {
        Ok(())
    }
}
