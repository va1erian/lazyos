//! The section list: the sidebar's `IconView` model.

use xui_core::widget::IconModel;
use xui_core::IconRef;
use xui_core::Lucide;

/// One page of the Settings window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Appearance,
    Windows,
    Keyboard,
    Menu,
    Hidden,
    Time,
    About,
}

impl Section {
    /// Sidebar order. Session scripts (`xui_settings*.json`) click rows by
    /// position: a new section moves the rows below it, so re-record their
    /// clicks. Hidden apps sits next to Menu (issue #509 §5), which moved
    /// Time & Date and About down one row.
    pub const ALL: [Section; 7] = [
        Section::Appearance,
        Section::Windows,
        Section::Keyboard,
        Section::Menu,
        Section::Hidden,
        Section::Time,
        Section::About,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Section::Appearance => "Appearance",
            Section::Windows => "Windows",
            Section::Time => "Time & Date",
            Section::Keyboard => "Keyboard",
            Section::Menu => "Menu",
            Section::Hidden => "Hidden apps",
            Section::About => "About",
        }
    }

    pub const fn icon(self) -> Lucide {
        match self {
            Section::Appearance => Lucide::Monitor,
            Section::Windows => Lucide::AppWindow,
            Section::Time => Lucide::History,
            Section::Keyboard => Lucide::TextCursorInput,
            Section::Menu => Lucide::List,
            Section::Hidden => Lucide::EyeOff,
            Section::About => Lucide::Info,
        }
    }

    /// The section's position in [`Section::ALL`].
    pub const fn index(self) -> usize {
        self as usize
    }

    pub fn from_index(index: usize) -> Option<Section> {
        Section::ALL.get(index).copied()
    }
}

/// The `IconView` model over [`Section::ALL`].
pub struct SectionsModel;

impl IconModel for SectionsModel {
    fn items(&self) -> usize {
        Section::ALL.len()
    }

    fn icon(&self, item: usize) -> Option<IconRef> {
        Some(IconRef::Lucide(Section::from_index(item)?.icon()))
    }

    fn line(&self, item: usize, line: usize) -> Option<&str> {
        match line {
            0 => Section::from_index(item).map(Section::label),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_lists_every_section_in_order() {
        let model = SectionsModel;
        assert_eq!(model.items(), Section::ALL.len());
        for (i, section) in Section::ALL.iter().enumerate() {
            assert_eq!(model.line(i, 0), Some(section.label()));
            assert!(model.icon(i).is_some());
            assert_eq!(model.line(i, 1), None);
        }
        assert_eq!(model.line(Section::ALL.len(), 0), None);
    }

    #[test]
    fn from_index_round_trips_and_rejects_out_of_range() {
        assert_eq!(Section::from_index(0), Some(Section::Appearance));
        assert_eq!(Section::from_index(99), None);
        for (i, section) in Section::ALL.iter().enumerate() {
            assert_eq!(section.index(), i);
            assert_eq!(Section::from_index(i), Some(*section));
        }
    }
}
