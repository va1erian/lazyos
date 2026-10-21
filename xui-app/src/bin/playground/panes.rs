//! How the main window is divided, and what the user changed
//! (`docs/messenger-playground-plan.md`, "As built: layout").
//!
//! Two dividers: the Explorer's width and the share of the right column the
//! doc pane takes; and one switch, the bottom row (editor and Output) shown or
//! hidden. The Explorer keeps its width when the window grows or shrinks and
//! the right column takes the difference; inside the right column the doc pane
//! and the bottom row keep their proportion. The choice is remembered per user
//! in `$HOME/.apps/os.lazy.playground/layout`, three `key=value` lines. The
//! file is optional: a missing, unreadable or odd one gives the defaults, and a
//! failed write is ignored (the layout then lasts until the window closes).

use std::io;
use std::path::{Path, PathBuf};

/// The Explorer's width bounds and default, in design pixels.
pub const EXPLORER_MIN: f32 = 240.0;
pub const EXPLORER_MAX: f32 = 640.0;
pub const EXPLORER_DEFAULT: f32 = 340.0;
/// The narrowest the right column may get, in design pixels.
pub const RIGHT_MIN: f32 = 420.0;
/// The doc pane's and the bottom row's least height, in design pixels.
pub const DOC_MIN: f32 = 150.0;
pub const BOTTOM_MIN: f32 = 130.0;
/// The doc pane's share of the right column's height (without the divider).
pub const DOC_SHARE_MIN: f32 = 0.2;
pub const DOC_SHARE_MAX: f32 = 0.85;
pub const DOC_SHARE_DEFAULT: f32 = 0.55;
/// The divider's thickness in design pixels (`xui-core`'s `Split`).
pub const DIVIDER: f32 = 5.0;
/// Pulling the divider of a hidden bottom row up by this much shows it again.
pub const REVEAL_PULL: f32 = 24.0;

/// The file's name under `$HOME/.apps/<system_name>/`.
const FILE: &str = "layout";
/// The most a layout file is read for.
const MAX_BYTES: u64 = 4096;

/// What is remembered.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Saved {
    /// The Explorer's width in design pixels.
    pub explorer: f32,
    /// The doc pane's share of the right column.
    pub doc_share: f32,
    /// Whether the editor and the Output show.
    pub bottom_visible: bool,
}

impl Default for Saved {
    fn default() -> Saved {
        Saved {
            explorer: EXPLORER_DEFAULT,
            doc_share: DOC_SHARE_DEFAULT,
            bottom_visible: true,
        }
    }
}

fn finite_or(value: f32, fallback: f32) -> f32 {
    if value.is_finite() {
        value
    } else {
        fallback
    }
}

impl Saved {
    /// The same values with every one inside its bounds.
    pub fn clamped(self) -> Saved {
        Saved {
            explorer: finite_or(self.explorer, EXPLORER_DEFAULT).clamp(EXPLORER_MIN, EXPLORER_MAX),
            doc_share: finite_or(self.doc_share, DOC_SHARE_DEFAULT)
                .clamp(DOC_SHARE_MIN, DOC_SHARE_MAX),
            bottom_visible: self.bottom_visible,
        }
    }

    /// The values a file's text holds: what it does not say, or says in a
    /// way that is not a number, stays the default.
    pub fn parse(text: &str) -> Saved {
        let mut saved = Saved::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            match key.trim() {
                "explorer" => {
                    if let Ok(v) = value.parse() {
                        saved.explorer = v;
                    }
                }
                "doc" => {
                    if let Ok(v) = value.parse() {
                        saved.doc_share = v;
                    }
                }
                "bottom" => saved.bottom_visible = value != "0",
                _ => {}
            }
        }
        saved.clamped()
    }

    /// The file's text.
    pub fn render(&self) -> String {
        format!(
            "explorer={:.0}\ndoc={:.3}\nbottom={}\n",
            self.explorer,
            self.doc_share,
            u8::from(self.bottom_visible)
        )
    }
}

/// The file of the tool called `system_name` under `home`.
pub fn path(home: &Path, system_name: &str) -> PathBuf {
    PathBuf::from(fhs::app_data_dir(&home.to_string_lossy(), system_name)).join(FILE)
}

/// What `path` holds, or the defaults.
pub fn load(path: &Path) -> Saved {
    let small = std::fs::metadata(path).is_ok_and(|m| m.len() <= MAX_BYTES);
    if !small {
        return Saved::default();
    }
    std::fs::read_to_string(path).map_or_else(|_| Saved::default(), |text| Saved::parse(&text))
}

/// Write `saved` to `path`, making its folder.
pub fn store(path: &Path, saved: &Saved) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, saved.render())
}

/// The doc pane's height in pixels for a right column `total` tall (the
/// divider included), at `share`, with both panes at least their minimum
/// (`scale` is device pixels per design pixel). A column too short for both
/// minimums splits at the share, as `Split` does.
pub fn doc_height(total: i32, share: f32, scale: f32) -> i32 {
    let divider = (DIVIDER * scale).round() as i32;
    let available = (total - divider).max(0);
    let wanted = (available as f32 * share).round() as i32;
    let (min_doc, min_bottom) = (
        (DOC_MIN * scale).round() as i32,
        (BOTTOM_MIN * scale).round() as i32,
    );
    if min_doc + min_bottom > available {
        return wanted.clamp(0, available);
    }
    wanted.clamp(min_doc, available - min_bottom)
}

/// The share a doc pane `doc` pixels tall stands for in a column `total` tall.
pub fn share_of(doc: i32, total: i32, scale: f32) -> f32 {
    let divider = (DIVIDER * scale).round() as i32;
    let available = (total - divider).max(1);
    (doc as f32 / available as f32).clamp(DOC_SHARE_MIN, DOC_SHARE_MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_round_trips_and_a_bad_one_gives_the_defaults() {
        let saved = Saved {
            explorer: 300.0,
            doc_share: 0.4,
            bottom_visible: false,
        };
        assert_eq!(Saved::parse(&saved.render()), saved);
        assert_eq!(Saved::parse(""), Saved::default());
        assert_eq!(
            Saved::parse("garbage\n=\nexplorer=wide\n"),
            Saved::default()
        );
    }

    #[test]
    fn values_are_clamped_and_not_a_number_is_the_default() {
        let saved = Saved::parse("explorer=5\ndoc=0.99\n");
        assert_eq!(saved.explorer, EXPLORER_MIN);
        assert_eq!(saved.doc_share, DOC_SHARE_MAX);
        let saved = Saved::parse("explorer=1e9\ndoc=-3\nbottom=0\n");
        assert_eq!(saved.explorer, EXPLORER_MAX);
        assert_eq!(saved.doc_share, DOC_SHARE_MIN);
        assert!(!saved.bottom_visible);
        assert_eq!(
            Saved::parse("explorer=NaN\ndoc=inf\n").explorer,
            EXPLORER_DEFAULT
        );
        assert_eq!(Saved::parse("doc=NaN\n").doc_share, DOC_SHARE_DEFAULT);
    }

    #[test]
    fn the_file_lives_under_the_tools_folder_and_survives_a_missing_home() {
        let dir = std::env::temp_dir().join(format!("pg-panes-{}", std::process::id()));
        let file = path(&dir, "os.lazy.playground");
        assert!(file.ends_with("layout"));
        assert_eq!(load(&file), Saved::default(), "no file: the defaults");
        let saved = Saved {
            explorer: 250.0,
            doc_share: 0.3,
            bottom_visible: false,
        };
        store(&file, &saved).expect("writes");
        assert_eq!(load(&file), saved);
        std::fs::write(&file, vec![b'x'; 10_000]).expect("writes");
        assert_eq!(
            load(&file),
            Saved::default(),
            "an oversized file is ignored"
        );
        // A folder that cannot be made (its parent is a file) is an error the caller ignores.
        assert!(store(&file.join("below").join("layout"), &saved).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_doc_pane_keeps_its_share_and_both_panes_their_minimum() {
        let h = |total, share| doc_height(total, share, 1.0);
        // 600 tall, 5 divider: 595 split at 0.55.
        assert_eq!(h(600, 0.55), 327);
        // A tiny share still leaves the doc pane its minimum, and a huge one the bottom row's.
        assert_eq!(h(600, 0.2), DOC_MIN as i32);
        assert_eq!(h(600, 0.85), 595 - BOTTOM_MIN as i32);
        // Too short for both minimums: the plain share.
        assert_eq!(h(200, 0.5), 98);
        assert_eq!(h(0, 0.5), 0);
        // At 2x everything doubles.
        assert_eq!(doc_height(1200, 0.55, 2.0), 655);
    }

    #[test]
    fn dragging_gives_back_the_share_it_came_from() {
        for total in [400, 600, 900, 1400] {
            for share in [0.25, 0.4, 0.55, 0.8] {
                let doc = doc_height(total, share, 1.0);
                let back = share_of(doc, total, 1.0);
                let again = doc_height(total, back, 1.0);
                assert!(
                    (again - doc).abs() <= 1,
                    "{total} {share}: {doc} vs {again}"
                );
            }
        }
        assert_eq!(share_of(0, 600, 1.0), DOC_SHARE_MIN);
        assert_eq!(share_of(10_000, 600, 1.0), DOC_SHARE_MAX);
    }
}
