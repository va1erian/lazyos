#![forbid(unsafe_code)]

//! The formatting row's controls and how they follow the selection.

use std::cell::RefCell;
use std::rc::Rc;

use xui_core::Dip;
use xui_core::arrange::{
    Build, Entry, Handle, Layout, LayoutExt, button, combo_box, row, spacer, toggle_button,
};
use xui_core::layout::Insets;
use xui_core::widget::{Button, ComboBox, Lucide, Placeable, ToggleButton, Tooltip};
use xui_rich_text::model::{Align, BlockKind, ListKind, Side, StyleSummary, Tri, Wrap};

use crate::app::{Mark, Msg};
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
#[derive(Default)]
pub struct Tools {
    pub block: Handle<ComboBox<Msg>>,
    pub family: Handle<ComboBox<Msg>>,
    pub size: Handle<ComboBox<Msg>>,
    /// Bold, italic, underline, strike-through.
    pub marks: [Handle<ToggleButton<Msg>>; 4],
    /// Left, centre, right, justify.
    pub aligns: [Handle<ToggleButton<Msg>>; 4],
    /// Bullets, numbers.
    pub lists: [Handle<ToggleButton<Msg>>; 2],
    pub wrap: Handle<ComboBox<Msg>>,
    /// Page view (on) or draft view (off).
    pub page_view: Handle<ToggleButton<Msg>>,
    /// Opens the Page setup menu under itself.
    pub page_setup: Handle<Button<Msg>>,
    /// Opens the Table menu under itself.
    pub table: Handle<Button<Msg>>,
    /// The icon-only buttons' names, shown on hover.
    pub tips: Tips,
}

impl Tools {
    /// The formatting row, bound to these controls.
    pub fn row(&self) -> Layout<Msg> {
        let tips = &self.tips;
        let size_labels: Vec<String> = SIZES.iter().map(|s| format!("{s}")).collect();
        let size_refs: Vec<&str> = size_labels.iter().map(String::as_str).collect();
        let [bold, italic, underline, strike] = &self.marks;
        let [left, centre, right, justify] = &self.aligns;
        let [bullets, numbers] = &self.lists;
        let page_view = toggle_button("")
            .bind(&self.page_view)
            .then(|button| button.icon(Lucide::BookOpen))
            .on_toggle(Msg::PageView)
            .checked(true);
        row()
            .gap(4)
            .padding(Insets::symmetric(Dip(8.0), Dip(3.0)))
            .children((
                combo_box(&BLOCKS)
                    .bind(&self.block)
                    .on_select(Msg::Block)
                    .then(|block| {
                        for (index, icon) in BLOCK_ICONS.into_iter().enumerate() {
                            block.set_item_icon(index, Some(icon.into()));
                        }
                        block
                    })
                    .width(120),
                combo_box(&FAMILIES)
                    .bind(&self.family)
                    .on_select(Msg::Family)
                    .width(96),
                combo_box(&size_refs)
                    .bind(&self.size)
                    .on_select(Msg::Size)
                    .then(|size| {
                        size.select(DEFAULT_SIZE);
                        size
                    })
                    .width(64),
                spacer().width(GAP),
                toggle(tips, bold, (Lucide::Bold, "Bold (Ctrl+B)"), || {
                    Msg::Toggle(Mark::Bold)
                }),
                toggle(tips, italic, (Lucide::Italic, "Italic (Ctrl+I)"), || {
                    Msg::Toggle(Mark::Italic)
                }),
                toggle(
                    tips,
                    underline,
                    (Lucide::Underline, "Underline (Ctrl+U)"),
                    || Msg::Toggle(Mark::Underline),
                ),
                toggle(
                    tips,
                    strike,
                    (Lucide::Strikethrough, "Strikethrough"),
                    || Msg::Toggle(Mark::Strike),
                ),
                spacer().width(GAP),
            ))
            .children((
                toggle(tips, left, (Lucide::TextAlignStart, "Align left"), || {
                    Msg::Align(Align::Left)
                }),
                toggle(tips, centre, (Lucide::TextAlignCenter, "Centre"), || {
                    Msg::Align(Align::Center)
                }),
                toggle(tips, right, (Lucide::TextAlignEnd, "Align right"), || {
                    Msg::Align(Align::Right)
                }),
                toggle(tips, justify, (Lucide::TextAlignJustify, "Justify"), || {
                    Msg::Align(Align::Justify)
                }),
                spacer().width(GAP),
                toggle(tips, bullets, (Lucide::List, "Bulleted list"), || {
                    Msg::List(ListKind::Bullet)
                }),
                toggle(
                    tips,
                    numbers,
                    (Lucide::ListOrdered, "Numbered list"),
                    || Msg::List(ListKind::Numbered),
                ),
                push(tips, button(""), (Lucide::IndentIncrease, "Indent"), || {
                    Msg::Indent
                }),
                push(
                    tips,
                    button(""),
                    (Lucide::IndentDecrease, "Outdent"),
                    || Msg::Outdent,
                ),
                spacer().width(GAP),
            ))
            .children((
                combo_box(&WRAPS)
                    .bind(&self.wrap)
                    .on_select(Msg::Wrap)
                    .then(|wrap| {
                        wrap.set_enabled(false);
                        wrap
                    })
                    .width(120),
                spacer().width(GAP),
                push(
                    tips,
                    button("").bind(&self.table),
                    (Lucide::Table, "Table"),
                    || Msg::Table,
                ),
                spacer(),
                tipped(page_view, tips, "Page view").width(ICON_WIDTH),
                push(
                    tips,
                    button("").bind(&self.page_setup),
                    (Lucide::Ruler, "Page setup"),
                    || Msg::PageSetup,
                ),
            ))
    }

    /// Shows `s` in the controls; a mixed attribute shows as off.
    pub fn sync(&self, s: &StyleSummary, host: &Host) {
        let on = |tri: &Tri<bool>| matches!(tri, Tri::Uniform(true));
        for (button, tri) in self
            .marks
            .iter()
            .zip([&s.bold, &s.italic, &s.underline, &s.strike])
        {
            button.get().set_checked(on(tri));
        }
        for (button, align) in self.aligns.iter().zip(ALIGNS) {
            button.get().set_checked(s.align == Tri::Uniform(align));
        }
        for (button, kind) in self
            .lists
            .iter()
            .zip([ListKind::Bullet, ListKind::Numbered])
        {
            button
                .get()
                .set_checked(matches!(s.list, Tri::Uniform(Some(item)) if item.kind == kind));
        }
        if let Tri::Uniform(kind) = s.kind {
            self.block.get().select(match kind {
                BlockKind::Body => 0,
                BlockKind::Heading(n) => usize::from(n.clamp(1, 3)),
                BlockKind::Quote => 4,
            });
        }
        if let Tri::Uniform(family) = &s.family {
            self.family
                .get()
                .select(family_index(family.as_deref(), host));
        }
        if let Tri::Uniform(size) = s.size
            && let Some(index) = SIZES.iter().position(|&v| v == size.0)
        {
            self.size.get().select(index);
        }
    }

    /// Enables the wrap picker while an image is selected and shows its wrap.
    pub fn sync_wrap(&self, wrap: Option<Wrap>) {
        let picker = self.wrap.get();
        picker.set_enabled(wrap.is_some());
        if let Some(wrap) = wrap {
            picker.select(match wrap {
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

/// The tooltips naming the icon-only buttons, attached as each is created.
pub type Tips = Rc<RefCell<Vec<Tooltip<Msg>>>>;

/// The width of an icon-only button in the formatting row.
const ICON_WIDTH: Dip = Dip(32.0);
/// The gap between groups of controls in the formatting row.
const GAP: Dip = Dip(6.0);

/// Names `widget` with `tip` on hover once it is created.
fn tipped<W: Placeable<Msg>>(
    widget: Build<W, Msg>,
    tips: &Tips,
    tip: &'static str,
) -> Build<W, Msg> {
    let tips = Rc::clone(tips);
    widget.then_with(move |widget, ui| {
        tips.borrow_mut()
            .push(Tooltip::attach(ui, widget.id(), tip)?);
        Ok(widget)
    })
}

/// An icon-only toggle bound to `handle` that raises `msg` and is named `tip`
/// on hover.
fn toggle(
    tips: &Tips,
    handle: &Handle<ToggleButton<Msg>>,
    (icon, tip): (Lucide, &'static str),
    msg: fn() -> Msg,
) -> Entry<Msg> {
    let button = toggle_button("")
        .bind(handle)
        .then(move |button| button.icon(icon))
        .on_toggle(move |_| msg());
    tipped(button, tips, tip).width(ICON_WIDTH)
}

/// `button` as an icon-only push button that raises `msg` and is named `tip`
/// on hover.
fn push(
    tips: &Tips,
    button: Build<Button<Msg>, Msg>,
    (icon, tip): (Lucide, &'static str),
    msg: fn() -> Msg,
) -> Entry<Msg> {
    let button = button
        .then(move |button| button.icon(icon))
        .on_click_with(move || Some(msg()));
    tipped(button, tips, tip).width(ICON_WIDTH)
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
