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
}

impl Section {
    /// Sidebar order.
    pub const ALL: [Section; 3] = [Section::Appearance, Section::Windows, Section::Keyboard];

    pub const fn label(self) -> &'static str {
        match self {
            Section::Appearance => "Appearance",
            Section::Windows => "Windows",
            Section::Keyboard => "Keyboard",
        }
    }

    pub const fn icon(self) -> Lucide {
        match self {
            Section::Appearance => Lucide::Monitor,
            Section::Windows => Lucide::AppWindow,
            Section::Keyboard => Lucide::TextCursorInput,
        }
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
    }
}
