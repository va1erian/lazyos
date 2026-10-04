#![forbid(unsafe_code)]

//! The formatting row's controls and how they follow the selection.

use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::backend::Result;
use xui_core::widget::{Button, ComboBox, Lucide, ToggleButton, Tooltip};
use xui_rich_text::model::{Align, BlockKind, ListKind, Side, StyleSummary, Tri, Wrap};

use crate::app::Msg;
use crate::host::Host;

/// The block kinds the picker offers, in order.
pub const BLOCKS: [&str; 5] = ["Normal", "Heading 1", "Heading 2", "Heading 3", "Quote"];
/// The icon of each block kind in the picker, in the same order.
pub const BLOCK_ICONS: [Lucide; 5] = [
    Lucide::Type,
    Lucide::Heading1,
    Lucide::Heading2,
    Lucide::Heading3,
    Lucide::TextQuote,
];
/// The font families the picker offers: Sans is the default UI face.
pub const FAMILIES: [&str; 3] = ["Sans", "Serif", "Mono"];
/// The font sizes the picker offers, in design units.
pub const SIZES: [f32; 8] = [10.0, 12.0, 14.0, 16.0, 18.0, 24.0, 32.0, 48.0];
/// The size a new document's text has (the editor's default).
pub const DEFAULT_SIZE: usize = 2;
/// The image wraps the picker offers, in order.
pub const WRAPS: [&str; 4] = ["Inline", "Float left", "Float right", "Top and bottom"];
/// The alignments, in the order of their toggles.
pub const ALIGNS: [Align; 4] = [Align::Left, Align::Center, Align::Right, Align::Justify];

/// The formatting controls the app keeps in step with the selection.
pub struct Tools {
    pub block: Rc<ComboBox<Msg>>,
    pub family: Rc<ComboBox<Msg>>,
    pub size: Rc<ComboBox<Msg>>,
    /// Bold, italic, underline, strike-through.
    pub marks: [Rc<ToggleButton<Msg>>; 4],
    /// Left, centre, right, justify.
    pub aligns: [Rc<ToggleButton<Msg>>; 4],
    /// Bullets, numbers.
    pub lists: [Rc<ToggleButton<Msg>>; 2],
    pub wrap: Rc<ComboBox<Msg>>,
    /// Page view (on) or draft view (off).
    pub page_view: Rc<ToggleButton<Msg>>,
    /// Opens the Page setup menu under itself.
    pub page_setup: Rc<Button<Msg>>,
    /// The icon-only buttons' names, shown on hover.
    pub tips: Vec<Tooltip<Msg>>,
}

impl Tools {
    /// Shows `s` in the controls; a mixed attribute shows as off.
    pub fn sync(&self, s: &StyleSummary, host: &Host) {
        let on = |tri: &Tri<bool>| matches!(tri, Tri::Uniform(true));
        for (button, tri) in self
            .marks
            .iter()
            .zip([&s.bold, &s.italic, &s.underline, &s.strike])
        {
            button.set_checked(on(tri));
        }
        for (button, align) in self.aligns.iter().zip(ALIGNS) {
            button.set_checked(s.align == Tri::Uniform(align));
        }
        for (button, kind) in self
            .lists
            .iter()
            .zip([ListKind::Bullet, ListKind::Numbered])
        {
            button.set_checked(matches!(s.list, Tri::Uniform(Some(item)) if item.kind == kind));
        }
        if let Tri::Uniform(kind) = s.kind {
            self.block.select(match kind {
                BlockKind::Body => 0,
                BlockKind::Heading(n) => usize::from(n.clamp(1, 3)),
                BlockKind::Quote => 4,
            });
        }
        if let Tri::Uniform(family) = &s.family {
            self.family.select(family_index(family.as_deref(), host));
        }
        if let Tri::Uniform(size) = s.size
            && let Some(index) = SIZES.iter().position(|&v| v == size.0)
        {
            self.size.select(index);
        }
    }

    /// Enables the wrap picker while an image is selected and shows its wrap.
    pub fn sync_wrap(&self, wrap: Option<Wrap>) {
        self.wrap.set_enabled(wrap.is_some());
        if let Some(wrap) = wrap {
            self.wrap.select(match wrap {
                Wrap::Inline => 0,
                Wrap::Square {
                    side: Side::Left, ..
                } => 1,
                Wrap::Square { .. } => 2,
                Wrap::TopAndBottom { .. } => 3,
            });
        }
    }
}

/// The picker index of a run's family: Serif or Mono by the host's names,
/// anything else (the default face) Sans.
pub fn family_index(family: Option<&str>, host: &Host) -> usize {
    match family {
        Some(name) if name == host.serif_family => 1,
        Some(name) if name == host.mono_family => 2,
        _ => 0,
    }
}

/// An icon-only toggle that raises `msg` and is named `tip` on hover.
pub fn toggle(
    ui: &Ui<Msg>,
    tips: &mut Vec<Tooltip<Msg>>,
    (icon, tip): (Lucide, &str),
    msg: fn() -> Msg,
) -> Result<Rc<ToggleButton<Msg>>> {
    let button = ToggleButton::auto(ui, "")?
        .icon(icon)
        .on_toggle(move |_| Some(msg()));
    tips.push(Tooltip::attach(ui, button.id(), tip)?);
    Ok(Rc::new(button))
}

/// An icon-only push button that raises `msg` and is named `tip` on hover.
pub fn push(
    ui: &Ui<Msg>,
    tips: &mut Vec<Tooltip<Msg>>,
    (icon, tip): (Lucide, &str),
    msg: fn() -> Msg,
) -> Result<Button<Msg>> {
    let button = Button::auto(ui, "")?
        .icon(icon)
        .on_click(move || Some(msg()));
    tips.push(Tooltip::attach(ui, button.id(), tip)?);
    Ok(button)
}

/// A drop-down that raises `msg` with the picked index.
pub fn picker(ui: &Ui<Msg>, items: &[&str], msg: fn(usize) -> Msg) -> Result<Rc<ComboBox<Msg>>> {
    Ok(Rc::new(
        ComboBox::auto(ui, items)?.on_select(move |i| Some(msg(i))),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_family_maps_to_its_picker_entry() {
        let mut host = Host::std("/");
        host.serif_family = "Droid Serif".into();
        host.mono_family = "JetBrains Mono".into();
        assert_eq!(family_index(None, &host), 0);
        assert_eq!(family_index(Some("Droid Sans"), &host), 0);
        assert_eq!(family_index(Some("Droid Serif"), &host), 1);
        assert_eq!(family_index(Some("JetBrains Mono"), &host), 2);
        assert_eq!(FAMILIES.len(), 3);
    }
}
