#![forbid(unsafe_code)]

//! The formatting row: its controls, their layout and how they follow the
//! selection.

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
/// The width of an icon-only button in the formatting row.
const ICON_WIDTH: Dip = Dip(32.0);
/// The space between groups of controls in the formatting row.
const GAP: Dip = Dip(6.0);

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
    /// Opens the Table menu under itself.
    pub table: Rc<Button<Msg>>,
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

/// The tooltips naming the icon-only buttons, attached as each is created.
type Tips = Rc<RefCell<Vec<Tooltip<Msg>>>>;

/// The formatting controls before the layout is mounted: the formatting row
/// binds each to its handle, and [`tools`](Self::tools) collects them after.
#[derive(Default)]
pub struct ToolHandles {
    block: Handle<ComboBox<Msg>>,
    family: Handle<ComboBox<Msg>>,
    size: Handle<ComboBox<Msg>>,
    marks: [Handle<ToggleButton<Msg>>; 4],
    aligns: [Handle<ToggleButton<Msg>>; 4],
    lists: [Handle<ToggleButton<Msg>>; 2],
    wrap: Handle<ComboBox<Msg>>,
    page_view: Handle<ToggleButton<Msg>>,
    page_setup: Handle<Button<Msg>>,
    table: Handle<Button<Msg>>,
    tips: Tips,
}

impl ToolHandles {
    /// The formatting row, bound to these handles.
    pub fn row(&self) -> Layout<Msg> {
        let t = &self.tips;
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
        let page_setup = button("")
            .bind(&self.page_setup)
            .then(|button| button.icon(Lucide::Ruler))
            .on_click_with(|| Some(Msg::PageSetup));
        let table = button("")
            .bind(&self.table)
            .then(|button| button.icon(Lucide::Table))
            .on_click_with(|| Some(Msg::Table));
        let entries = vec![
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
            toggle(t, bold, (Lucide::Bold, "Bold (Ctrl+B)"), || {
                Msg::Toggle(Mark::Bold)
            }),
            toggle(t, italic, (Lucide::Italic, "Italic (Ctrl+I)"), || {
                Msg::Toggle(Mark::Italic)
            }),
            toggle(
                t,
                underline,
                (Lucide::Underline, "Underline (Ctrl+U)"),
                || Msg::Toggle(Mark::Underline),
            ),
            toggle(t, strike, (Lucide::Strikethrough, "Strikethrough"), || {
                Msg::Toggle(Mark::Strike)
            }),
            spacer().width(GAP),
            toggle(t, left, (Lucide::TextAlignStart, "Align left"), || {
                Msg::Align(Align::Left)
            }),
            toggle(t, centre, (Lucide::TextAlignCenter, "Centre"), || {
                Msg::Align(Align::Center)
            }),
            toggle(t, right, (Lucide::TextAlignEnd, "Align right"), || {
                Msg::Align(Align::Right)
            }),
            toggle(t, justify, (Lucide::TextAlignJustify, "Justify"), || {
                Msg::Align(Align::Justify)
            }),
            spacer().width(GAP),
            toggle(t, bullets, (Lucide::List, "Bulleted list"), || {
                Msg::List(ListKind::Bullet)
            }),
            toggle(t, numbers, (Lucide::ListOrdered, "Numbered list"), || {
                Msg::List(ListKind::Numbered)
            }),
            push(t, (Lucide::IndentIncrease, "Indent"), || Msg::Indent),
            push(t, (Lucide::IndentDecrease, "Outdent"), || Msg::Outdent),
            spacer().width(GAP),
            combo_box(&WRAPS)
                .bind(&self.wrap)
                .on_select(Msg::Wrap)
                .then(|wrap| {
                    wrap.set_enabled(false);
                    wrap
                })
                .width(120),
            spacer().width(GAP),
            tipped(table, t, "Table").width(ICON_WIDTH),
            spacer(),
            tipped(page_view, t, "Page view").width(ICON_WIDTH),
            tipped(page_setup, t, "Page setup").width(ICON_WIDTH),
        ];
        row()
            .gap(4)
            .padding(Insets::symmetric(Dip(8.0), Dip(3.0)))
            .children(entries)
    }

    /// The mounted controls.
    ///
    /// # Panics
    ///
    /// When the row has not been mounted.
    pub fn tools(self) -> Tools {
        Tools {
            block: self.block.get(),
            family: self.family.get(),
            size: self.size.get(),
            marks: self.marks.each_ref().map(Handle::get),
            aligns: self.aligns.each_ref().map(Handle::get),
            lists: self.lists.each_ref().map(Handle::get),
            wrap: self.wrap.get(),
            page_view: self.page_view.get(),
            page_setup: self.page_setup.get(),
            table: self.table.get(),
            tips: self.tips.take(),
        }
    }
}

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

/// An icon-only push button that raises `msg` and is named `tip` on hover.
fn push(tips: &Tips, (icon, tip): (Lucide, &'static str), msg: fn() -> Msg) -> Entry<Msg> {
    let button = button("")
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
