//! Small builders for the common items, so an app writes
//! `item(lucide("volume-2"), "Volume 40%")` instead of filling every
//! wire field.

use alloc::string::String;
use alloc::vec::Vec;

use crate::wire;

pub use wire::Item;

/// An icon naming a Lucide outline (`docs/icons.md`), tinted by the shell.
pub fn lucide(name: &str) -> wire::Icon {
    wire::Icon {
        lucide: Some(String::from(name)),
        ..wire::Icon::default()
    }
}

/// A full-colour icon from straight RGBA8 images (1x, and optionally 2x).
pub fn pixels(images: Vec<(u32, u32, Vec<u8>)>) -> wire::Icon {
    wire::Icon {
        pixels: images
            .into_iter()
            .map(|(width, height, data)| wire::Image {
                width,
                height,
                data,
            })
            .collect(),
        ..wire::Icon::default()
    }
}

/// An enabled top-level menu row of `kind` (`wire::MENU_KIND_*`).
pub fn menu_row(id: u32, label: &str, kind: u32) -> wire::MenuItem {
    wire::MenuItem {
        id,
        parent: 0,
        label: String::from(label),
        kind,
        enabled: true,
        checked: false,
        is_default: false,
    }
}

/// An active item with `icon` and `tooltip`, no menu, sending `Activate` on
/// a click.
pub fn item(icon: wire::Icon, tooltip: &str) -> Item {
    Item {
        icon,
        tooltip: String::from(tooltip),
        status: wire::STATUS_ACTIVE,
        badge: None,
        menu: Vec::new(),
        activate: wire::ACTIVATION_EVENT,
    }
}
