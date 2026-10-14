//! The Removable Drives tray applet's model (`os.lazy.removable`,
//! docs/usb-hotplug-plan.md G): what the list of removable volumes `mountd`
//! keeps (`os.lazy.mount.v1` `Volumes`) says to the person, as a tray item.
//!
//! Pure: [`Model::poll`] takes the list, gives back the notice of what
//! changed since the last one, and the item is read off the model
//! ([`Model::tooltip`], [`Model::status`], [`Model::badge`],
//! [`Model::menu`], [`mask`]). The bin (`src/bin/removable.rs`) only fetches
//! the list, puts the item on the tray and acts on a click.
//!
//! # Which events are a notice
//!
//! * A volume that **appears mounted** while the session runs: "NAME mounted
//!   at /media/NAME". What was in when the applet started is the baseline and
//!   stays silent (a stick that was plugged in at login mounts a second
//!   later, and nobody needs telling).
//! * A volume that is **ejected**: "NAME can be removed".
//! * A volume that **goes away without having been ejected**: "NAME was
//!   removed without ejecting (data may be lost)". This one is told for the
//!   baseline's volumes too.
//! * A stick with no FAT volume, or a volume that failed: one line saying so.
//!
//! The several of one look are one notice. A notice is the tooltip and the
//! `Attention` status for [`ATTENTION_POLLS`] looks, which is all the tray
//! has (no toast, docs/usb-hotplug-plan.md decision 8); this is the one
//! place a toast would be added.

use trayclient::{menu_row, wire};

/// How many looks (one a second) a notice keeps the tray item in
/// `Attention` and its text in the tooltip.
pub const ATTENTION_POLLS: u32 = 6;
/// How many looks an eject refusal stays on the menu.
pub const REFUSAL_POLLS: u32 = 20;
/// The longest text of one menu row (characters).
pub const MAX_ROW: usize = 36;

/// Side of the tray icon mask in pixels (the shell takes up to 64).
pub const MASK_SIDE: u32 = 32;

/// Menu row ids: `OPEN + i` and `EJECT + i` for the `i`-th mounted drive.
const OPEN: u32 = 100;
const EJECT: u32 = 200;
const EJECT_ALL: u32 = 300;
const SEPARATOR: u32 = 301;
const INFO: u32 = 302;
const REFUSAL: u32 = 303;
const EJECTING: u32 = 400;

/// Where a volume is, as `mountd` words it (`VolumeInfo.state`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// A daemon is looking for a volume on it.
    Starting,
    Mounted,
    Ejecting,
    /// Flushed, closed and unmounted: safe to pull.
    Ejected,
    /// The stick holds no volume the system can mount.
    Unsupported,
    /// Pulled while it was mounted.
    Removed,
    Failed,
}

impl Phase {
    pub fn of(state: &str) -> Phase {
        match state {
            "mounted" => Phase::Mounted,
            "ejecting" => Phase::Ejecting,
            "ejected" => Phase::Ejected,
            "unsupported" => Phase::Unsupported,
            "removed" => Phase::Removed,
            "starting" => Phase::Starting,
            _ => Phase::Failed,
        }
    }
}

/// One removable volume.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Drive {
    /// `usb<n>`: what `Eject` names.
    pub disk: String,
    /// The mount's name (the label), empty until mounted.
    pub name: String,
    /// `/media/<name>`, empty until mounted.
    pub path: String,
    pub phase: Phase,
    pub detail: String,
}

impl Drive {
    /// What to call it: the label, else the disk.
    pub fn title(&self) -> &str {
        if self.name.is_empty() {
            &self.disk
        } else {
            &self.name
        }
    }
}

/// What a click on a menu row (or the icon) asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Open this folder in Files.
    Open(String),
    /// Eject this disk.
    Eject(String),
    EjectAll(Vec<String>),
}

/// The applet's state.
#[derive(Debug, Default)]
pub struct Model {
    drives: Vec<Drive>,
    /// Disks that were there at the first look: their mounting is not news.
    quiet: Vec<String>,
    seen: bool,
    notice: Option<String>,
    attention: u32,
    refusal: Option<String>,
    refusal_left: u32,
}

impl Model {
    pub fn new() -> Model {
        Model::default()
    }

    /// Take a look at the list. Returns the notice this look produced (also
    /// the tooltip while it lasts), if any.
    pub fn poll(&mut self, list: Vec<Drive>) -> Option<String> {
        self.attention = self.attention.saturating_sub(1);
        self.refusal_left = self.refusal_left.saturating_sub(1);
        if self.refusal_left == 0 {
            self.refusal = None;
        }
        if !self.seen {
            self.seen = true;
            self.quiet = list.iter().map(|d| d.disk.clone()).collect();
            self.drives = list;
            return None;
        }
        let mut told: Vec<String> = Vec::new();
        let mut ejecting = false;
        for old in &self.drives {
            if !list.iter().any(|d| d.disk == old.disk) {
                match old.phase {
                    Phase::Ejected => told.push(format!("{} removed", old.title())),
                    Phase::Mounted | Phase::Ejecting | Phase::Starting => told.push(format!(
                        "{} was removed without ejecting (data may be lost)",
                        old.title()
                    )),
                    _ => {}
                }
                self.quiet.retain(|disk| *disk != old.disk);
            }
        }
        for new in &list {
            let before = self
                .drives
                .iter()
                .find(|d| d.disk == new.disk)
                .map(|d| d.phase);
            if before == Some(new.phase) {
                continue;
            }
            let quiet = self.quiet.contains(&new.disk);
            ejecting |= new.phase == Phase::Ejecting;
            match new.phase {
                Phase::Mounted if !quiet => {
                    told.push(format!("{} mounted at {}", new.title(), new.path));
                }
                Phase::Ejected => told.push(format!("{} can be removed", new.title())),
                Phase::Unsupported if !quiet => {
                    told.push(format!("{}: no volume Lazy OS can use", new.disk));
                }
                Phase::Failed if !quiet => {
                    let why = if new.detail.is_empty() {
                        "failed"
                    } else {
                        &new.detail
                    };
                    told.push(format!("{}: {why}", new.title()));
                }
                _ => {}
            }
        }
        self.drives = list;
        if told.is_empty() {
            // An eject the person asked for: what the tooltip said before
            // (a stale "mounted") gives way to what is happening.
            if ejecting {
                self.attention = 0;
            }
            return None;
        }
        let text = told.join("; ");
        self.notice = Some(text.clone());
        self.attention = ATTENTION_POLLS;
        Some(text)
    }

    /// An eject was refused (`busy`): say who holds the volume.
    pub fn refused(&mut self, drive: &str, holders: &str) {
        let first = holders.lines().next().unwrap_or("a program");
        let more = holders.lines().count().saturating_sub(1);
        let tail = if more > 0 {
            format!(" (+{more} more)")
        } else {
            String::new()
        };
        let text = format!("Cannot eject {drive}: {first}{tail}");
        // The menu row is narrow: who, not where.
        let who = first.split(": ").next().unwrap_or(first);
        self.refusal = Some(format!("In use: {who}{tail}"));
        self.refusal_left = REFUSAL_POLLS;
        self.notice = Some(text);
        self.attention = ATTENTION_POLLS;
    }

    /// The mounted drives, in list order.
    pub fn mounted(&self) -> impl Iterator<Item = &Drive> {
        self.drives.iter().filter(|d| d.phase == Phase::Mounted)
    }

    pub fn drives(&self) -> &[Drive] {
        &self.drives
    }

    pub fn tooltip(&self) -> String {
        if self.attention > 0 {
            if let Some(notice) = &self.notice {
                return notice.clone();
            }
        }
        let mounted: Vec<&Drive> = self.mounted().collect();
        let ejecting = self.drives.iter().find(|d| d.phase == Phase::Ejecting);
        match (mounted.as_slice(), ejecting) {
            (_, Some(drive)) => format!("Ejecting {}...", drive.title()),
            ([], None) => String::from("No removable drives"),
            ([one], None) => format!("{} at {}", one.title(), one.path),
            (many, None) => format!("{} removable drives", many.len()),
        }
    }

    pub fn status(&self) -> u32 {
        if self.attention > 0 {
            wire::STATUS_ATTENTION
        } else if self
            .drives
            .iter()
            .any(|d| matches!(d.phase, Phase::Mounted | Phase::Ejecting))
        {
            wire::STATUS_ACTIVE
        } else {
            wire::STATUS_PASSIVE
        }
    }

    /// The number of mounted drives, or none (at most three characters).
    pub fn badge(&self) -> String {
        match self.mounted().count() {
            0 => String::new(),
            n => n.min(99).to_string(),
        }
    }

    /// The menu: Open and Eject for each mounted drive, Eject all when
    /// there are several, what is being ejected, and the last refusal.
    pub fn menu(&self) -> Vec<wire::MenuItem> {
        let mut rows = Vec::new();
        let mounted: Vec<&Drive> = self.mounted().collect();
        for (index, drive) in mounted.iter().enumerate() {
            let index = index as u32;
            let mut open = menu_row(
                OPEN + index,
                &clip(&format!("Open {}", drive.title())),
                wire::MENU_KIND_NORMAL,
            );
            open.is_default = index == 0;
            rows.push(open);
            rows.push(menu_row(
                EJECT + index,
                &clip(&format!("Eject {}", drive.title())),
                wire::MENU_KIND_NORMAL,
            ));
        }
        if mounted.len() > 1 {
            rows.push(menu_row(SEPARATOR, "", wire::MENU_KIND_SEPARATOR));
            rows.push(menu_row(EJECT_ALL, "Eject all", wire::MENU_KIND_NORMAL));
        }
        for (index, drive) in self
            .drives
            .iter()
            .filter(|d| d.phase == Phase::Ejecting)
            .enumerate()
        {
            let mut row = menu_row(
                EJECTING + index as u32,
                &clip(&format!("Ejecting {}...", drive.title())),
                wire::MENU_KIND_NORMAL,
            );
            row.enabled = false;
            rows.push(row);
        }
        if let Some(text) = &self.refusal {
            let mut row = menu_row(REFUSAL, &clip(text), wire::MENU_KIND_NORMAL);
            row.enabled = false;
            rows.push(row);
        }
        if rows.is_empty() {
            let mut row = menu_row(INFO, "No removable drives", wire::MENU_KIND_NORMAL);
            row.enabled = false;
            rows.push(row);
        }
        rows
    }

    /// What menu row `id` asks for, against the drives as they are now.
    pub fn action(&self, id: u32) -> Option<Action> {
        let mounted: Vec<&Drive> = self.mounted().collect();
        let nth = |base: u32| mounted.get(id.checked_sub(base)? as usize).copied();
        match id {
            EJECT_ALL => {
                let disks: Vec<String> = mounted.iter().map(|d| d.disk.clone()).collect();
                (!disks.is_empty()).then_some(Action::EjectAll(disks))
            }
            OPEN..=199 => nth(OPEN).map(|d| Action::Open(d.path.clone())),
            EJECT..=299 => nth(EJECT).map(|d| Action::Eject(d.disk.clone())),
            _ => None,
        }
    }

    /// What a click on the icon does: open the first mounted drive.
    pub fn default_action(&self) -> Option<Action> {
        self.action(OPEN)
    }

    /// A cheap fingerprint of everything the tray shows, to send an update
    /// only when it changed.
    pub fn shown(&self) -> (String, u32, String, Vec<(u32, String, bool)>) {
        (
            self.tooltip(),
            self.status(),
            self.badge(),
            self.menu()
                .into_iter()
                .map(|row| (row.id, row.label, row.enabled))
                .collect(),
        )
    }
}

/// `text` cut to [`MAX_ROW`] characters.
fn clip(text: &str) -> String {
    if text.chars().count() <= MAX_ROW {
        return String::from(text);
    }
    let mut cut: String = text.chars().take(MAX_ROW - 3).collect();
    cut.push_str("...");
    cut
}

/// The tray picture: a USB stick lying at 45 degrees, plug to the upper
/// right, as an RGBA mask (only alpha is used; the shell tints it with the
/// bar's ink so it follows the theme). xui's outline set has no drive, so it
/// is drawn here. Tilted so that it does not read as a padlock: a metal
/// plug with two contacts, a wider body with a strap hole.
pub fn mask() -> Vec<u8> {
    const SIDE: usize = MASK_SIDE as usize;
    let centre = SIDE as f32 / 2.0;
    let root2 = core::f32::consts::SQRT_2;
    let mut data = vec![0u8; SIDE * SIDE * 4];
    for y in 0..SIDE {
        for x in 0..SIDE {
            let (dx, dy) = (x as f32 + 0.5 - centre, y as f32 + 0.5 - centre);
            // `t` along the stick (towards the plug), `s` across it.
            let t = (dx - dy) / root2;
            let s = (dx + dy) / root2;
            let body = (-14.0..=3.0).contains(&t) && s.abs() <= 6.5;
            let plug = (3.0..=14.0).contains(&t) && s.abs() <= 4.5;
            let contact = (8.5..=12.0).contains(&t)
                && ((-3.2..=-1.3).contains(&s) || (1.3..=3.2).contains(&s));
            let strap = (-11.5..=-8.5).contains(&t) && s.abs() <= 1.6;
            if (body || plug) && !contact && !strap {
                let at = (y * SIDE + x) * 4;
                data[at..at + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
    }
    data
}

#[cfg(test)]
mod tests;
